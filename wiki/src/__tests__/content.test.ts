import { describe, it, expect, vi, beforeEach } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

import { invoke } from '@tauri-apps/api/core'
import {
  formatDate,
  formatRelative,
  readBacklinks,
  listCategories,
  listCategoryTree,
  listPapersInCategory,
  listRecentPapers,
  listAllPapers,
  readPaper,
  listUnclassified,
  getContentRoot,
} from '../lib/content'

const mockedInvoke = vi.mocked(invoke)

describe('content helpers', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
  })

  describe('formatDate', () => {
    it('returns empty string for invalid input', () => {
      expect(formatDate(null)).toBe('')
      expect(formatDate('not-a-date')).toBe('')
    })

    it('formats a valid ISO date', () => {
      const out = formatDate('2024-06-15T12:00:00Z')
      expect(out).toMatch(/2024/)
      expect(out).toMatch(/Jun|6/)
    })
  })

  describe('formatRelative', () => {
    it('returns empty string for invalid input', () => {
      expect(formatRelative(undefined)).toBe('')
    })

    it('returns "방금 전" for very recent timestamps', () => {
      const now = new Date().toISOString()
      expect(formatRelative(now)).toBe('방금 전')
    })

    it('falls back to formatDate for old timestamps', () => {
      const old = new Date('2010-01-01T00:00:00Z').toISOString()
      const out = formatRelative(old)
      expect(out).toMatch(/2010/)
    })

    it('returns minutes and hours for recent timestamps', () => {
      const fiveMinAgo = new Date(Date.now() - 5 * 60 * 1000).toISOString()
      expect(formatRelative(fiveMinAgo)).toBe('5분 전')

      const twoHrAgo = new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString()
      expect(formatRelative(twoHrAgo)).toBe('2시간 전')

      const threeDaysAgo = new Date(Date.now() - 3 * 24 * 60 * 60 * 1000).toISOString()
      expect(formatRelative(threeDaysAgo)).toBe('3일 전')
    })
  })

  describe('getContentRoot', () => {
    it('reads content-root from localStorage', () => {
      localStorage.setItem('content-root', '/data/content')
      expect(getContentRoot()).toBe('/data/content')
    })
  })

  describe('Tauri invoke wrappers', () => {
    it('readBacklinks parses JSON from invoke', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce('{"foo":["bar"]}')

      const result = await readBacklinks()
      expect(result).toEqual({ foo: ['bar'] })
      expect(mockedInvoke).toHaveBeenCalledWith('read_backlinks', {
        contentRoot: '/data/content',
      })
    })

    it('readBacklinks returns {} on malformed JSON', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce('not json')

      expect(await readBacklinks()).toEqual({})
    })

    it('listCategories throws when content-root is missing', async () => {
      await expect(listCategories()).rejects.toThrow(/content-root/)
    })

    it('listCategories calls invoke with content root', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce([
        { name: 'nlp', paper_count: 2, latest_paper_date: null },
      ])

      const cats = await listCategories()
      expect(cats).toHaveLength(1)
      expect(cats[0].name).toBe('nlp')
    })

    it('listCategoryTree returns nested nodes from invoke', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce([
        {
          name: 'Computer Vision',
          path: 'Computer Vision',
          paper_count: 0,
          total_paper_count: 1,
          latest_paper_date: null,
          children: [
            {
              name: 'Autoencoders',
              path: 'Computer Vision/Autoencoders',
              paper_count: 1,
              total_paper_count: 1,
              latest_paper_date: '2024-01-01T00:00:00Z',
              children: [],
            },
          ],
        },
      ])

      const tree = await listCategoryTree()
      expect(tree).toHaveLength(1)
      expect(tree[0].children[0].path).toBe('Computer Vision/Autoencoders')
    })

    it('listPapersInCategory parses frontmatter into PaperMeta', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce([
        {
          slug: 'attention',
          category: 'Computer Vision/Autoencoders',
          created_at: '2024-06-01T00:00:00Z',
          frontmatter:
            'title: Attention\nauthors: ["A", "B"]\ndoi: 10.1/abc\nyear: 2017\nzotero_key: KEY1\ntags:\n  - nlp\nsummary: Short summary.\n',
        },
      ])

      const papers = await listPapersInCategory('Computer Vision/Autoencoders')
      expect(papers).toHaveLength(1)
      expect(papers[0].title).toBe('Attention')
      expect(papers[0].authors).toEqual(['A', 'B'])
      expect(papers[0].doi).toBe('10.1/abc')
      expect(papers[0].year).toBe(2017)
      expect(papers[0].zotero_key).toBe('KEY1')
      expect(papers[0].tags).toEqual(['nlp'])
      expect(papers[0].summary).toBe('Short summary.')
    })

    it('listPapersInCategory uses abstract and slug title fallbacks', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce([
        {
          slug: 'my-paper',
          category: 'nlp',
          created_at: '2024-01-01T00:00:00Z',
          frontmatter: 'abstract: From abstract field.\ndate: Spring 2019\n',
        },
      ])

      const papers = await listPapersInCategory('nlp')
      expect(papers[0].summary).toBe('From abstract field.')
      expect(papers[0].year).toBe(2019)
      expect(papers[0].title).toBe('My Paper')
    })

    it('listRecentPapers and listAllPapers map invoke results', async () => {
      localStorage.setItem('content-root', '/data/content')
      const raw = [
        {
          slug: 'gpt',
          category: 'ml/llm',
          created_at: '2024-01-01T00:00:00Z',
          frontmatter: 'title: GPT\n',
        },
      ]
      mockedInvoke.mockResolvedValueOnce(raw)
      const recent = await listRecentPapers(5)
      expect(recent[0].slug).toBe('gpt')
      expect(mockedInvoke).toHaveBeenCalledWith('list_recent_papers', {
        contentRoot: '/data/content',
        limit: 5,
      })

      mockedInvoke.mockResolvedValueOnce(raw)
      const all = await listAllPapers()
      expect(all).toHaveLength(1)
      expect(mockedInvoke).toHaveBeenLastCalledWith('list_recent_papers', {
        contentRoot: '/data/content',
        limit: 100_000,
      })
    })

    it('readPaper loads full markdown body', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke
        .mockResolvedValueOnce('ml/llm')
        .mockResolvedValueOnce({
          slug: 'gpt',
          category: 'ml/llm',
          created_at: '2024-01-01T00:00:00Z',
          content: '---\ntitle: GPT Paper\nyear: 2020\n---\n\n## Intro\n\nBody here.',
        })

      const paper = await readPaper('gpt')
      expect(paper.slug).toBe('gpt')
      expect(paper.title).toBe('GPT Paper')
      expect(paper.year).toBe(2020)
      expect(paper.body).toContain('Body here.')
    })

    it('listUnclassified returns pending files', async () => {
      localStorage.setItem('content-root', '/data/content')
      mockedInvoke.mockResolvedValueOnce([
        { path: '/data/content/papers/unclassified/draft.md', name: 'draft.md', created_at: '2024-01-01T00:00:00Z' },
      ])

      const pending = await listUnclassified()
      expect(pending).toHaveLength(1)
      expect(pending[0].name).toBe('draft.md')
    })
  })
})
