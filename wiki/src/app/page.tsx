'use client'

/**
 * Dashboard — `/`
 *
 * Three sections:
 *   1. Category cards     (name, paper count, latest paper date)
 *   2. Recent additions   (cross-category feed sorted by created_at)
 *   3. Organize Now       (drives `process_paper` for every unclassified file)
 *
 * All data is loaded from the Rust `content::*` commands so the dashboard
 * reflects the live filesystem state immediately after an organize run.
 */

import { useCallback, useEffect, useState } from 'react'
import Link from 'next/link'
import { invoke } from '@tauri-apps/api/core'

import {
  listCategories,
  listRecentPapers,
  listUnclassified,
  formatRelative,
  type CategoryInfo,
  type PaperMeta,
  type UnclassifiedPaper,
} from '@/lib/content'

type OrganizeState =
  | { kind: 'idle' }
  | { kind: 'running'; done: number; total: number; current: string }
  | { kind: 'finished'; success: number; failed: number; total: number }

type ImportState =
  | { kind: 'idle' }
  | { kind: 'scanning' }
  | { kind: 'running'; done: number; total: number; current: string }
  | { kind: 'finished'; success: number; failed: number; total: number }
  | { kind: 'error'; message: string }

interface PdfEntry {
  path: string
  stem: string
  size_bytes: number
}

export default function DashboardPage() {
  const [categories, setCategories] = useState<CategoryInfo[]>([])
  const [recent, setRecent] = useState<PaperMeta[]>([])
  const [pending, setPending] = useState<UnclassifiedPaper[]>([])
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [organize, setOrganize] = useState<OrganizeState>({ kind: 'idle' })
  const [importState, setImportState] = useState<ImportState>({ kind: 'idle' })

  const refresh = useCallback(async () => {
    // Yield to the microtask queue so the first setState lands after an
    // await boundary (satisfies react-hooks/set-state-in-effect).
    await Promise.resolve()
    setLoading(true)
    setLoadError(null)
    try {
      const [cats, rec, un] = await Promise.all([
        listCategories(),
        listRecentPapers(8),
        listUnclassified(),
      ])
      setCategories(cats)
      setRecent(rec)
      setPending(un)
    } catch (e) {
      setLoadError(String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    let cancelled = false
    ;(async () => {
      if (cancelled) return
      await refresh()
    })()
    return () => {
      cancelled = true
    }
  }, [refresh])

  const handleOrganize = async () => {
    const targets = pending
    if (targets.length === 0 || organize.kind === 'running') return

    const contentRoot = window.localStorage.getItem('content-root')
    if (!contentRoot) {
      setOrganize({ kind: 'finished', success: 0, failed: 0, total: 0 })
      return
    }

    setOrganize({
      kind: 'running',
      done: 0,
      total: targets.length,
      current: targets[0].name,
    })

    let success = 0
    let failed = 0
    for (let i = 0; i < targets.length; i++) {
      const t = targets[i]
      setOrganize({
        kind: 'running',
        done: i,
        total: targets.length,
        current: t.name,
      })
      try {
        await invoke('process_paper', {
          paperPath: t.path,
          contentRoot,
          pdfRoot: null,
          pdfFilename: null,
        })
        success++
      } catch {
        failed++
      }
    }

    setOrganize({ kind: 'finished', success, failed, total: targets.length })
    await refresh()
  }

  const handleImportPdfs = async () => {
    if (importState.kind === 'running' || importState.kind === 'scanning') return

    const pdfRoot   = window.localStorage.getItem('zotero-pdf-root') ?? ''
    const contentRoot = window.localStorage.getItem('content-root') ?? ''
    if (!pdfRoot || !contentRoot) {
      setImportState({
        kind: 'error',
        message: '온보딩에서 Zotero PDF 폴더와 위키 폴더가 설정되어야 합니다.',
      })
      return
    }

    setImportState({ kind: 'scanning' })

    let pdfs: PdfEntry[]
    try {
      pdfs = await invoke<PdfEntry[]>('list_unprocessed_pdfs', {
        pdfRoot,
        contentRoot,
      })
    } catch (e) {
      setImportState({ kind: 'error', message: String(e) })
      return
    }

    if (pdfs.length === 0) {
      setImportState({ kind: 'finished', success: 0, failed: 0, total: 0 })
      return
    }

    let success = 0
    let failed  = 0
    for (let i = 0; i < pdfs.length; i++) {
      const p = pdfs[i]
      setImportState({
        kind: 'running',
        done: i,
        total: pdfs.length,
        current: p.stem,
      })
      try {
        await invoke('import_pdf_and_organize', {
          pdfPath: p.path,
          contentRoot,
          pdfRoot,
        })
        success++
      } catch {
        failed++
      }
    }

    setImportState({ kind: 'finished', success, failed, total: pdfs.length })
    await refresh()
  }

  return (
    <div className="mx-auto max-w-6xl px-6 py-8 space-y-10">
      {/* ── Header ───────────────────────────────────────────────────── */}
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-zinc-900 dark:text-zinc-50">
            대시보드
          </h1>
          <p className="mt-1 text-sm text-zinc-500">
            카테고리 {categories.length}개 · 최근 추가 {recent.length}개 ·
            정리 대기 {pending.length}개
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={refresh}
            disabled={loading}
            className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-1.5 text-xs font-medium text-zinc-700 dark:text-zinc-300 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800 disabled:opacity-50"
          >
            새로고침
          </button>
          <ImportPdfsButton state={importState} onClick={handleImportPdfs} />
          <OrganizeNowButton
            pendingCount={pending.length}
            state={organize}
            onClick={handleOrganize}
          />
        </div>
      </header>

      {importState.kind === 'error' && (
        <div className="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          PDF 가져오기 실패: {importState.message}
        </div>
      )}

      {loadError && (
        <div className="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          데이터를 불러올 수 없습니다: {loadError}
        </div>
      )}

      {/* ── Category cards ──────────────────────────────────────────── */}
      <section>
        <SectionHeading title="카테고리" />
        {loading && categories.length === 0 ? (
          <SkeletonGrid />
        ) : categories.length === 0 ? (
          <EmptyState message="아직 분류된 논문이 없습니다. 정리 대기 중인 파일을 처리해 보세요." />
        ) : (
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
            {categories.map((c) => (
              <Link
                key={c.name}
                href={`/categories/${encodeURIComponent(c.name)}`}
                className="group rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-4 transition-all hover:border-blue-400 dark:hover:border-blue-600 hover:shadow-md"
              >
                <div className="flex items-start justify-between gap-2">
                  <h3 className="font-medium text-zinc-900 dark:text-zinc-100 break-all">
                    {c.name}
                  </h3>
                  <span className="shrink-0 rounded-md bg-blue-50 dark:bg-blue-950 px-2 py-0.5 text-xs font-mono text-blue-700 dark:text-blue-300">
                    {c.paper_count}
                  </span>
                </div>
                <p className="mt-2 text-xs text-zinc-500">
                  {c.latest_paper_date
                    ? `최근 ${formatRelative(c.latest_paper_date)}`
                    : '비어 있음'}
                </p>
              </Link>
            ))}
          </div>
        )}
      </section>

      {/* ── Recent additions feed ───────────────────────────────────── */}
      <section>
        <SectionHeading title="최근 추가" />
        {loading && recent.length === 0 ? (
          <SkeletonList />
        ) : recent.length === 0 ? (
          <EmptyState message="최근에 추가된 논문이 없습니다." />
        ) : (
          <ul className="divide-y divide-zinc-200 dark:divide-zinc-800 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 overflow-hidden">
            {recent.map((p) => (
              <li key={`${p.category}/${p.slug}`}>
                <Link
                  href={`/papers/${encodeURIComponent(p.slug)}`}
                  className="flex items-baseline justify-between gap-3 px-4 py-3 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
                      {p.title}
                    </p>
                    <p className="mt-0.5 flex items-center gap-2 text-xs text-zinc-500">
                      <span className="rounded bg-zinc-100 dark:bg-zinc-800 px-1.5 py-0.5">
                        {p.category}
                      </span>
                      {p.year != null && <span>{p.year}</span>}
                      {p.authors.length > 0 && (
                        <span className="truncate">
                          {p.authors.slice(0, 3).join(', ')}
                          {p.authors.length > 3 && '…'}
                        </span>
                      )}
                    </p>
                  </div>
                  <span className="shrink-0 text-xs text-zinc-400">
                    {formatRelative(p.created_at)}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}

// ── Subcomponents ───────────────────────────────────────────────────────────

function SectionHeading({ title }: { title: string }) {
  return (
    <h2 className="mb-3 text-xs font-semibold uppercase tracking-wider text-zinc-500">
      {title}
    </h2>
  )
}

function EmptyState({ message }: { message: string }) {
  return (
    <div className="rounded-xl border border-dashed border-zinc-300 dark:border-zinc-700 px-4 py-8 text-center text-sm text-zinc-500">
      {message}
    </div>
  )
}

function SkeletonGrid() {
  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
      {Array.from({ length: 3 }, (_, i) => (
        <div
          key={i}
          className="h-24 animate-pulse rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900"
        />
      ))}
    </div>
  )
}

function SkeletonList() {
  return (
    <div className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 divide-y divide-zinc-200 dark:divide-zinc-800">
      {Array.from({ length: 4 }, (_, i) => (
        <div
          key={i}
          className="h-14 animate-pulse bg-zinc-50/60 dark:bg-zinc-900"
        />
      ))}
    </div>
  )
}

function OrganizeNowButton({
  pendingCount,
  state,
  onClick,
}: {
  pendingCount: number
  state: OrganizeState
  onClick: () => void
}) {
  if (state.kind === 'running') {
    return (
      <div className="flex items-center gap-2 rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white">
        <span className="inline-block h-3 w-3 rounded-full border-2 border-white border-t-transparent animate-spin" />
        <span>
          {state.done}/{state.total} · {state.current}
        </span>
      </div>
    )
  }

  if (state.kind === 'finished') {
    const ok = state.failed === 0
    return (
      <button
        type="button"
        onClick={onClick}
        disabled={pendingCount === 0}
        className={`rounded-lg px-3 py-1.5 text-xs font-medium text-white transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
          ok ? 'bg-green-600 hover:bg-green-700' : 'bg-amber-600 hover:bg-amber-700'
        }`}
      >
        {ok ? '✓' : '!'} 완료 {state.success}/{state.total}
        {state.failed > 0 && ` (실패 ${state.failed})`}
      </button>
    )
  }

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={pendingCount === 0}
      className="rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:opacity-50"
    >
      지금 정리 ({pendingCount})
    </button>
  )
}

function ImportPdfsButton({
  state,
  onClick,
}: {
  state: ImportState
  onClick: () => void
}) {
  if (state.kind === 'scanning') {
    return (
      <div className="flex items-center gap-2 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white">
        <span className="inline-block h-3 w-3 rounded-full border-2 border-white border-t-transparent animate-spin" />
        <span>PDF 스캔 중…</span>
      </div>
    )
  }

  if (state.kind === 'running') {
    return (
      <div className="flex items-center gap-2 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white">
        <span className="inline-block h-3 w-3 rounded-full border-2 border-white border-t-transparent animate-spin" />
        <span>
          {state.done}/{state.total} · {state.current}
        </span>
      </div>
    )
  }

  if (state.kind === 'finished') {
    const ok = state.failed === 0
    const label =
      state.total === 0
        ? '새 PDF 없음'
        : `${ok ? '✓' : '!'} ${state.success}/${state.total}`
    return (
      <button
        type="button"
        onClick={onClick}
        className={`rounded-lg px-3 py-1.5 text-xs font-medium text-white transition-colors ${
          ok ? 'bg-green-600 hover:bg-green-700' : 'bg-amber-600 hover:bg-amber-700'
        }`}
      >
        {label}
      </button>
    )
  }

  return (
    <button
      type="button"
      onClick={onClick}
      className="rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-violet-700"
    >
      PDF 가져오기
    </button>
  )
}
