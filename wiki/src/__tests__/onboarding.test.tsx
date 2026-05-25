/**
 * Onboarding page tests.
 *
 * Validates the start-button gating spec from CLAUDE.md > Onboarding Rules:
 *
 *   * Folder path (Step 1) AND
 *   * Gemini API key (Step 2) AND
 *   * Successful "연결 테스트" (test_connection)
 *
 * are ALL required before the "시작하기 →" button becomes enabled.  Also
 * checks that a failing test_connection surfaces an error message.
 */

import React from 'react'
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'

// ── module mocks (hoisted before component import) ───────────────────────────

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('next/navigation', () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn() }),
}))

// Import AFTER vi.mock so the mocked invoke is in effect.
import { invoke } from '@tauri-apps/api/core'
import OnboardingPage from '../app/onboarding/page'

const mockedInvoke = vi.mocked(invoke)

// ── helpers ──────────────────────────────────────────────────────────────────

/** Default invoke handler — returns null for `get_api_key` (no pre-existing key). */
function defaultInvoke() {
  mockedInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'get_api_key') return null
    return null
  })
}

/** Convenience accessor for the gated button. */
function getStartButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /시작하기/ }) as HTMLButtonElement
}

// Type-fence around fireEvent.change for `<input>` elements.
function typeInto(el: HTMLElement, value: string) {
  fireEvent.change(el, { target: { value } })
}

// ── tests ────────────────────────────────────────────────────────────────────

describe('OnboardingPage', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    defaultInvoke()
  })

  it('Start button is disabled on initial render', async () => {
    render(<OnboardingPage />)

    // Wait for the async pre-fill effect (get_api_key) to settle so the
    // button reflects the true post-mount state.
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    expect(getStartButton()).toBeDisabled()
  })

  it('Start button stays disabled after entering folder only (no key)', async () => {
    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    const folderInput = screen.getByPlaceholderText('/home/user/llm-wiki/content')
    typeInto(folderInput, '/some/content/path')

    expect(getStartButton()).toBeDisabled()
  })

  it('Start button stays disabled after entering key only (no folder)', async () => {
    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    const keyInput = screen.getByPlaceholderText('AIza…')
    typeInto(keyInput, 'AIzaTESTKEY1234567890')

    expect(getStartButton()).toBeDisabled()
  })

  it('Start button is enabled after folder + key + successful test_connection', async () => {
    // Successful test_connection — `save_api_key` and `test_connection`
    // both resolve cleanly.
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'get_api_key') return null
      if (cmd === 'save_api_key') return null
      if (cmd === 'test_connection') return true
      return null
    })

    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    typeInto(
      screen.getByPlaceholderText('/home/user/llm-wiki/content'),
      '/some/content/path',
    )
    typeInto(screen.getByPlaceholderText('AIza…'), 'AIzaTESTKEY1234567890')

    // Trigger the test_connection flow.
    fireEvent.click(screen.getByRole('button', { name: /연결 테스트/ }))

    await waitFor(() => {
      expect(mockedInvoke).toHaveBeenCalledWith('test_connection', {
        apiKey: 'AIzaTESTKEY1234567890',
      })
    })

    // Success badge appears AND start button becomes enabled.
    await waitFor(() => {
      expect(screen.getByText(/연결 성공/)).toBeInTheDocument()
      expect(getStartButton()).not.toBeDisabled()
    })
  })

  it('shows an error message when test_connection fails', async () => {
    // test_connection rejects before save_api_key is called.
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'get_api_key') return null
      if (cmd === 'test_connection') {
        throw new Error('Gemini API: invalid key (401)')
      }
      return null
    })

    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    typeInto(
      screen.getByPlaceholderText('/home/user/llm-wiki/content'),
      '/some/content/path',
    )
    typeInto(screen.getByPlaceholderText('AIza…'), 'AIzaBADKEY1234567890')

    fireEvent.click(screen.getByRole('button', { name: /연결 테스트/ }))

    // Error badge + detail message render; start button stays disabled.
    await waitFor(() => {
      expect(screen.getByText(/연결 실패/)).toBeInTheDocument()
      expect(screen.getByText(/invalid key/)).toBeInTheDocument()
      expect(getStartButton()).toBeDisabled()
    })
  })
})
