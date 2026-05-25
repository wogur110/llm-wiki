'use client'

/**
 * OrganizeProgress — floating widget that shows the live pipeline status
 * for the currently-processing paper.
 *
 * Appears automatically when the first "tx-progress" event is received
 * and stays visible until the user dismisses it after completion.
 *
 * Resets automatically when a new paper starts (MovedToStaging started).
 *
 * Step order mirrors the Rust pipeline:
 *   1. MovedToStaging
 *   2. GeminiClassified
 *   3. MovedToTarget
 *   4. ZoteroCollectionChanged
 *   5. ZotMovConfirmed
 */

import { useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'

// ── Types ─────────────────────────────────────────────────────────────────────

type StepStatus = 'started' | 'done' | 'skipped' | 'failed'

interface TxProgressPayload {
  step: string
  status: StepStatus
  detail?: string
}

interface StepState {
  status: StepStatus
  detail?: string
}

// ── Constants ─────────────────────────────────────────────────────────────────

const STEP_KEYS = [
  'MovedToStaging',
  'GeminiClassified',
  'MovedToTarget',
  'ZoteroCollectionChanged',
  'ZotMovConfirmed',
] as const

type StepKey = (typeof STEP_KEYS)[number]

const STEP_LABELS: Record<StepKey, string> = {
  MovedToStaging:           '스테이징으로 이동',
  GeminiClassified:         'AI 분류',
  MovedToTarget:            '카테고리로 이동',
  ZoteroCollectionChanged:  'Zotero 컬렉션 업데이트',
  ZotMovConfirmed:          'PDF 이동 확인 (ZotMoov)',
}

// ── Component ─────────────────────────────────────────────────────────────────

export default function OrganizeProgress() {
  const [visible, setVisible]   = useState(false)
  const [steps, setSteps]       = useState<Partial<Record<StepKey, StepState>>>({})
  const [hasFailed, setHasFailed] = useState(false)

  useEffect(() => {
    let unlisten: (() => void) | undefined

    listen<TxProgressPayload>('tx-progress', e => {
      const { step, status, detail } = e.payload

      // A new paper starting — reset everything.
      if (step === 'MovedToStaging' && status === 'started') {
        setSteps({})
        setHasFailed(false)
        setVisible(true)
      }

      if (status === 'failed') setHasFailed(true)

      setSteps(prev => ({
        ...prev,
        [step]: { status, detail },
      }))
    }).then(fn => { unlisten = fn })

    return () => { unlisten?.() }
  }, [])

  if (!visible) return null

  // Determine overall completion state.
  const allSettled = STEP_KEYS.every(k => {
    const s = steps[k]?.status
    return s === 'done' || s === 'skipped' || s === 'failed'
  })

  const handleDismiss = () => {
    setVisible(false)
    setSteps({})
    setHasFailed(false)
  }

  // ── Render ────────────────────────────────────────────────────────────────
  return (
    <div
      role="status"
      aria-live="polite"
      className="fixed bottom-4 right-4 z-50 w-80 rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 shadow-xl shadow-zinc-900/10"
    >
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-zinc-100 dark:border-zinc-800">
        <span className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
          논문 정리 중
        </span>
        {allSettled && (
          <button
            type="button"
            onClick={handleDismiss}
            className="text-xs text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 px-1"
          >
            닫기
          </button>
        )}
      </div>

      {/* Step list */}
      <ol className="px-4 py-3 space-y-2.5">
        {STEP_KEYS.map(key => {
          const state = steps[key]
          return (
            <li key={key} className="flex items-start gap-2.5">
              <StepIcon status={state?.status} />
              <div className="flex-1 min-w-0 pt-px">
                <p className={`text-xs leading-tight ${
                  state ? 'text-zinc-800 dark:text-zinc-200' : 'text-zinc-400 dark:text-zinc-500'
                }`}>
                  {STEP_LABELS[key]}
                </p>
                {state?.detail && (
                  <p className="mt-0.5 text-xs text-zinc-500 dark:text-zinc-400 truncate" title={state.detail}>
                    {state.detail}
                  </p>
                )}
              </div>
            </li>
          )
        })}
      </ol>

      {/* Failure notice */}
      {hasFailed && (
        <div className="mx-4 mb-3 rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/50 px-3 py-2">
          <p className="text-xs text-red-700 dark:text-red-300 leading-relaxed">
            오류가 발생했습니다.
            파일이 <code className="font-mono">unclassified/</code>로 복구되었습니다.
          </p>
        </div>
      )}

      {/* Success state */}
      {allSettled && !hasFailed && (
        <div className="mx-4 mb-3 rounded-lg border border-green-200 dark:border-green-800 bg-green-50 dark:bg-green-950/50 px-3 py-2">
          <p className="text-xs text-green-700 dark:text-green-300">
            논문 정리가 완료되었습니다. ✓
          </p>
        </div>
      )}
    </div>
  )
}

// ── Step icon ─────────────────────────────────────────────────────────────────

function StepIcon({ status }: { status?: StepStatus }) {
  const base = 'mt-0.5 flex-shrink-0 w-4 h-4 flex items-center justify-center text-xs'

  if (!status) {
    return (
      <span className={`${base} rounded-full border border-zinc-300 dark:border-zinc-600`} />
    )
  }

  switch (status) {
    case 'started':
      return (
        <span
          className={`${base} rounded-full border-2 border-blue-500 border-t-transparent animate-spin`}
          aria-label="처리 중"
        />
      )
    case 'done':
      return (
        <span className={`${base} text-green-500`} aria-label="완료">✓</span>
      )
    case 'skipped':
      return (
        <span className={`${base} text-zinc-400`} aria-label="건너뜀">—</span>
      )
    case 'failed':
      return (
        <span className={`${base} text-red-500`} aria-label="실패">✗</span>
      )
  }
}
