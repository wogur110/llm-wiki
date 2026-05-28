import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import React from 'react'

vi.mock('@tauri-apps/plugin-shell', () => ({
  open: vi.fn(),
}))

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}))

vi.mock('next/link', () => ({
  default: ({
    children,
    href,
    ...props
  }: {
    children: React.ReactNode
    href: string
    onClick?: () => void
  }) => (
    <a href={href} {...props}>
      {children}
    </a>
  ),
}))

import { open as shellOpen } from '@tauri-apps/plugin-shell'
import { invoke } from '@tauri-apps/api/core'
import {
  PaperPreviewProvider,
  usePaperPreview,
} from '../components/PaperPreviewContext'
import { type PaperMeta } from '../lib/content'

const mockedShellOpen = vi.mocked(shellOpen)
const mockedInvoke = vi.mocked(invoke)

const samplePaper: PaperMeta = {
  slug: 'attention',
  category: 'Computer Vision/01_Generative_Models',
  created_at: '2024-01-15T12:00:00Z',
  title: 'Attention Is All You Need',
  year: 2017,
  authors: ['Ashish Vaswani', 'Noam Shazeer'],
  publication: 'NeurIPS',
  doi: '10.1234/test',
  url: 'https://arxiv.org/abs/1706.03762',
  zotero_key: 'ABCD1234',
  tags: ['nlp', 'transformers'],
  abstract: 'We rely entirely on attention mechanisms.',
  summary: 'We propose a new architecture based on attention.',
  extra: {},
}

function PreviewControls() {
  const { openPreview, closePreview } = usePaperPreview()
  return (
    <div>
      <button type="button" onClick={() => openPreview(samplePaper)}>
        open-preview
      </button>
      <button type="button" onClick={closePreview}>
        close-preview
      </button>
    </div>
  )
}

function drawerTitle() {
  return screen.getByRole('heading', { name: 'Attention Is All You Need' })
}

function renderWithProvider() {
  return render(
    <PaperPreviewProvider>
      <PreviewControls />
    </PaperPreviewProvider>,
  )
}

describe('usePaperPreview', () => {
  it('throws when used outside PaperPreviewProvider', () => {
    function Outside() {
      usePaperPreview()
      return null
    }
    expect(() => render(<Outside />)).toThrow(/PaperPreviewProvider/)
  })
})

describe('PaperPreviewProvider + drawer', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mockedShellOpen.mockResolvedValue(undefined)
  })

  it('opens the drawer with paper metadata when openPreview is called', async () => {
    renderWithProvider()

    fireEvent.click(screen.getByRole('button', { name: 'open-preview' }))

    await waitFor(() => {
      expect(drawerTitle()).toBeInTheDocument()
    })
    expect(screen.getByText('Computer Vision/01_Generative_Models')).toBeInTheDocument()
    expect(screen.getByText(/Ashish Vaswani/)).toBeInTheDocument()
    expect(screen.getByText('2017년')).toBeInTheDocument()
    expect(screen.getByText('NeurIPS')).toBeInTheDocument()
    expect(screen.getByText('10.1234/test')).toBeInTheDocument()
    expect(screen.getByText('nlp')).toBeInTheDocument()
    expect(
      screen.getByText('We propose a new architecture based on attention.'),
    ).toBeInTheDocument()
  })

  it('closes the drawer when the close button is clicked', async () => {
    renderWithProvider()
    fireEvent.click(screen.getByRole('button', { name: 'open-preview' }))
    await waitFor(() => {
      expect(drawerTitle()).toBeInTheDocument()
    })

    fireEvent.click(screen.getByLabelText('닫기'))

    await waitFor(() => {
      expect(
        screen.queryByRole('heading', { name: 'Attention Is All You Need' }),
      ).not.toBeInTheDocument()
    })
  })

  it('closes the drawer when the backdrop is clicked', async () => {
    renderWithProvider()
    fireEvent.click(screen.getByRole('button', { name: 'open-preview' }))
    await waitFor(() => {
      expect(drawerTitle()).toBeInTheDocument()
    })

    const backdrop = document.querySelector('.backdrop-blur-xs') as HTMLElement
    expect(backdrop).toBeTruthy()
    fireEvent.click(backdrop)

    await waitFor(() => {
      expect(
        screen.queryByRole('heading', { name: 'Attention Is All You Need' }),
      ).not.toBeInTheDocument()
    })
  })

  it('opens Zotero via shell when the footer button is clicked', async () => {
    renderWithProvider()
    fireEvent.click(screen.getByRole('button', { name: 'open-preview' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Zotero에서 열기' })).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: 'Zotero에서 열기' }))

    await waitFor(() => {
      expect(mockedShellOpen).toHaveBeenCalledWith('zotero://select/items/ABCD1234')
    })
  })

  it('shows an error when shell.open fails', async () => {
    mockedShellOpen.mockRejectedValueOnce(new Error('shell unavailable'))
    renderWithProvider()
    fireEvent.click(screen.getByRole('button', { name: 'open-preview' }))
    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Zotero에서 열기' })).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: 'Zotero에서 열기' }))

    await waitFor(() => {
      expect(screen.getByText(/shell unavailable/)).toBeInTheDocument()
    })
  })

  it('shows the AI summary generator when summary is missing', async () => {
    const noSummary: PaperMeta = { ...samplePaper, summary: null, zotero_key: null }
    function OpenNoSummary() {
      const { openPreview } = usePaperPreview()
      return (
        <button type="button" onClick={() => openPreview(noSummary)}>
          open-no-summary
        </button>
      )
    }
    render(
      <PaperPreviewProvider>
        <OpenNoSummary />
      </PaperPreviewProvider>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'open-no-summary' }))

    await waitFor(() => {
      expect(screen.getByText(/아직 AI 요약이 없습니다/)).toBeInTheDocument()
    })
    expect(
      screen.getByRole('button', { name: /AI 요약 생성/ }),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole('button', { name: 'Zotero에서 열기' }),
    ).not.toBeInTheDocument()
  })

  it('renders the abstract block separately from the AI summary', async () => {
    renderWithProvider()
    fireEvent.click(screen.getByRole('button', { name: 'open-preview' }))
    await waitFor(() => {
      expect(drawerTitle()).toBeInTheDocument()
    })
    expect(screen.getByText('초록')).toBeInTheDocument()
    expect(
      screen.getByText('We rely entirely on attention mechanisms.'),
    ).toBeInTheDocument()
  })

  it('generates an AI summary via Gemini and displays the result', async () => {
    const noSummary: PaperMeta = { ...samplePaper, summary: null }
    window.localStorage.setItem('content-root', '/tmp/wiki')
    mockedInvoke.mockResolvedValueOnce({
      summary: '한국어 요약입니다.',
    })

    function OpenNoSummary() {
      const { openPreview } = usePaperPreview()
      return (
        <button type="button" onClick={() => openPreview(noSummary)}>
          open-no-summary
        </button>
      )
    }
    render(
      <PaperPreviewProvider>
        <OpenNoSummary />
      </PaperPreviewProvider>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'open-no-summary' }))
    await waitFor(() => {
      expect(
        screen.getByRole('button', { name: /AI 요약 생성/ }),
      ).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: /AI 요약 생성/ }))

    await waitFor(() => {
      expect(mockedInvoke).toHaveBeenCalledWith('summarize_paper', {
        contentRoot: '/tmp/wiki',
        slug: 'attention',
      })
      expect(screen.getByText('한국어 요약입니다.')).toBeInTheDocument()
    })
  })

  it('surfaces summary generation errors', async () => {
    const noSummary: PaperMeta = { ...samplePaper, summary: null }
    window.localStorage.setItem('content-root', '/tmp/wiki')
    mockedInvoke.mockRejectedValueOnce(new Error('Gemini quota exhausted'))

    function OpenNoSummary() {
      const { openPreview } = usePaperPreview()
      return (
        <button type="button" onClick={() => openPreview(noSummary)}>
          open-no-summary
        </button>
      )
    }
    render(
      <PaperPreviewProvider>
        <OpenNoSummary />
      </PaperPreviewProvider>,
    )
    fireEvent.click(screen.getByRole('button', { name: 'open-no-summary' }))
    await waitFor(() => {
      expect(
        screen.getByRole('button', { name: /AI 요약 생성/ }),
      ).toBeInTheDocument()
    })

    fireEvent.click(screen.getByRole('button', { name: /AI 요약 생성/ }))

    await waitFor(() => {
      expect(screen.getByText(/Gemini quota exhausted/)).toBeInTheDocument()
    })
  })

})
