'use client'

/**
 * ZoteroStatusBar — fixed top-bar that shows live Zotero connectivity.
 *
 * Listens to the "zotero-status" event emitted by the Rust watcher
 * (polls every 30 s).  Also subscribes to "pending-sync-complete" to
 * refresh the pending-item count after a sync run.
 *
 * Hidden on /onboarding.
 *
 * Display rules:
 *   Connected                  → green  "● Zotero 연결됨"
 *   Disconnected, N pending    → yellow "● Zotero 꺼짐 (N개 동기화 대기)"
 *   Disconnected, 0 pending    → yellow "● Zotero 꺼짐"
 *   Connected   + N pending    → green  "● Zotero 연결됨"
 *                                + blue  "지금 동기화" button
 */

import { useEffect, useState, useCallback } from 'react'
import { usePathname } from 'next/navigation'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'

// ── Types ─────────────────────────────────────────────────────────────────────

type ZoteroStatusPayload =
  | { status: 'Connected' }
  | { status: 'Disconnected' }
  | { status: 'Error'; error: string }

interface PendingSyncItem {
  paper_file: string
  zotero_item_key: string
  target_collection: string
  queued_at: string
}

// ── Component ─────────────────────────────────────────────────────────────────

export default function ZoteroStatusBar() {
  const pathname = usePathname()

  const [status, setStatus]           = useState<ZoteroStatusPayload>({ status: 'Disconnected' })
  const [pendingCount, setPendingCount] = useState(0)
  const [syncing, setSyncing]         = useState(false)

  // ── Refresh pending count ─────────────────────────────────────────────────
  const refreshPending = useCallback(async () => {
    const root = localStorage.getItem('content-root')
    if (!root) return
    const queuePath = `${root}/meta/pending-zotero-sync.json`
    try {
      const items = await invoke<PendingSyncItem[]>('load_queue', { queuePath })
      setPendingCount(items.length)
    } catch {
      setPendingCount(0)
    }
  }, [])

  // ── Event subscriptions ───────────────────────────────────────────────────
  useEffect(() => {
    let unlistenStatus: (() => void) | undefined
    let unlistenComplete: (() => void) | undefined

    // Async IIFE: all setState / invoke calls happen after at least one await,
    // keeping the synchronous effect body free of state mutations
    // (satisfies react-hooks/set-state-in-effect).
    ;(async () => {
      // Initial probe so the bar shows correct state before the first watcher tick.
      try {
        const s = await invoke<ZoteroStatusPayload>('check_status')
        setStatus(s)
      } catch {}
      await refreshPending()

      unlistenStatus = await listen<ZoteroStatusPayload>('zotero-status', e => {
        setStatus(e.payload)
        // Refresh count on every status change (reconnect may have triggered a sync).
        refreshPending()
      })

      unlistenComplete = await listen<unknown>('pending-sync-complete', () => {
        refreshPending()
      })
    })()

    return () => {
      unlistenStatus?.()
      unlistenComplete?.()
    }
  }, [refreshPending])

  // ── Sync handler ──────────────────────────────────────────────────────────
  const handleSync = async () => {
    const root = localStorage.getItem('content-root')
    if (!root || syncing) return
    setSyncing(true)
    try {
      await invoke('sync_all', {
        queuePath: `${root}/meta/pending-zotero-sync.json`,
      })
    } catch {
      // sync_all emits events for errors; command-level failure is rare.
    } finally {
      setSyncing(false)
      refreshPending()
    }
  }

  // ── Hidden on onboarding ──────────────────────────────────────────────────
  if (pathname === '/onboarding') return null

  // ── Derived display values ────────────────────────────────────────────────
  const isConnected = status.status === 'Connected'

  const dotClass = isConnected
    ? 'text-green-500 dark:text-green-400'
    : 'text-yellow-500 dark:text-yellow-400'

  const label = isConnected
    ? 'Zotero 연결됨'
    : pendingCount > 0
      ? `Zotero 꺼짐 (${pendingCount}개 동기화 대기)`
      : 'Zotero 꺼짐'

  const showSyncButton = isConnected && pendingCount > 0

  // ── Render ────────────────────────────────────────────────────────────────
  return (
    <div className="flex items-center gap-3 px-4 py-1.5 text-sm border-b border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-950 select-none">

      {/* Status indicator */}
      <span className={`flex items-center gap-1.5 text-xs font-medium ${dotClass}`}>
        <span aria-hidden="true">●</span>
        {label}
      </span>

      {/* Error detail (collapsed by default) */}
      {status.status === 'Error' && (
        <span className="text-xs text-red-500 truncate max-w-xs" title={status.error}>
          ({status.error})
        </span>
      )}

      {/* Manual sync button */}
      {showSyncButton && (
        <button
          type="button"
          onClick={handleSync}
          disabled={syncing}
          className="ml-1 rounded bg-blue-600 px-2.5 py-0.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {syncing ? (
            <span className="flex items-center gap-1">
              <span className="inline-block h-2.5 w-2.5 rounded-full border-2 border-white border-t-transparent animate-spin" />
              동기화 중…
            </span>
          ) : '지금 동기화'}
        </button>
      )}
    </div>
  )
}
