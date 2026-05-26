'use client'

/**
 * Dashboard — `/`
 *
 * Three sections:
 *   1. Category cards     (name, paper count, latest paper date)
 *      → clicking a card shows that category's papers inline
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
  listPapersInCategory,
  formatRelative,
  type CategoryInfo,
  type PaperMeta,
  type UnclassifiedPaper,
} from '@/lib/content'

type OrganizeState =
  | { kind: 'idle' }
  | { kind: 'running'; done: number; total: number; current: string }
  | { kind: 'finished'; success: number; failed: number; total: number }

type ImportSource = 'unclassified' | 'library'

type ImportState =
  | { kind: 'idle' }
  | { kind: 'scanning'; source: ImportSource }
  | { kind: 'running';  source: ImportSource; done: number; total: number; current: string }
  | { kind: 'finished'; source: ImportSource; success: number; failed: number; total: number }
  | { kind: 'error';    source: ImportSource; message: string }

type SyncState =
  | { kind: 'idle' }
  | { kind: 'running' }
  | { kind: 'finished'; foldersCreated: number; collectionsCreated: number; errors: number }
  | { kind: 'error'; message: string }

interface ZoteroPdfImportEntry {
  item_key: string
  attachment_key: string
  title: string
  slug: string
  /** Non-null for library-import items; Gemini will classify when null. */
  collection_name: string | null
}

export default function DashboardPage() {
  const [categories, setCategories] = useState<CategoryInfo[]>([])
  const [recent, setRecent] = useState<PaperMeta[]>([])
  const [pending, setPending] = useState<UnclassifiedPaper[]>([])
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [organize, setOrganize] = useState<OrganizeState>({ kind: 'idle' })
  const [importState, setImportState] = useState<ImportState>({ kind: 'idle' })
  const [syncState, setSyncState] = useState<SyncState>({ kind: 'idle' })

  // ── Inline category drilldown ─────────────────────────────────────────
  const [selectedCategory, setSelectedCategory] = useState<string | null>(null)
  const [categoryPapers, setCategoryPapers] = useState<PaperMeta[]>([])
  const [loadingCategoryPapers, setLoadingCategoryPapers] = useState(false)

  const handleSelectCategory = useCallback(async (name: string) => {
    setSelectedCategory(name)
    setLoadingCategoryPapers(true)
    try {
      const papers = await listPapersInCategory(name)
      setCategoryPapers(papers)
    } catch {
      setCategoryPapers([])
    } finally {
      setLoadingCategoryPapers(false)
    }
  }, [])

  const handleBackToCategories = useCallback(() => {
    setSelectedCategory(null)
    setCategoryPapers([])
  }, [])

  const refresh = useCallback(async () => {
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

  /**
   * Bidirectional Zotero ↔ wiki folder structure sync.
   *
   * Creates missing category folders in the wiki for every Zotero collection,
   * and creates missing Zotero collections for every wiki category folder.
   */
  const handleSync = async () => {
    if (syncState.kind === 'running') return
    const contentRoot = window.localStorage.getItem('content-root') ?? ''
    if (!contentRoot) return

    setSyncState({ kind: 'running' })
    try {
      const result = await invoke<{
        folders_created: string[]
        collections_created: string[]
        errors: string[]
      }>('sync_zotero_structure', { contentRoot })
      setSyncState({
        kind: 'finished',
        foldersCreated: result.folders_created.length,
        collectionsCreated: result.collections_created.length,
        errors: result.errors.length,
      })
      await refresh()
    } catch (e) {
      setSyncState({ kind: 'error', message: String(e) })
    }
  }

  /**
   * Drive the Zotero-driven PDF import flow.
   *
   * `source = 'unclassified'`  → only the `Unclassified` collection;
   *                               Gemini classifies with existing-category hints
   * `source = 'library'`       → every top-level item in the Zotero library;
   *                               items with a known collection skip Gemini
   *
   * Already-imported papers (by slug) are filtered out backend-side.
   */
  const runImport = async (source: ImportSource) => {
    if (importState.kind === 'running' || importState.kind === 'scanning') return

    const contentRoot = window.localStorage.getItem('content-root') ?? ''
    if (!contentRoot) {
      setImportState({
        kind: 'error',
        source,
        message: '위키 폴더가 초기화되지 않았습니다. 앱을 재시작해 주세요.',
      })
      return
    }

    setImportState({ kind: 'scanning', source })

    let items: ZoteroPdfImportEntry[]
    try {
      items = source === 'unclassified'
        ? await invoke<ZoteroPdfImportEntry[]>(
            'list_zotero_unclassified',
            { collection: null, contentRoot },
          )
        : await invoke<ZoteroPdfImportEntry[]>(
            'list_zotero_all',
            { contentRoot },
          )
    } catch (e) {
      setImportState({ kind: 'error', source, message: String(e) })
      return
    }

    if (items.length === 0) {
      setImportState({ kind: 'finished', source, success: 0, failed: 0, total: 0 })
      return
    }

    let success = 0
    let failed  = 0
    for (let i = 0; i < items.length; i++) {
      const item = items[i]
      setImportState({
        kind: 'running',
        source,
        done: i,
        total: items.length,
        current: item.title,
      })
      try {
        await invoke('import_zotero_item_and_organize', {
          itemKey: item.item_key,
          attachmentKey: item.attachment_key,
          contentRoot,
          // Pass known collection as override so Gemini is skipped for
          // items that already have an established category in Zotero.
          overrideCategory: item.collection_name ?? null,
        })
        success++
      } catch {
        failed++
      }
    }

    setImportState({ kind: 'finished', source, success, failed, total: items.length })
    await refresh()
  }

  const handleImportPdfs    = () => runImport('unclassified')
  const handleImportLibrary = () => runImport('library')

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
          <ImportPdfsButton
            source="unclassified"
            state={importState}
            onClick={handleImportPdfs}
            idleLabel="PDF 가져오기"
            tooltip="Zotero의 Unclassified 컬렉션에서 새 PDF만 가져옵니다"
          />
          <ImportPdfsButton
            source="library"
            state={importState}
            onClick={handleImportLibrary}
            idleLabel="기존 Zotero 불러오기"
            tooltip="Zotero 라이브러리 전체 PDF를 한 번에 불러옵니다 (폴더 구조 그대로 유지)"
            variant="secondary"
          />
          <SyncButton state={syncState} onClick={handleSync} />
          <OrganizeNowButton
            pendingCount={pending.length}
            state={organize}
            onClick={handleOrganize}
          />
        </div>
      </header>

      {importState.kind === 'error' && (
        <div className="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          {importState.source === 'library' ? '기존 Zotero 불러오기' : 'PDF 가져오기'} 실패: {importState.message}
        </div>
      )}

      {syncState.kind === 'error' && (
        <div className="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          Zotero 동기화 실패: {syncState.message}
        </div>
      )}

      {loadError && (
        <div className="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          데이터를 불러올 수 없습니다: {loadError}
        </div>
      )}

      {/* ── Category section — grid or drilldown ─────────────────────── */}
      <section>
        {selectedCategory ? (
          // ── Drilldown: papers in selected category ────────────────────
          <>
            <div className="mb-3 flex items-center gap-3">
              <button
                type="button"
                onClick={handleBackToCategories}
                className="flex items-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
              >
                ← 카테고리 목록
              </button>
              <h2 className="text-xs font-semibold uppercase tracking-wider text-zinc-500">
                {selectedCategory}
              </h2>
              {!loadingCategoryPapers && (
                <span className="rounded-md bg-blue-50 dark:bg-blue-950 px-2 py-0.5 text-xs font-mono text-blue-700 dark:text-blue-300">
                  {categoryPapers.length}
                </span>
              )}
            </div>
            {loadingCategoryPapers ? (
              <SkeletonList />
            ) : categoryPapers.length === 0 ? (
              <EmptyState message="이 카테고리에 논문이 없습니다." />
            ) : (
              <ul className="divide-y divide-zinc-200 dark:divide-zinc-800 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 overflow-hidden">
                {categoryPapers.map((p) => (
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
          </>
        ) : (
          // ── Category grid ──────────────────────────────────────────────
          <>
            <SectionHeading title="카테고리" />
            {loading && categories.length === 0 ? (
              <SkeletonGrid />
            ) : categories.length === 0 ? (
              <EmptyState message="아직 분류된 논문이 없습니다. 정리 대기 중인 파일을 처리해 보세요." />
            ) : (
              <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
                {categories.map((c) => (
                  <button
                    key={c.name}
                    type="button"
                    onClick={() => handleSelectCategory(c.name)}
                    className="group rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-4 text-left transition-all hover:border-blue-400 dark:hover:border-blue-600 hover:shadow-md"
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
                  </button>
                ))}
              </div>
            )}
          </>
        )}
      </section>

      {/* ── Recent additions feed ───────────────────────────────────── */}
      {!selectedCategory && (
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
      )}
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

/**
 * Zotero structure sync button — syncs llm-wiki folders ↔ Zotero collections.
 */
function SyncButton({
  state,
  onClick,
}: {
  state: SyncState
  onClick: () => void
}) {
  if (state.kind === 'running') {
    return (
      <div className="flex items-center gap-2 rounded-lg bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white">
        <span className="inline-block h-3 w-3 rounded-full border-2 border-white border-t-transparent animate-spin" />
        <span>동기화 중…</span>
      </div>
    )
  }

  if (state.kind === 'finished') {
    const hasDiff = state.foldersCreated > 0 || state.collectionsCreated > 0
    const hasErrors = state.errors > 0
    const label = hasErrors
      ? `동기화 완료 (오류 ${state.errors})`
      : hasDiff
        ? `동기화 완료 (+${state.foldersCreated + state.collectionsCreated})`
        : '동기화 완료'
    return (
      <button
        type="button"
        onClick={onClick}
        title="폴더/컬렉션 구조를 다시 동기화합니다"
        className={`rounded-lg px-3 py-1.5 text-xs font-medium text-white transition-colors ${
          hasErrors
            ? 'bg-amber-600 hover:bg-amber-700'
            : 'bg-emerald-600 hover:bg-emerald-700'
        }`}
      >
        {hasErrors ? '!' : '✓'} {label}
      </button>
    )
  }

  return (
    <button
      type="button"
      onClick={onClick}
      title="Zotero 컬렉션과 LLM-Wiki 폴더 구조를 양방향으로 동기화합니다"
      className="rounded-lg border border-emerald-300 dark:border-emerald-700 bg-white dark:bg-zinc-900 px-3 py-1.5 text-xs font-medium text-emerald-700 dark:text-emerald-300 transition-colors hover:bg-emerald-50 dark:hover:bg-emerald-950/30 disabled:cursor-not-allowed disabled:opacity-50"
    >
      Zotero 동기화
    </button>
  )
}

/**
 * Trigger one of the Zotero import flows.  Two of these live side-by-side on
 * the dashboard and share a single `importState`; the `source` prop tells the
 * component which flow it represents so only the active button shows progress
 * while the other stays disabled and idle-styled.
 */
function ImportPdfsButton({
  source,
  state,
  onClick,
  idleLabel,
  tooltip,
  variant = 'primary',
}: {
  source: ImportSource
  state: ImportState
  onClick: () => void
  idleLabel: string
  tooltip?: string
  variant?: 'primary' | 'secondary'
}) {
  const isActive  = state.kind !== 'idle' && 'source' in state && state.source === source
  const otherBusy =
    !isActive &&
    (state.kind === 'scanning' || state.kind === 'running')

  if (isActive && state.kind === 'scanning') {
    return (
      <div className="flex items-center gap-2 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white">
        <span className="inline-block h-3 w-3 rounded-full border-2 border-white border-t-transparent animate-spin" />
        <span>PDF 스캔 중…</span>
      </div>
    )
  }

  if (isActive && state.kind === 'running') {
    return (
      <div className="flex items-center gap-2 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white">
        <span className="inline-block h-3 w-3 rounded-full border-2 border-white border-t-transparent animate-spin" />
        <span>
          {state.done}/{state.total} · {state.current}
        </span>
      </div>
    )
  }

  if (isActive && state.kind === 'finished') {
    const ok = state.failed === 0
    const label =
      state.total === 0
        ? '새 PDF 없음'
        : `${ok ? '✓' : '!'} ${state.success}/${state.total}`
    return (
      <button
        type="button"
        onClick={onClick}
        title={tooltip}
        className={`rounded-lg px-3 py-1.5 text-xs font-medium text-white transition-colors ${
          ok ? 'bg-green-600 hover:bg-green-700' : 'bg-amber-600 hover:bg-amber-700'
        }`}
      >
        {label}
      </button>
    )
  }

  const idleClass =
    variant === 'primary'
      ? 'bg-violet-600 text-white hover:bg-violet-700'
      : 'border border-violet-300 dark:border-violet-700 bg-white dark:bg-zinc-900 text-violet-700 dark:text-violet-300 hover:bg-violet-50 dark:hover:bg-violet-950/30'

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={otherBusy}
      title={tooltip}
      className={`rounded-lg px-3 py-1.5 text-xs font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${idleClass}`}
    >
      {idleLabel}
    </button>
  )
}
