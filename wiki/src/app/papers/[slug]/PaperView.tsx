'use client'

/**
 * PaperView — Client Component for `/papers/[slug]`.
 *
 * Two-column layout:
 *   LEFT  Title, metadata grid, rendered markdown body (KaTeX + wikilinks)
 *   RIGHT Sticky sidebar:
 *           - Zotero deep-link button (opens zotero://select/items/<key>)
 *           - Ask Gemini panel (streams via "gemini-stream" events)
 *           - Backlinks                (read from content/meta/backlinks.json)
 *           - Related papers            (tag-intersection ranking)
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import Link from 'next/link'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open as shellOpen } from '@tauri-apps/plugin-shell'

import {
  readPaper,
  listAllPapers,
  readBacklinks,
  formatDate,
  type PaperContent,
  type PaperMeta,
} from '@/lib/content'
import { renderMarkdown } from '@/lib/markdown'

interface Props {
  slug: string
}

const RELATED_LIMIT = 6

export default function PaperView({ slug }: Props) {
  const [paper, setPaper] = useState<PaperContent | null>(null)
  const [html, setHtml] = useState<string>('')
  const [allPapers, setAllPapers] = useState<PaperMeta[]>([])
  const [backlinks, setBacklinks] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  // ── Load paper + body + sidebar data ────────────────────────────────────
  useEffect(() => {
    let cancelled = false
    ;(async () => {
      await Promise.resolve()
      if (cancelled) return
      setLoading(true)
      setError(null)
      try {
        const p = await readPaper(slug)
        if (cancelled) return
        setPaper(p)
        const rendered = await renderMarkdown(p.body)
        if (cancelled) return
        setHtml(rendered)
      } catch (e) {
        if (!cancelled) setError(String(e))
      } finally {
        if (!cancelled) setLoading(false)
      }
    })()
    return () => {
      cancelled = true
    }
  }, [slug])

  // ── Load all papers (for related) + backlinks ───────────────────────────
  useEffect(() => {
    let cancelled = false
    ;(async () => {
      try {
        const [all, links] = await Promise.all([
          listAllPapers(),
          readBacklinks(),
        ])
        if (cancelled) return
        setAllPapers(all)
        setBacklinks(links[slug] ?? [])
      } catch {
        // Sidebar enrichment is non-fatal
      }
    })()
    return () => {
      cancelled = true
    }
  }, [slug])

  // ── Related-papers ranking via tag intersection ─────────────────────────
  const related = useMemo(() => {
    if (!paper || allPapers.length === 0) return []
    const myTags = new Set(paper.tags)
    if (myTags.size === 0) return []
    type Scored = { p: PaperMeta; score: number }
    const scored: Scored[] = []
    for (const cand of allPapers) {
      if (cand.slug === paper.slug) continue
      let shared = 0
      for (const t of cand.tags) if (myTags.has(t)) shared++
      if (shared > 0) scored.push({ p: cand, score: shared })
    }
    scored.sort(
      (a, b) =>
        b.score - a.score || b.p.created_at.localeCompare(a.p.created_at),
    )
    return scored.slice(0, RELATED_LIMIT).map((s) => s.p)
  }, [paper, allPapers])

  // ── Render ──────────────────────────────────────────────────────────────
  if (error) {
    return (
      <div className="mx-auto max-w-3xl px-6 py-12">
        <div className="rounded-xl border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 px-4 py-3 text-sm text-red-700 dark:text-red-300">
          논문을 불러올 수 없습니다: {error}
        </div>
        <Link
          href="/"
          className="mt-4 inline-block text-sm text-blue-600 hover:underline"
        >
          ← 대시보드로 돌아가기
        </Link>
      </div>
    )
  }

  if (loading || !paper) {
    return (
      <div className="mx-auto max-w-6xl px-6 py-8">
        <div className="h-8 w-2/3 animate-pulse rounded bg-zinc-200 dark:bg-zinc-800" />
        <div className="mt-4 h-4 w-1/2 animate-pulse rounded bg-zinc-200 dark:bg-zinc-800" />
        <div className="mt-8 h-64 animate-pulse rounded-xl bg-zinc-100 dark:bg-zinc-900" />
      </div>
    )
  }

  return (
    <div className="mx-auto max-w-6xl px-6 py-8">
      <div className="grid grid-cols-1 gap-8 lg:grid-cols-[minmax(0,1fr)_20rem]">
        {/* ── LEFT: paper body ─────────────────────────────────────────── */}
        <article>
          <Breadcrumb category={paper.category} title={paper.title} />

          <h1 className="mt-3 text-3xl font-bold tracking-tight text-zinc-900 dark:text-zinc-50 leading-tight">
            {paper.title}
          </h1>

          {paper.summary && (
            <p className="mt-3 text-base text-zinc-600 dark:text-zinc-400 leading-relaxed">
              {paper.summary}
            </p>
          )}

          <MetadataGrid paper={paper} />

          <div
            className="paper-body mt-8"
            dangerouslySetInnerHTML={{ __html: html }}
          />
          <style jsx global>{`
            .paper-body h1 { font-size: 1.5rem; font-weight: 600; margin-top: 2rem; margin-bottom: 0.75rem; }
            .paper-body h2 { font-size: 1.25rem; font-weight: 600; margin-top: 1.75rem; margin-bottom: 0.5rem; }
            .paper-body h3 { font-size: 1.05rem; font-weight: 600; margin-top: 1.5rem; margin-bottom: 0.5rem; }
            .paper-body p  { margin: 0.75rem 0; line-height: 1.7; }
            .paper-body ul, .paper-body ol { margin: 0.75rem 0; padding-left: 1.5rem; }
            .paper-body li { margin: 0.25rem 0; }
            .paper-body code:not(pre code) {
              padding: 0.125rem 0.35rem;
              background: rgba(120, 120, 120, 0.12);
              border-radius: 0.25rem;
              font-family: var(--font-geist-mono), monospace;
              font-size: 0.875em;
            }
            .paper-body pre {
              background: rgba(120, 120, 120, 0.08);
              padding: 1rem;
              border-radius: 0.5rem;
              overflow-x: auto;
              margin: 1rem 0;
            }
            .paper-body a { color: rgb(37 99 235); text-decoration: underline; }
            .paper-body a.wikilink {
              text-decoration: none;
              border-bottom: 1px dashed rgb(37 99 235);
            }
            .paper-body blockquote {
              border-left: 3px solid rgb(212 212 216);
              padding-left: 1rem;
              color: rgb(113 113 122);
              margin: 1rem 0;
            }
            .paper-body table { border-collapse: collapse; margin: 1rem 0; }
            .paper-body th, .paper-body td {
              border: 1px solid rgb(228 228 231);
              padding: 0.5rem 0.75rem;
            }
            .paper-body th { background: rgba(120, 120, 120, 0.06); }
          `}</style>
        </article>

        {/* ── RIGHT: sticky sidebar ────────────────────────────────────── */}
        <aside className="space-y-4 lg:sticky lg:top-20 lg:self-start lg:max-h-[calc(100vh-6rem)] lg:overflow-y-auto pr-1">
          {paper.zotero_key && <ZoteroButton itemKey={paper.zotero_key} />}
          <AskGeminiPanel paper={paper} />
          <BacklinksPanel slugs={backlinks} />
          <RelatedPanel papers={related} />
        </aside>
      </div>
    </div>
  )
}

// ── Subcomponents ───────────────────────────────────────────────────────────

function Breadcrumb({ category, title }: { category: string; title: string }) {
  return (
    <div className="text-xs text-zinc-500">
      <Link href="/" className="hover:underline">
        대시보드
      </Link>{' '}
      /{' '}
      <Link
        href={`/categories/${encodeURIComponent(category)}`}
        className="hover:underline"
      >
        {category}
      </Link>{' '}
      / <span className="text-zinc-700 dark:text-zinc-300 truncate">{title}</span>
    </div>
  )
}

function MetadataGrid({ paper }: { paper: PaperContent }) {
  const rows: Array<{ label: string; value: React.ReactNode }> = []
  if (paper.authors.length > 0)
    rows.push({ label: '저자', value: paper.authors.join(', ') })
  if (paper.year != null) rows.push({ label: '연도', value: paper.year })
  if (paper.publication)
    rows.push({ label: '게재', value: <em>{paper.publication}</em> })
  if (paper.doi)
    rows.push({
      label: 'DOI',
      value: (
        <a
          href={`https://doi.org/${paper.doi}`}
          target="_blank"
          rel="noopener noreferrer"
          className="font-mono text-blue-600 hover:underline break-all"
        >
          {paper.doi}
        </a>
      ),
    })
  rows.push({ label: '추가일', value: formatDate(paper.created_at) })

  if (rows.length === 0) return null
  return (
    <dl className="mt-6 grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 px-5 py-4 text-sm">
      {rows.map((r) => (
        <div key={r.label} className="contents">
          <dt className="text-xs font-semibold uppercase tracking-wider text-zinc-500 pt-0.5">
            {r.label}
          </dt>
          <dd className="text-zinc-700 dark:text-zinc-300">{r.value}</dd>
        </div>
      ))}
    </dl>
  )
}

function ZoteroButton({ itemKey }: { itemKey: string }) {
  const [opening, setOpening] = useState(false)
  const [openError, setOpenError] = useState<string | null>(null)

  const handleClick = async () => {
    setOpening(true)
    setOpenError(null)
    try {
      await shellOpen(`zotero://select/items/${itemKey}`)
    } catch (e) {
      setOpenError(String(e))
    } finally {
      setOpening(false)
    }
  }

  return (
    <SidebarSection>
      <button
        type="button"
        onClick={handleClick}
        disabled={opening}
        className="flex w-full items-center justify-center gap-2 rounded-lg bg-red-600 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-red-700 disabled:opacity-60"
      >
        {opening ? '여는 중…' : 'Zotero에서 열기'}
      </button>
      {openError && (
        <p className="mt-2 text-xs text-red-500 break-all">{openError}</p>
      )}
      <p className="mt-2 text-[11px] text-zinc-500 font-mono break-all">
        zotero://select/items/{itemKey}
      </p>
    </SidebarSection>
  )
}

// ── Ask Gemini ──────────────────────────────────────────────────────────────

function AskGeminiPanel({ paper }: { paper: PaperContent }) {
  const [question, setQuestion] = useState('')
  const [answer, setAnswer] = useState('')
  const [streaming, setStreaming] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const answerRef = useRef<HTMLDivElement | null>(null)

  // Subscribe to the three Gemini stream events for this paper's lifetime.
  useEffect(() => {
    let unlistenChunk: (() => void) | undefined
    let unlistenDone: (() => void) | undefined
    let unlistenError: (() => void) | undefined

    ;(async () => {
      unlistenChunk = await listen<string>('gemini-stream', (e) => {
        setAnswer((prev) => prev + e.payload)
      })
      unlistenDone = await listen<unknown>('gemini-stream-done', () => {
        setStreaming(false)
      })
      unlistenError = await listen<string>('gemini-stream-error', (e) => {
        setError(e.payload)
        setStreaming(false)
      })
    })()

    return () => {
      unlistenChunk?.()
      unlistenDone?.()
      unlistenError?.()
    }
  }, [])

  // Auto-scroll the answer box as new chunks arrive.
  useEffect(() => {
    if (answerRef.current) {
      answerRef.current.scrollTop = answerRef.current.scrollHeight
    }
  }, [answer])

  const handleAsk = useCallback(async () => {
    const q = question.trim()
    if (!q || streaming) return
    setAnswer('')
    setError(null)
    setStreaming(true)

    const contextHeader = [
      `You are answering questions about the following research paper.`,
      `Title: ${paper.title}`,
      paper.authors.length > 0 ? `Authors: ${paper.authors.join(', ')}` : null,
      paper.year != null ? `Year: ${paper.year}` : null,
      paper.publication ? `Publication: ${paper.publication}` : null,
      paper.doi ? `DOI: ${paper.doi}` : null,
      ``,
      `--- Paper content (markdown) ---`,
      paper.body.slice(0, 12_000),
      `--- End of paper content ---`,
      ``,
      `Answer concisely in Korean unless the user asks otherwise.`,
    ]
      .filter(Boolean)
      .join('\n')

    const messages = [
      { role: 'user', text: contextHeader },
      { role: 'user', text: q },
    ]

    try {
      await invoke('call_gemini', { messages })
    } catch (e) {
      setError(String(e))
      setStreaming(false)
    }
  }, [paper, question, streaming])

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      handleAsk()
    }
  }

  return (
    <SidebarSection title="Gemini에 질문">
      <textarea
        rows={3}
        value={question}
        onChange={(e) => setQuestion(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="이 논문에 대해 물어보세요…"
        className="w-full rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-2.5 py-2 text-sm placeholder:text-zinc-400 focus:outline-none focus:ring-2 focus:ring-blue-500/40 focus:border-blue-500 resize-none"
      />
      <div className="mt-2 flex items-center justify-between gap-2">
        <span className="text-[11px] text-zinc-400 hidden sm:inline">⌘/Ctrl + Enter</span>
        <button
          type="button"
          onClick={handleAsk}
          disabled={streaming || !question.trim()}
          className="ml-auto rounded-md bg-blue-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
        >
          {streaming ? (
            <span className="flex items-center gap-1.5">
              <span className="inline-block h-2.5 w-2.5 rounded-full border-2 border-white border-t-transparent animate-spin" />
              스트리밍…
            </span>
          ) : (
            '질문하기'
          )}
        </button>
      </div>

      {(answer || streaming || error) && (
        <div
          ref={answerRef}
          className="mt-3 max-h-72 overflow-y-auto rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-950 px-2.5 py-2 text-xs text-zinc-800 dark:text-zinc-200 whitespace-pre-wrap leading-relaxed"
        >
          {answer || (streaming ? '응답을 기다리는 중…' : '')}
          {error && <p className="mt-2 text-red-500">오류: {error}</p>}
        </div>
      )}
    </SidebarSection>
  )
}

// ── Sidebar lists ───────────────────────────────────────────────────────────

function BacklinksPanel({ slugs }: { slugs: string[] }) {
  return (
    <SidebarSection title={`백링크 (${slugs.length})`}>
      {slugs.length === 0 ? (
        <p className="text-xs text-zinc-400">이 논문을 참조하는 글이 없습니다.</p>
      ) : (
        <ul className="space-y-1">
          {slugs.map((s) => (
            <li key={s}>
              <Link
                href={`/papers/${encodeURIComponent(s)}`}
                className="block truncate rounded px-1.5 py-1 text-xs text-blue-600 dark:text-blue-400 hover:bg-blue-50 dark:hover:bg-blue-950/40"
              >
                ← {s}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </SidebarSection>
  )
}

function RelatedPanel({ papers }: { papers: PaperMeta[] }) {
  return (
    <SidebarSection title={`관련 논문 (${papers.length})`}>
      {papers.length === 0 ? (
        <p className="text-xs text-zinc-400">태그가 겹치는 논문이 없습니다.</p>
      ) : (
        <ul className="space-y-1.5">
          {papers.map((p) => (
            <li key={p.slug}>
              <Link
                href={`/papers/${encodeURIComponent(p.slug)}`}
                className="block rounded px-1.5 py-1.5 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
              >
                <p className="truncate text-xs font-medium text-zinc-800 dark:text-zinc-200">
                  {p.title}
                </p>
                <p className="mt-0.5 truncate text-[10px] text-zinc-500">
                  {p.category}
                  {p.year != null && ` · ${p.year}`}
                </p>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </SidebarSection>
  )
}

function SidebarSection({
  title,
  children,
}: {
  title?: string
  children: React.ReactNode
}) {
  return (
    <section className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-3.5">
      {title && (
        <h2 className="mb-2 text-xs font-semibold uppercase tracking-wider text-zinc-500">
          {title}
        </h2>
      )}
      {children}
    </section>
  )
}
