'use client'

/**
 * Settings — `/settings`
 *
 * Three sections:
 *   1. Gemini API key      (re-validates on save via `test_connection`)
 *   2. Zotero PDF folder   (where the PDF importer reads from)
 *   3. Zotero connection status + manual sync trigger
 *
 * The wiki content folder is auto-managed under Tauri's AppData and is not
 * editable from here.
 */

import { useCallback, useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

type SaveState = 'idle' | 'saving' | 'success' | 'error'

interface PendingSyncItem {
  paper_file: string
  zotero_item_key: string
  target_collection: string
  queued_at: string
}

type ZoteroStatus =
  | { status: 'Connected' }
  | { status: 'Disconnected' }
  | { status: 'Error'; error: string }

export default function SettingsPage() {
  const router = useRouter()

  // ── API key section ─────────────────────────────────────────────────────
  const [apiKey, setApiKey] = useState('')
  const [showKey, setShowKey] = useState(false)
  const [keyState, setKeyState] = useState<SaveState>('idle')
  const [keyError, setKeyError] = useState('')

  // ── Zotero PDF folder section ───────────────────────────────────────────
  const [folderPath, setFolderPath] = useState('')
  const [folderState, setFolderState] = useState<SaveState>('idle')
  const [folderError, setFolderError] = useState('')

  // ── Zotero section ──────────────────────────────────────────────────────
  const [zStatus, setZStatus] = useState<ZoteroStatus>({ status: 'Disconnected' })
  const [pending, setPending] = useState<PendingSyncItem[]>([])
  const [syncing, setSyncing] = useState(false)

  // Declared before the effects that use it to avoid a TDZ access.
  const refreshPending = useCallback(async (root: string) => {
    if (!root) {
      await Promise.resolve()
      setPending([])
      return
    }
    try {
      const items = await invoke<PendingSyncItem[]>('load_queue', {
        queuePath: `${root}/meta/pending-zotero-sync.json`,
      })
      setPending(items)
    } catch {
      setPending([])
    }
  }, [])

  // ── Initial load ────────────────────────────────────────────────────────
  useEffect(() => {
    let cancelled = false
    ;(async () => {
      const k = await invoke<string>('get_api_key').catch(() => '')
      const pdfRoot =
        (await invoke<string | null>('get_pdf_root').catch(() => null)) ??
        window.localStorage.getItem('zotero-pdf-root') ??
        ''
      if (cancelled) return
      setApiKey(k)
      setFolderPath(pdfRoot)

      const s = await invoke<ZoteroStatus>('check_status').catch(
        () => ({ status: 'Disconnected' }) as ZoteroStatus,
      )
      if (cancelled) return
      setZStatus(s)
      await refreshPending(window.localStorage.getItem('content-root') ?? '')
    })()
    return () => {
      cancelled = true
    }
  }, [refreshPending])

  // ── Live Zotero status / pending count ──────────────────────────────────
  useEffect(() => {
    let unlistenStatus: (() => void) | undefined
    let unlistenDone: (() => void) | undefined
    ;(async () => {
      unlistenStatus = await listen<ZoteroStatus>('zotero-status', (e) => {
        setZStatus(e.payload)
      })
      unlistenDone = await listen<unknown>('pending-sync-complete', () => {
        refreshPending(window.localStorage.getItem('content-root') ?? '')
      })
    })()
    return () => {
      unlistenStatus?.()
      unlistenDone?.()
    }
  }, [refreshPending])

  // ── Handlers ────────────────────────────────────────────────────────────

  const handleSaveKey = async () => {
    const trimmed = apiKey.trim()
    if (!trimmed) {
      setKeyState('error')
      setKeyError('API 키를 입력하세요.')
      return
    }
    setKeyState('saving')
    setKeyError('')
    try {
      await invoke<boolean>('test_connection', { apiKey: trimmed })
      await invoke('save_api_key', { key: trimmed })
      setKeyState('success')
      window.setTimeout(() => setKeyState('idle'), 2500)
    } catch (e) {
      setKeyState('error')
      setKeyError(String(e))
    }
  }

  const handleSaveFolder = async () => {
    const trimmed = folderPath.trim()
    if (!trimmed) {
      setFolderState('error')
      setFolderError('폴더 경로를 입력하세요.')
      return
    }
    setFolderState('saving')
    setFolderError('')
    try {
      await invoke('set_pdf_root', { path: trimmed })
      window.localStorage.setItem('zotero-pdf-root', trimmed)
      setFolderState('success')
      window.setTimeout(() => setFolderState('idle'), 2500)
    } catch (e) {
      setFolderState('error')
      setFolderError(String(e))
    }
  }

  const handleManualSync = async () => {
    const root = window.localStorage.getItem('content-root')
    if (!root || syncing) return
    setSyncing(true)
    try {
      await invoke('sync_all', {
        queuePath: `${root}/meta/pending-zotero-sync.json`,
      })
    } catch {
      // sync_all emits its own error events
    } finally {
      setSyncing(false)
      refreshPending(root)
    }
  }

  const handleReset = async () => {
    if (!window.confirm('API 키와 Zotero PDF 폴더 설정을 모두 지울까요?')) return
    try {
      await invoke('delete_api_key').catch(() => {})
    } catch {}
    window.localStorage.removeItem('zotero-pdf-root')
    window.localStorage.removeItem('content-root')
    router.replace('/onboarding')
  }

  // ── Render ──────────────────────────────────────────────────────────────
  return (
    <div className="mx-auto max-w-3xl px-6 py-8 space-y-6">
      <header>
        <h1 className="text-2xl font-bold text-zinc-900 dark:text-zinc-50">
          설정
        </h1>
        <p className="mt-1 text-sm text-zinc-500">
          API 키와 콘텐츠 폴더 경로, 연동 상태를 관리합니다.
        </p>
      </header>

      {/* ── Gemini API key ──────────────────────────────────────────── */}
      <Card title="Gemini API 키" subtitle="OS 키체인에 저장 후 즉시 재검증합니다.">
        <div className="relative">
          <input
            type={showKey ? 'text' : 'password'}
            value={apiKey}
            onChange={(e) => {
              setApiKey(e.target.value)
              setKeyState('idle')
              setKeyError('')
            }}
            placeholder="AIza…"
            autoComplete="off"
            spellCheck={false}
            className="w-full rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 pr-14 font-mono text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/40"
          />
          <button
            type="button"
            onClick={() => setShowKey((s) => !s)}
            className="absolute right-2 top-1/2 -translate-y-1/2 rounded px-1.5 py-0.5 text-xs text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200"
          >
            {showKey ? '숨김' : '표시'}
          </button>
        </div>
        <div className="mt-3 flex items-center gap-3 flex-wrap">
          <button
            type="button"
            onClick={handleSaveKey}
            disabled={keyState === 'saving' || !apiKey.trim()}
            className="rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
          >
            {keyState === 'saving' ? '검증 중…' : '저장 + 검증'}
          </button>
          {keyState === 'success' && (
            <span className="text-xs font-medium text-green-600 dark:text-green-400">
              ✓ 저장됨
            </span>
          )}
          {keyState === 'error' && (
            <span className="text-xs font-medium text-red-500 break-all">
              ✗ {keyError}
            </span>
          )}
        </div>
      </Card>

      {/* ── Zotero PDF folder ───────────────────────────────────────── */}
      <Card
        title="Zotero PDF 폴더"
        subtitle="PDF 가져오기가 스캔할 Zotero storage/ 폴더 경로."
      >
        <input
          type="text"
          value={folderPath}
          onChange={(e) => {
            setFolderPath(e.target.value)
            setFolderState('idle')
            setFolderError('')
          }}
          placeholder="C:\Users\name\Zotero\storage"
          spellCheck={false}
          className="w-full rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 font-mono text-sm focus:outline-none focus:ring-2 focus:ring-blue-500/40"
        />
        <div className="mt-3 flex items-center gap-3 flex-wrap">
          <button
            type="button"
            onClick={handleSaveFolder}
            disabled={folderState === 'saving' || !folderPath.trim()}
            className="rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
          >
            {folderState === 'saving' ? '저장 중…' : '저장'}
          </button>
          {folderState === 'success' && (
            <span className="text-xs font-medium text-green-600 dark:text-green-400">
              ✓ 저장됨
            </span>
          )}
          {folderState === 'error' && (
            <span className="text-xs font-medium text-red-500 break-all">
              ✗ {folderError}
            </span>
          )}
        </div>
      </Card>

      {/* ── Zotero ─────────────────────────────────────────────────── */}
      <Card
        title="Zotero 연동"
        subtitle="localhost:23119 ZotMoov 연결 상태와 동기화 대기 큐."
      >
        <div className="flex items-center gap-3">
          <span
            className={`flex items-center gap-1.5 text-sm font-medium ${
              zStatus.status === 'Connected'
                ? 'text-green-600 dark:text-green-400'
                : 'text-yellow-600 dark:text-yellow-400'
            }`}
          >
            <span aria-hidden="true">●</span>
            {zStatus.status === 'Connected'
              ? '연결됨'
              : zStatus.status === 'Error'
                ? '오류'
                : '꺼짐'}
          </span>
          {zStatus.status === 'Error' && (
            <span className="text-xs text-red-500 truncate" title={zStatus.error}>
              ({zStatus.error})
            </span>
          )}
          <span className="ml-auto text-xs text-zinc-500">
            대기 {pending.length}개
          </span>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={handleManualSync}
            disabled={
              syncing || pending.length === 0 || zStatus.status !== 'Connected'
            }
            className="rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-blue-700 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {syncing ? (
              <span className="flex items-center gap-1.5">
                <span className="inline-block h-2.5 w-2.5 rounded-full border-2 border-white border-t-transparent animate-spin" />
                동기화 중…
              </span>
            ) : (
              '지금 동기화'
            )}
          </button>
          {pending.length > 0 && zStatus.status !== 'Connected' && (
            <span className="text-xs text-zinc-500">
              Zotero에 다시 연결되면 자동 동기화됩니다.
            </span>
          )}
        </div>

        {pending.length > 0 && (
          <ul className="mt-3 space-y-1.5 max-h-56 overflow-y-auto rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-950 p-2.5 text-xs">
            {pending.map((p) => (
              <li
                key={p.zotero_item_key}
                className="flex items-baseline justify-between gap-2"
              >
                <span className="truncate font-mono text-zinc-700 dark:text-zinc-300">
                  {p.paper_file}
                </span>
                <span className="shrink-0 rounded bg-zinc-200 dark:bg-zinc-800 px-1.5 py-0.5 text-[10px] text-zinc-600 dark:text-zinc-400">
                  → {p.target_collection}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>

      {/* ── Reset ──────────────────────────────────────────────────── */}
      <Card title="초기화" subtitle="설정을 모두 지우고 온보딩으로 돌아갑니다.">
        <button
          type="button"
          onClick={handleReset}
          className="rounded-lg border border-red-300 dark:border-red-800 bg-white dark:bg-zinc-900 px-3 py-1.5 text-xs font-medium text-red-600 dark:text-red-400 transition-colors hover:bg-red-50 dark:hover:bg-red-950/30"
        >
          모두 초기화
        </button>
      </Card>
    </div>
  )
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function Card({
  title,
  subtitle,
  children,
}: {
  title: string
  subtitle?: string
  children: React.ReactNode
}) {
  return (
    <section className="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-5">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
        {title}
      </h2>
      {subtitle && <p className="mt-1 text-xs text-zinc-500">{subtitle}</p>}
      <div className="mt-4">{children}</div>
    </section>
  )
}
