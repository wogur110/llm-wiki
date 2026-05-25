/**
 * Onboarding page tests.
 *
 * Validates the start-button gating spec from CLAUDE.md > Onboarding Rules
 * (Zotero-driven flow):
 *
 *   * Gemini API key (the only step) AND
 *   * Successful "연결 테스트" (test_connection)
 *
 * must both be true before the "시작하기 →" button becomes enabled.  The PDF
 * folder step was removed — PDFs are now fetched on demand from the Zotero
 * local API, so onboarding no longer asks for a filesystem path.
 */

import React from 'react'
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('next/navigation', () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn() }),
}))

import { invoke } from '@tauri-apps/api/core'
import OnboardingPage from '../app/onboarding/page'

const mockedInvoke = vi.mocked(invoke)

function defaultInvoke() {
  mockedInvoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'get_api_key') return null
    return null
  })
}

function getStartButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /시작하기/ }) as HTMLButtonElement
}

function typeInto(el: HTMLElement, value: string) {
  fireEvent.change(el, { target: { value } })
}

describe('OnboardingPage', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    defaultInvoke()
  })

  it('Start button is disabled on initial render', async () => {
    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))
    expect(getStartButton()).toBeDisabled()
  })

  it('Start button stays disabled with only a key (no successful test)', async () => {
    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    typeInto(screen.getByPlaceholderText('AIza…'), 'AIzaTESTKEY1234567890')
    expect(getStartButton()).toBeDisabled()
  })

  it('does not ask for any filesystem path', async () => {
    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    // A folder text input would have a Zotero-storage placeholder — assert
    // the new UI does not render one.
    expect(screen.queryByPlaceholderText(/Zotero[\\/]storage/i)).toBeNull()
  })

  it('Start button enables after key + successful test_connection', async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'get_api_key') return null
      if (cmd === 'save_api_key') return null
      if (cmd === 'test_connection') return true
      return null
    })

    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    typeInto(screen.getByPlaceholderText('AIza…'), 'AIzaTESTKEY1234567890')
    fireEvent.click(screen.getByRole('button', { name: /연결 테스트/ }))

    await waitFor(() => {
      expect(mockedInvoke).toHaveBeenCalledWith('test_connection', {
        apiKey: 'AIzaTESTKEY1234567890',
      })
    })

    await waitFor(() => {
      expect(screen.getByText(/연결 성공/)).toBeInTheDocument()
      expect(getStartButton()).not.toBeDisabled()
    })
  })

  it('shows an error message when test_connection fails', async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'get_api_key') return null
      if (cmd === 'test_connection') {
        throw new Error('Gemini API: invalid key (401)')
      }
      return null
    })

    render(<OnboardingPage />)
    await waitFor(() => expect(mockedInvoke).toHaveBeenCalledWith('get_api_key'))

    typeInto(screen.getByPlaceholderText('AIza…'), 'AIzaBADKEY1234567890')
    fireEvent.click(screen.getByRole('button', { name: /연결 테스트/ }))

    await waitFor(() => {
      expect(screen.getByText(/연결 실패/)).toBeInTheDocument()
      expect(screen.getByText(/invalid key/)).toBeInTheDocument()
      expect(getStartButton()).toBeDisabled()
    })
  })
})
