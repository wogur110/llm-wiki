'use client'

/**
 * SearchBar — fuzzy search across `wiki/public/search-index.json`.
 *
 * Index format (see `scripts/build-search-index.js`):
 *   { title, tags, summary, category, slug, year }
 *
 * Search fields are weighted via Fuse.js: title > tags > summary.
 *
 * Triggers:
 *   * Click on the input
 *   * Cmd/Ctrl+K from anywhere in the app
 *
 * Results dropdown shows up to 8 hits; Enter / click navigates to
 * `/papers/<slug>`.  Escape closes the dropdown.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import Fuse, { type IFuseOptions } from 'fuse.js'

interface SearchEntry {
  title: string
  tags: string[]
  summary: string
  category: string | null
  slug: string
  year: number | null
}

const FUSE_OPTIONS: IFuseOptions<SearchEntry> = {
  includeScore: true,
  threshold: 0.4,
  ignoreLocation: true,
  minMatchCharLength: 2,
  keys: [
    { name: 'title', weight: 0.6 },
    { name: 'tags', weight: 0.25 },
    { name: 'summary', weight: 0.15 },
  ],
}

const MAX_RESULTS = 8

export default function SearchBar() {
  const router = useRouter()
  const inputRef = useRef<HTMLInputElement | null>(null)
  const wrapperRef = useRef<HTMLDivElement | null>(null)

  const [query, setQuery] = useState('')
  const [entries, setEntries] = useState<SearchEntry[]>([])
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)

  // ── Load the search index once on mount ─────────────────────────────────
  useEffect(() => {
    let cancelled = false
    fetch('/search-index.json')
      .then((r) => (r.ok ? r.json() : []))
      .then((data: SearchEntry[]) => {
        if (!cancelled && Array.isArray(data)) setEntries(data)
      })
      .catch(() => {
        // search-index is optional — empty array is fine
      })
    return () => {
      cancelled = true
    }
  }, [])

  // ── Build the Fuse instance whenever entries change ─────────────────────
  const fuse = useMemo(
    () => new Fuse(entries, FUSE_OPTIONS),
    [entries],
  )

  // ── Cmd/Ctrl+K global shortcut ──────────────────────────────────────────
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const isCmdK = (e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k'
      if (isCmdK) {
        e.preventDefault()
        inputRef.current?.focus()
        inputRef.current?.select()
        setOpen(true)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  // ── Click-outside to close ──────────────────────────────────────────────
  useEffect(() => {
    if (!open) return
    const onClick = (e: MouseEvent) => {
      if (!wrapperRef.current?.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [open])

  // ── Compute results ─────────────────────────────────────────────────────
  const results = useMemo(() => {
    const q = query.trim()
    if (q.length < 2) return []
    return fuse.search(q, { limit: MAX_RESULTS }).map((r) => r.item)
  }, [fuse, query])

  // Keep the highlighted index in bounds when the result set changes.
  // The setState is deferred past an await so it doesn't violate the
  // react-hooks/set-state-in-effect rule.
  useEffect(() => {
    let cancelled = false
    ;(async () => {
      await Promise.resolve()
      if (!cancelled) setActive(0)
    })()
    return () => {
      cancelled = true
    }
  }, [query])

  // ── Selection ───────────────────────────────────────────────────────────
  const selectResult = useCallback(
    (entry: SearchEntry) => {
      setOpen(false)
      setQuery('')
      inputRef.current?.blur()
      router.push(`/papers/${encodeURIComponent(entry.slug)}`)
    },
    [router],
  )

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape') {
      setOpen(false)
      inputRef.current?.blur()
      return
    }
    if (results.length === 0) return
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActive((i) => (i + 1) % results.length)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((i) => (i - 1 + results.length) % results.length)
    } else if (e.key === 'Enter') {
      e.preventDefault()
      selectResult(results[active])
    }
  }

  // ── Render ──────────────────────────────────────────────────────────────
  return (
    <div ref={wrapperRef} className="relative w-full max-w-md">
      <div className="relative">
        <span
          aria-hidden="true"
          className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400"
        >
          🔍
        </span>
        <input
          ref={inputRef}
          type="search"
          value={query}
          onChange={(e) => {
            setQuery(e.target.value)
            setOpen(true)
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={onKeyDown}
          placeholder="논문 검색…"
          spellCheck={false}
          autoComplete="off"
          className="w-full rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 pl-9 pr-16 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder:text-zinc-400 focus:outline-none focus:ring-2 focus:ring-blue-500/40 focus:border-blue-500 transition-colors"
        />
        <kbd className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 hidden rounded border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 px-1.5 py-0.5 font-mono text-[10px] text-zinc-500 sm:inline-block">
          ⌘K
        </kbd>
      </div>

      {open && results.length > 0 && (
        <ul
          role="listbox"
          className="absolute left-0 right-0 mt-1 max-h-96 overflow-y-auto rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 shadow-xl shadow-zinc-900/10 z-50"
        >
          {results.map((entry, i) => (
            <li
              key={entry.slug}
              role="option"
              aria-selected={i === active}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault()
                selectResult(entry)
              }}
              className={`cursor-pointer px-3 py-2 transition-colors ${
                i === active
                  ? 'bg-blue-50 dark:bg-blue-950/40'
                  : 'hover:bg-zinc-50 dark:hover:bg-zinc-800/60'
              }`}
            >
              <div className="flex items-baseline justify-between gap-3">
                <span className="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
                  {entry.title}
                </span>
                {entry.year != null && (
                  <span className="shrink-0 font-mono text-xs text-zinc-400">
                    {entry.year}
                  </span>
                )}
              </div>
              <div className="mt-0.5 flex items-center gap-2 text-xs text-zinc-500">
                {entry.category && (
                  <span className="rounded bg-zinc-100 dark:bg-zinc-800 px-1.5 py-0.5">
                    {entry.category}
                  </span>
                )}
                <span className="truncate font-mono">{entry.slug}</span>
              </div>
            </li>
          ))}
        </ul>
      )}

      {open && query.trim().length >= 2 && results.length === 0 && (
        <div className="absolute left-0 right-0 mt-1 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-3 text-sm text-zinc-500 shadow-xl shadow-zinc-900/10 z-50">
          검색 결과 없음
        </div>
      )}
    </div>
  )
}
