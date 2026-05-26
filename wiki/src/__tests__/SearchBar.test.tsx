import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import React from 'react'
import SearchBar from '../components/SearchBar'

// ── module mocks (hoisted by vitest) ─────────────────────────────────────────

const mockPush = vi.fn()
vi.mock('next/navigation', () => ({
  useRouter: () => ({ push: mockPush, replace: vi.fn() }),
}))

// Mock @/lib/content so that:
//   - getContentRoot() returns a non-null path (allows the useEffect to run)
//   - listAllPapers() resolves with sample paper data
const mockListAllPapers = vi.fn()
vi.mock('@/lib/content', () => ({
  getContentRoot: () => '/mock/content',
  listAllPapers: (...args: unknown[]) => mockListAllPapers(...args),
}))

// ── sample data ───────────────────────────────────────────────────────────────

const samplePapers = [
  {
    title: 'Attention Is All You Need',
    authors: ['Vaswani, A.', 'Shazeer, N.'],
    tags: ['transformer', 'nlp', 'attention'],
    summary: 'A novel architecture based solely on attention mechanisms.',
    category: 'large-language-models',
    slug: 'attention-is-all-you-need',
    year: 2017,
  },
  {
    title: 'BERT: Pre-training of Deep Bidirectional Transformers',
    authors: ['Devlin, J.', 'Chang, M.'],
    tags: ['bert', 'pretraining', 'nlp'],
    summary: 'Pre-training language model for NLP tasks.',
    category: 'large-language-models',
    slug: 'bert',
    year: 2018,
  },
  {
    title: 'Deep Residual Learning for Image Recognition',
    authors: ['He, K.', 'Zhang, X.'],
    tags: ['resnet', 'computer-vision', 'image-classification'],
    summary: 'Residual networks for deep learning.',
    category: 'computer-vision',
    slug: 'resnet',
    year: 2015,
  },
]

// ── tests ─────────────────────────────────────────────────────────────────────

describe('SearchBar', () => {
  beforeEach(() => {
    mockListAllPapers.mockResolvedValue(samplePapers)
    mockPush.mockClear()
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders without crashing', () => {
    render(<SearchBar />)
    expect(
      screen.getByPlaceholderText('논문 검색 (제목·저자)…'),
    ).toBeInTheDocument()
  })

  it('loads papers from Tauri at mount', async () => {
    render(<SearchBar />)
    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())
  })

  it('shows results when query matches title', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())

    fireEvent.change(input, { target: { value: 'Attention' } })

    await waitFor(() => {
      expect(
        screen.getByText('Attention Is All You Need'),
      ).toBeInTheDocument()
    })
  })

  it('shows results when query matches tags', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())

    fireEvent.change(input, { target: { value: 'transformer' } })

    await waitFor(() => {
      expect(
        screen.getByText('Attention Is All You Need'),
      ).toBeInTheDocument()
    })
  })

  it('shows results when query matches author name', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())

    fireEvent.change(input, { target: { value: 'Vaswani' } })

    await waitFor(() => {
      expect(
        screen.getByText('Attention Is All You Need'),
      ).toBeInTheDocument()
    })
  })

  it('shows empty state when no match', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())

    fireEvent.change(input, { target: { value: 'zzznonexistentzzzxxx' } })

    await waitFor(() => {
      expect(screen.getByText('검색 결과 없음')).toBeInTheDocument()
    })
  })

  it('closes dropdown on Escape', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())

    fireEvent.change(input, { target: { value: 'Attention' } })
    await waitFor(() => {
      expect(
        screen.getByText('Attention Is All You Need'),
      ).toBeInTheDocument()
    })

    fireEvent.keyDown(input, { key: 'Escape' })
    expect(
      screen.queryByText('Attention Is All You Need'),
    ).not.toBeInTheDocument()
  })

  it('keyboard shortcut Cmd+K focuses the search input', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    expect(document.activeElement).not.toBe(input)

    fireEvent.keyDown(window, { key: 'k', metaKey: true })

    await waitFor(() => {
      expect(document.activeElement).toBe(input)
    })
  })

  it('shows author names in result items', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색 (제목·저자)…')

    await waitFor(() => expect(mockListAllPapers).toHaveBeenCalled())

    fireEvent.change(input, { target: { value: 'Attention' } })

    await waitFor(() => {
      expect(screen.getByText('Attention Is All You Need')).toBeInTheDocument()
      // Author names rendered in the dropdown item
      expect(screen.getByText(/Vaswani/)).toBeInTheDocument()
    })
  })
})
