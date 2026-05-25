import { describe, it, expect, vi, beforeEach } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

import { invoke } from '@tauri-apps/api/core'
import {
  formatDate,
  formatRelative,
  readBacklinks,
  listCategories,
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
  })
})
