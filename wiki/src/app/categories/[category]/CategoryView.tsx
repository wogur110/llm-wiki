'use client'

/**
 * CategoryView — Client Component for `/categories/[category]`.
 *
 * Lists every paper in the category with sort (date/year/title) and
 * tag filter controls.  Reads via `listPapersInCategory()`.
 */

import { useEffect, useMemo, useState } from 'react'
import Link from 'next/link'
import {
  listPapersInCategory,
  formatDate,
  type PaperMeta,
} from '@/lib/content'

type SortKey = 'date' | 'year' | 'title'

export default function CategoryView({ category }: { category: string }) {
  const [papers, setPapers] = useState<PaperMeta[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [sort, setSort] = useState<SortKey>('date')
  const [activeTags, setActiveTags] = useState<Set<string>>(new Set())

  useEffect(() => {
    let cancelled = false
    ;(async () => {
      // Yield once so the loading flag setState lands after an await.
      await Promise.resolve()
      if (cancelled) return
      setLoading(true)
      setError(null)
      try {
        const p = await listPapersInCategory(category)
        if (!cancelled) setPapers(p)
      } catch (e) {
        if (!cancelled) setError(String(e))
      } finally {
        if (!cancelled) setLoading(false)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [category])

  // ── Tag universe (sorted, deduped) ──────────────────────────────────────
  const allTags = useMemo(() => {
    const counts = new Map<string, number>()
    for (const p of papers) {
      for (const t of p.tags) counts.set(t, (counts.get(t) ?? 0) + 1)
    }
    return Array.from(counts.entries()).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
  }, [papers])

  // ── Filtered + sorted view ──────────────────────────────────────────────
  const visible = useMemo(() => {
    const filtered =
      activeTags.size === 0
        ? papers
        : papers.filter((p) => p.tags.some((t) => activeTags.has(t)))

    const sorted = [...filtered]
    sorted.sort((a, b) => {
      switch (sort) {
        case 'title':
          return a.title.localeCompare(b.title)
        case 'year': {
          const ay = a.year ?? -Infinity
          const by = b.year ?? -Infinity
          if (ay !== by) return by - ay
          return a.title.localeCompare(b.title)
        }
        case 'date':
        default:
          return b.created_at.localeCompare(a.created_at)
      }
    })
    return sorted
  }, [papers, sort, activeTags])

  const toggleTag = (t: string) => {
    setActiveTags((prev) => {
      const next = new Set(prev)
      if (next.has(t)) next.delete(t)
      else next.add(t)
      return next
    })
  }

  // ── Render ──────────────────────────────────────────────────────────────
  return (
    <div className="mx-auto max-w-6xl px-6 py-8 space-y-6">
      <header className="space-y-2">
        <div className="text-xs text-zinc-500">
          <Link href="/" className="hover:underline">
            대시보드
          </Link>{' '}
          / 카테고리
        </div>
        <h1 className="text-2xl font-bold text-zinc-900 dark:text-zinc-50 break-all">
          {category}
        </h1>
        <p className="text-sm text-zinc-500">
          {loading ? '불러오는 중…' : `${visible.length} / ${papers.length}편`}
        </p>
      </header>

      {error && (
        <div className="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          {error}
        </div>
      )}

      {/* ── Controls ────────────────────────────────────────────────── */}
      <div className="flex flex-col gap-3 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-4">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs font-semibold uppercase tracking-wider text-zinc-500">
            정렬
          </span>
          {(['date', 'year', 'title'] as const).map((k) => (
            <button
              key={k}
              type="button"
              onClick={() => setSort(k)}
              className={`rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
                sort === k
                  ? 'bg-blue-600 text-white'
                  : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-200 dark:hover:bg-zinc-700'
              }`}
            >
              {k === 'date' ? '추가순' : k === 'year' ? '연도순' : '제목순'}
            </button>
          ))}
        </div>

        {allTags.length > 0 && (
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs font-semibold uppercase tracking-wider text-zinc-500">
              태그
            </span>
            {allTags.map(([tag, count]) => {
              const active = activeTags.has(tag)
              return (
                <button
                  key={tag}
                  type="button"
                  onClick={() => toggleTag(tag)}
                  className={`rounded-full px-2.5 py-0.5 text-xs font-medium transition-colors ${
                    active
                      ? 'bg-blue-600 text-white'
                      : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-400 hover:bg-zinc-200 dark:hover:bg-zinc-700'
                  }`}
                >
                  {tag}
                  <span className="ml-1 opacity-60">{count}</span>
                </button>
              )
            })}
            {activeTags.size > 0 && (
              <button
                type="button"
                onClick={() => setActiveTags(new Set())}
                className="ml-1 rounded-md px-2 py-0.5 text-xs text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
              >
                모두 해제
              </button>
            )}
          </div>
        )}
      </div>

      {/* ── List ────────────────────────────────────────────────────── */}
      {visible.length === 0 && !loading ? (
        <div className="rounded-xl border border-dashed border-zinc-300 dark:border-zinc-700 px-4 py-12 text-center text-sm text-zinc-500">
          {papers.length === 0
            ? '이 카테고리에 논문이 없습니다.'
            : '필터 조건에 맞는 논문이 없습니다.'}
        </div>
      ) : (
        <ul className="divide-y divide-zinc-200 dark:divide-zinc-800 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 overflow-hidden">
          {visible.map((p) => (
            <li key={p.slug}>
              <Link
                href={`/papers/${encodeURIComponent(p.slug)}`}
                className="block px-4 py-3 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
              >
                <div className="flex flex-wrap items-baseline justify-between gap-2">
                  <h3 className="font-medium text-zinc-900 dark:text-zinc-100">
                    {p.title}
                  </h3>
                  <div className="flex items-baseline gap-3 text-xs text-zinc-500">
                    {p.year != null && (
                      <span className="font-mono">{p.year}</span>
                    )}
                    {p.publication && (
                      <span className="italic truncate max-w-[16rem]">
                        {p.publication}
                      </span>
                    )}
                    <span>{formatDate(p.created_at)}</span>
                  </div>
                </div>
                {p.tags.length > 0 && (
                  <div className="mt-1.5 flex flex-wrap gap-1">
                    {p.tags.map((t) => (
                      <span
                        key={t}
                        className={`rounded-full px-1.5 py-0.5 text-[10px] ${
                          activeTags.has(t)
                            ? 'bg-blue-100 dark:bg-blue-950 text-blue-700 dark:text-blue-300'
                            : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-400'
                        }`}
                      >
                        {t}
                      </span>
                    ))}
                  </div>
                )}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
