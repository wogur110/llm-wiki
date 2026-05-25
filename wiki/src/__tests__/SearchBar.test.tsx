import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import React from 'react'
import SearchBar from '../components/SearchBar'

// ── module mocks (hoisted) ────────────────────────────────────────────────────

const mockPush = vi.fn()
vi.mock('next/navigation', () => ({
  useRouter: () => ({ push: mockPush, replace: vi.fn() }),
}))

// ── helpers ───────────────────────────────────────────────────────────────────

const sampleIndex = [
  {
    title: 'Attention Is All You Need',
    tags: ['transformer', 'nlp', 'attention'],
    summary: 'A novel architecture based solely on attention mechanisms.',
    category: 'large-language-models',
    slug: 'attention-is-all-you-need',
    year: 2017,
  },
  {
    title: 'BERT: Pre-training of Deep Bidirectional Transformers',
    tags: ['bert', 'pretraining', 'nlp'],
    summary: 'Pre-training language model for NLP tasks.',
    category: 'large-language-models',
    slug: 'bert',
    year: 2018,
  },
  {
    title: 'Deep Residual Learning for Image Recognition',
    tags: ['resnet', 'computer-vision', 'image-classification'],
    summary: 'Residual networks for deep learning.',
    category: 'computer-vision',
    slug: 'resnet',
    year: 2015,
  },
]

function mockFetchWithIndex(data = sampleIndex) {
  global.fetch = vi.fn().mockResolvedValue({
    ok: true,
    json: async () => data,
  } as Response)
}

// ── tests ─────────────────────────────────────────────────────────────────────

describe('SearchBar', () => {
  beforeEach(() => {
    mockFetchWithIndex()
    mockPush.mockClear()
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders without crashing', () => {
    render(<SearchBar />)
    expect(screen.getByPlaceholderText('논문 검색…')).toBeInTheDocument()
  })

  it('shows results when query matches title', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색…')

    // Wait for the search index to be fetched and loaded
    await waitFor(() =>
      expect(global.fetch).toHaveBeenCalledWith('/search-index.json')
    )

    fireEvent.change(input, { target: { value: 'Attention' } })

    await waitFor(() => {
      expect(screen.getByText('Attention Is All You Need')).toBeInTheDocument()
    })
  })

  it('shows results when query matches tags', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색…')

    await waitFor(() =>
      expect(global.fetch).toHaveBeenCalledWith('/search-index.json')
    )

    // 'transformer' is in the tags array of the first entry
    fireEvent.change(input, { target: { value: 'transformer' } })

    await waitFor(() => {
      expect(screen.getByText('Attention Is All You Need')).toBeInTheDocument()
    })
  })

  it('shows empty state when no match', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색…')

    await waitFor(() =>
      expect(global.fetch).toHaveBeenCalledWith('/search-index.json')
    )

    // A nonsense query that cannot match any title / tag / summary
    fireEvent.change(input, { target: { value: 'zzznonexistentzzzxxx' } })

    await waitFor(() => {
      expect(screen.getByText('검색 결과 없음')).toBeInTheDocument()
    })
  })

  it('closes dropdown on Escape', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색…')

    await waitFor(() =>
      expect(global.fetch).toHaveBeenCalledWith('/search-index.json'),
    )

    fireEvent.change(input, { target: { value: 'Attention' } })
    await waitFor(() => {
      expect(screen.getByText('Attention Is All You Need')).toBeInTheDocument()
    })

    fireEvent.keyDown(input, { key: 'Escape' })
    expect(screen.queryByText('Attention Is All You Need')).not.toBeInTheDocument()
  })

  it('keyboard shortcut Cmd+K focuses the search input', async () => {
    render(<SearchBar />)
    const input = screen.getByPlaceholderText('논문 검색…')

    // Input should not be focused before the shortcut
    expect(document.activeElement).not.toBe(input)

    // Dispatch Cmd+K on the window
    fireEvent.keyDown(window, { key: 'k', metaKey: true })

    await waitFor(() => {
      expect(document.activeElement).toBe(input)
    })
  })
})
