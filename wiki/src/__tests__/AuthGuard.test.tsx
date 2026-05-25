import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import React from 'react'

const mockReplace = vi.fn()
let mockPathname = '/'

vi.mock('next/navigation', () => ({
  useRouter: () => ({ replace: mockReplace, push: vi.fn() }),
  usePathname: () => mockPathname,
}))

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

import { invoke } from '@tauri-apps/api/core'
import AuthGuard from '../components/AuthGuard'

const mockedInvoke = vi.mocked(invoke)

describe('AuthGuard', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    mockPathname = '/'
  })

  it('renders children immediately on /onboarding', async () => {
    mockPathname = '/onboarding'
    render(
      <AuthGuard>
        <span>child</span>
      </AuthGuard>,
    )
    await waitFor(() => {
      expect(screen.getByText('child')).toBeInTheDocument()
    })
    expect(mockReplace).not.toHaveBeenCalled()
  })

  it('redirects to onboarding when key or root is missing', async () => {
    mockedInvoke.mockResolvedValueOnce(false)
    render(
      <AuthGuard>
        <span>child</span>
      </AuthGuard>,
    )
    await waitFor(() => {
      expect(mockReplace).toHaveBeenCalledWith('/onboarding')
    })
    expect(screen.queryByText('child')).not.toBeInTheDocument()
  })

  it('renders children when key and pdf-root are present', async () => {
    localStorage.setItem('zotero-pdf-root', '/some/zotero/storage')

    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'has_api_key')     return true
      if (cmd === 'set_pdf_root')    return undefined
      if (cmd === 'get_content_root') return '/auto/content'
      return undefined
    })

    render(
      <AuthGuard>
        <span>child</span>
      </AuthGuard>,
    )

    await waitFor(() => {
      expect(screen.getByText('child')).toBeInTheDocument()
    })
    expect(mockedInvoke).toHaveBeenCalledWith('set_pdf_root', {
      path: '/some/zotero/storage',
    })
    expect(localStorage.getItem('content-root')).toBe('/auto/content')
  })
})
