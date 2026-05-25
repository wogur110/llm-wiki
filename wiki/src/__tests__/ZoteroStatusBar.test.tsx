import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import React from 'react'

// ── module mocks (hoisted before imports) ────────────────────────────────────

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
vi.mock('next/navigation', () => ({
  usePathname: () => '/',  // not /onboarding → component renders
}))

// Import AFTER vi.mock so the mocked versions are used
import { invoke } from '@tauri-apps/api/core'
import ZoteroStatusBar from '../components/ZoteroStatusBar'

// ── helpers ───────────────────────────────────────────────────────────────────

const mockedInvoke = vi.mocked(invoke)

const pendingItem = (key: string) => ({
  paper_file: `${key}.md`,
  zotero_item_key: key,
  target_collection: 'large-language-models',
  queued_at: '2026-01-01T00:00:00Z',
})

// ── tests ─────────────────────────────────────────────────────────────────────

describe('ZoteroStatusBar', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    // Provide a content-root so refreshPending() can build the queue path
    localStorage.setItem('content-root', '/test/content/root')
  })

  it('shows "Zotero 연결됨" when status = Connected', async () => {
    mockedInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'check_status') return { status: 'Connected' }
      if (cmd === 'load_queue') return []
      return null
    })

    render(<ZoteroStatusBar />)

    await waitFor(() => {
      expect(screen.getByText(/Zotero 연결됨/)).toBeInTheDocument()
    })
  })

  it('shows "Zotero 꺼짐" when status = Disconnected', async () => {
    mockedInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'check_status') return { status: 'Disconnected' }
      if (cmd === 'load_queue') return []
      return null
    })

    render(<ZoteroStatusBar />)

    await waitFor(() => {
      expect(screen.getByText(/Zotero 꺼짐/)).toBeInTheDocument()
    })
  })

  it('shows pending count when N items are pending', async () => {
    mockedInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'check_status') return { status: 'Disconnected' }
      if (cmd === 'load_queue') return [pendingItem('K1'), pendingItem('K2')]
      return null
    })

    render(<ZoteroStatusBar />)

    // The label is "Zotero 꺼짐 (2개 동기화 대기)"
    await waitFor(() => {
      expect(screen.getByText(/2개 동기화 대기/)).toBeInTheDocument()
    })
  })

  describe('"지금 동기화" button visibility', () => {
    it('is visible when Connected AND pending > 0', async () => {
      mockedInvoke.mockImplementation(async (cmd) => {
        if (cmd === 'check_status') return { status: 'Connected' }
        if (cmd === 'load_queue') return [pendingItem('K1')]
        return null
      })

      render(<ZoteroStatusBar />)

      await waitFor(() => {
        expect(screen.getByText('지금 동기화')).toBeInTheDocument()
      })
    })

    it('is NOT visible when Disconnected (even with pending items)', async () => {
      mockedInvoke.mockImplementation(async (cmd) => {
        if (cmd === 'check_status') return { status: 'Disconnected' }
        if (cmd === 'load_queue') return [pendingItem('K1')]
        return null
      })

      render(<ZoteroStatusBar />)

      await waitFor(() => {
        // Status text appears first, then we can check button is absent
        expect(screen.getByText(/Zotero 꺼짐/)).toBeInTheDocument()
      })
      expect(screen.queryByText('지금 동기화')).not.toBeInTheDocument()
    })

    it('is NOT visible when Connected but pending = 0', async () => {
      mockedInvoke.mockImplementation(async (cmd) => {
        if (cmd === 'check_status') return { status: 'Connected' }
        if (cmd === 'load_queue') return []
        return null
      })

      render(<ZoteroStatusBar />)

      await waitFor(() => {
        expect(screen.getByText(/Zotero 연결됨/)).toBeInTheDocument()
      })
      expect(screen.queryByText('지금 동기화')).not.toBeInTheDocument()
    })
  })

  it('button click calls sync_all command', async () => {
    mockedInvoke.mockImplementation(async (cmd) => {
      if (cmd === 'check_status') return { status: 'Connected' }
      if (cmd === 'load_queue') return [pendingItem('K1')]
      if (cmd === 'sync_all') return { synced: 1, failed: 0, remaining: 0, errors: [] }
      return null
    })

    render(<ZoteroStatusBar />)

    // Wait for the sync button to appear
    const syncButton = await screen.findByText('지금 동기화')
    fireEvent.click(syncButton)

    await waitFor(() => {
      expect(mockedInvoke).toHaveBeenCalledWith(
        'sync_all',
        expect.objectContaining({ queuePath: expect.stringContaining('pending-zotero-sync.json') })
      )
    })
  })
})
