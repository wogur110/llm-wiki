'use client'

import { useEffect, useState } from 'react'
import Link from 'next/link'
import { open as shellOpen } from '@tauri-apps/plugin-shell'
import { type PaperMeta, formatDate } from '@/lib/content'
import { usePaperPreview } from './PaperPreviewContext'

export default function PaperPreviewDrawer() {
  const { previewPaper, closePreview } = usePaperPreview()
  const [prevPaper, setPrevPaper] = useState<PaperMeta | null>(null)
  const [activePaper, setActivePaper] = useState<PaperMeta | null>(null)
  const [isOpen, setIsOpen] = useState(false)
  const [openingZotero, setOpeningZotero] = useState(false)
  const [zoteroError, setZoteroError] = useState<string | null>(null)

  if (previewPaper !== prevPaper) {
    setPrevPaper(previewPaper)
    if (previewPaper) {
      setActivePaper(previewPaper)
      setIsOpen(true)
      setZoteroError(null)
    } else {
      setIsOpen(false)
    }
  }

  useEffect(() => {
    if (!isOpen && activePaper) {
      const timer = setTimeout(() => {
        setActivePaper(null)
      }, 300) // matches CSS transition duration
      return () => clearTimeout(timer)
    }
  }, [isOpen, activePaper])

  if (!activePaper) return null

  const handleOpenZotero = async () => {
    if (!activePaper.zotero_key) return
    setOpeningZotero(true)
    setZoteroError(null)
    try {
      await shellOpen(`zotero://select/items/${activePaper.zotero_key}`)
    } catch (e) {
      setZoteroError(String(e))
    } finally {
      setOpeningZotero(false)
    }
  }

  return (
    <div className={`fixed inset-0 z-50 pointer-events-none ${isOpen ? 'pointer-events-auto' : ''}`}>
      {/* Backdrop with fade effect */}
      <div
        className={`absolute inset-0 bg-zinc-950/20 dark:bg-black/40 backdrop-blur-xs transition-opacity duration-300 ease-in-out ${
          isOpen ? 'opacity-100' : 'opacity-0 pointer-events-none'
        }`}
        onClick={closePreview}
      />

      {/* Drawer panel with slide effect */}
      <div
        className={`absolute top-0 right-0 h-full w-full sm:w-[480px] bg-white/95 dark:bg-zinc-900/95 backdrop-blur-md border-l border-zinc-200 dark:border-zinc-800 shadow-2xl transition-transform duration-300 ease-in-out flex flex-col ${
          isOpen ? 'translate-x-0' : 'translate-x-full'
        }`}
      >
        {/* Header */}
        <div className="flex items-start justify-between p-5 border-b border-zinc-100 dark:border-zinc-800/80">
          <div className="min-w-0 pr-4">
            <Link
              href={`/categories/${encodeURIComponent(activePaper.category)}`}
              onClick={closePreview}
              className="inline-block rounded-md bg-blue-50 dark:bg-blue-950/60 px-2.5 py-0.5 text-xs font-semibold text-blue-700 dark:text-blue-300 hover:bg-blue-100 dark:hover:bg-blue-900/50 transition-colors"
            >
              {activePaper.category}
            </Link>
            <h3 className="mt-2.5 text-lg font-bold tracking-tight text-zinc-900 dark:text-zinc-50 leading-snug">
              {activePaper.title}
            </h3>
          </div>
          <button
            type="button"
            onClick={closePreview}
            className="shrink-0 rounded-full p-1.5 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-600 dark:hover:text-zinc-300 transition-colors"
            aria-label="닫기"
          >
            ✕
          </button>
        </div>

        {/* Body content */}
        <div className="flex-1 overflow-y-auto p-5 space-y-6">
          {/* Metadata Grid */}
          <div>
            <h4 className="text-xs font-bold uppercase tracking-wider text-zinc-400 dark:text-zinc-500 mb-3">논문 정보</h4>
            <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-3 text-sm">
              {activePaper.authors.length > 0 && (
                <div className="contents">
                  <dt className="text-zinc-400 dark:text-zinc-500">저자</dt>
                  <dd className="text-zinc-700 dark:text-zinc-300 font-medium">{activePaper.authors.join(', ')}</dd>
                </div>
              )}
              {activePaper.year != null && (
                <div className="contents">
                  <dt className="text-zinc-400 dark:text-zinc-500">발행 연도</dt>
                  <dd className="text-zinc-700 dark:text-zinc-300">{activePaper.year}년</dd>
                </div>
              )}
              {activePaper.publication && (
                <div className="contents">
                  <dt className="text-zinc-400 dark:text-zinc-500">게재지</dt>
                  <dd className="text-zinc-700 dark:text-zinc-300 italic">{activePaper.publication}</dd>
                </div>
              )}
              {activePaper.doi && (
                <div className="contents">
                  <dt className="text-zinc-400 dark:text-zinc-500">DOI</dt>
                  <dd>
                    <a
                      href={`https://doi.org/${activePaper.doi}`}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="font-mono text-blue-600 dark:text-blue-400 hover:underline break-all"
                    >
                      {activePaper.doi}
                    </a>
                  </dd>
                </div>
              )}
              <div className="contents">
                <dt className="text-zinc-400 dark:text-zinc-500">추가일</dt>
                <dd className="text-zinc-700 dark:text-zinc-300">{formatDate(activePaper.created_at)}</dd>
              </div>
            </dl>
          </div>

          {/* Tags */}
          {activePaper.tags.length > 0 && (
            <div>
              <h4 className="text-xs font-bold uppercase tracking-wider text-zinc-400 dark:text-zinc-500 mb-2.5">태그</h4>
              <div className="flex flex-wrap gap-1.5">
                {activePaper.tags.map((t) => (
                  <span
                    key={t}
                    className="rounded-full bg-zinc-100 dark:bg-zinc-800 px-2.5 py-0.5 text-xs text-zinc-600 dark:text-zinc-400"
                  >
                    {t}
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* AI Summary Card */}
          <div>
            <div className="flex items-center gap-1.5 mb-2.5">
              <h4 className="text-xs font-bold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">AI 요약</h4>
              <span className="rounded bg-violet-100 dark:bg-violet-950/60 px-1.5 py-0.5 text-[10px] font-medium text-violet-700 dark:text-violet-300">Gemini</span>
            </div>
            <div className="rounded-xl border border-blue-100/50 dark:border-zinc-800 bg-gradient-to-br from-blue-50/30 to-indigo-50/30 dark:from-zinc-950/40 dark:to-zinc-950/20 p-4 shadow-sm">
              {activePaper.summary ? (
                <p className="text-sm leading-relaxed text-zinc-700 dark:text-zinc-300 whitespace-pre-wrap">
                  {activePaper.summary}
                </p>
              ) : (
                <p className="text-xs text-zinc-400 dark:text-zinc-500 italic font-mono">요약정보가 존재하지 않습니다.</p>
              )}
            </div>
          </div>
        </div>

        {/* Footer actions */}
        <div className="p-5 border-t border-zinc-100 dark:border-zinc-800/80 bg-zinc-50/50 dark:bg-zinc-900/50 space-y-2.5">
          <Link
            href={`/papers/${encodeURIComponent(activePaper.slug)}`}
            onClick={closePreview}
            className="flex w-full items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2.5 text-sm font-semibold text-white transition-all hover:bg-blue-700 shadow-sm hover:shadow active:scale-[0.98]"
          >
            상세 메모 보기
          </Link>

          {activePaper.zotero_key && (
            <button
              type="button"
              onClick={handleOpenZotero}
              disabled={openingZotero}
              className="flex w-full items-center justify-center gap-2 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-4 py-2.5 text-sm font-semibold text-zinc-700 dark:text-zinc-300 transition-all hover:bg-zinc-50 dark:hover:bg-zinc-800 active:scale-[0.98] disabled:opacity-50"
            >
              {openingZotero ? 'Zotero 여는 중…' : 'Zotero에서 열기'}
            </button>
          )}
          {zoteroError && (
            <p className="text-[11px] text-red-500 text-center mt-1 break-all">{zoteroError}</p>
          )}
        </div>
      </div>
    </div>
  )
}
