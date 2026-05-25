'use client'

/**
 * Mandatory onboarding page.
 *
 * Step 1 – Zotero PDF folder path (the `storage/` directory Zotero uses to
 *           keep linked attachments; PDFs are read directly from here).
 * Step 2 – Gemini API key (password field; stored in OS Keychain via
 *           the save_api_key Tauri command).
 *
 * The wiki content folder (where generated .md files live) is auto-resolved
 * to the Tauri AppData directory during startup and never asked for.
 *
 * "Test Connection" must succeed before "시작하기" is enabled.
 * On start: key is saved to Keychain, pdf_root is persisted to both
 * localStorage and Tauri AppState, then redirects to /.
 */

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { invoke } from '@tauri-apps/api/core'

type TestState = 'idle' | 'loading' | 'success' | 'error'

export default function OnboardingPage() {
  const router = useRouter()

  // ── Step 1: Zotero PDF folder ───────────────────────────────────────────
  const [folderPath, setFolderPath] = useState('')

  // ── Step 2: Gemini API key ──────────────────────────────────────────────
  const [apiKey, setApiKey] = useState('')
  const [showKey, setShowKey] = useState(false)
  const [testState, setTestState] = useState<TestState>('idle')
  const [testError, setTestError] = useState('')

  // ── Submit ──────────────────────────────────────────────────────────────
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState('')

  useEffect(() => {
    ;(async () => {
      const savedPdf =
        (await invoke<string | null>('get_pdf_root').catch(() => null)) ??
        localStorage.getItem('zotero-pdf-root')
      const k = await invoke<string>('get_api_key').catch(() => null)
      if (savedPdf) setFolderPath(savedPdf)
      if (k)        setApiKey(k)
    })()
  }, [])

  const folderValid = folderPath.trim().length > 0
  const keyValid    = apiKey.trim().length > 0
  const canStart    = folderValid && keyValid && testState === 'success'

  // ── Handlers ────────────────────────────────────────────────────────────

  // Reset test when key changes.
  const handleKeyChange = (v: string) => {
    setApiKey(v)
    setTestState('idle')
    setTestError('')
  }

  const handleTestConnection = async () => {
    if (!keyValid) return
    setTestState('loading')
    setTestError('')
    const trimmed = apiKey.trim()
    try {
      // Validate the typed key against Gemini (does not require keychain).
      await invoke<boolean>('test_connection', { apiKey: trimmed })
      try {
        await invoke('save_api_key', { key: trimmed })
        setTestState('success')
      } catch (saveErr) {
        setTestState('error')
        setTestError(
          `Gemini 연결은 성공했지만 OS 키체인 저장에 실패했습니다: ${saveErr}`,
        )
      }
    } catch (err) {
      setTestState('error')
      setTestError(String(err))
    }
  }

  const handleStart = async () => {
    if (!canStart) return
    setSaving(true)
    setSaveError('')
    try {
      await invoke('save_api_key', { key: apiKey.trim() })
      await invoke('set_pdf_root', { path: folderPath.trim() })
      localStorage.setItem('zotero-pdf-root', folderPath.trim())
      router.replace('/')
    } catch (err) {
      setSaveError(String(err))
      setSaving(false)
    }
  }

  // ── Render ───────────────────────────────────────────────────────────────
  return (
    <div className="min-h-screen flex items-center justify-center bg-zinc-50 dark:bg-zinc-950 p-6">
      <div className="w-full max-w-md">

        {/* Header */}
        <div className="text-center mb-8">
          <div className="inline-flex h-12 w-12 items-center justify-center rounded-2xl bg-blue-600 text-white text-xl font-bold mb-4">
            W
          </div>
          <h1 className="text-2xl font-bold text-zinc-900 dark:text-zinc-50 mb-1">
            LLM Wiki 설정
          </h1>
          <p className="text-sm text-zinc-500">
            시작하기 전에 두 가지를 설정해주세요.
          </p>
        </div>

        <div className="space-y-4">

          {/* ── Step 1: Zotero PDF folder ─────────────────────────────────── */}
          <section className="rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 p-5">
            <div className="flex items-center gap-2.5 mb-3">
              <StepBadge n={1} done={folderValid} />
              <h2 className="font-semibold text-zinc-900 dark:text-zinc-100">
                Zotero PDF 폴더
              </h2>
            </div>

            <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-3 leading-relaxed">
              논문 PDF가 저장된 Zotero{' '}
              <code className="rounded bg-zinc-100 dark:bg-zinc-800 px-1 py-0.5 font-mono">
                storage/
              </code>{' '}
              폴더의 <strong>절대 경로</strong>를 입력하세요. Gemini가 PDF를 읽어
              마크다운 위키 글을 자동 생성합니다.
            </p>

            <input
              type="text"
              value={folderPath}
              onChange={e => setFolderPath(e.target.value)}
              placeholder="C:\Users\name\Zotero\storage"
              spellCheck={false}
              className="w-full rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 font-mono text-sm text-zinc-900 dark:text-zinc-100 placeholder:text-zinc-400 focus:outline-none focus:ring-2 focus:ring-blue-500 dark:focus:ring-blue-400"
            />

            {folderValid && (
              <p className="mt-1.5 text-xs text-zinc-400 font-mono truncate">
                → 이 폴더의 .pdf 파일들을 자동 스캔합니다
              </p>
            )}
          </section>

          {/* ── Step 2: Gemini API key ────────────────────────────────────── */}
          <section className="rounded-xl border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 p-5">
            <div className="flex items-center gap-2.5 mb-3">
              <StepBadge n={2} done={testState === 'success'} />
              <h2 className="font-semibold text-zinc-900 dark:text-zinc-100">
                Gemini API 키
              </h2>
            </div>

            <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-3 leading-relaxed">
              <a
                href="https://aistudio.google.com/apikey"
                target="_blank"
                rel="noopener noreferrer"
                className="text-blue-500 hover:underline"
              >
                aistudio.google.com/apikey
              </a>
              에서 키를 발급하세요. OS 키체인에 저장됩니다.
              <br />
              <span className="text-zinc-400">
                자주 쓸 예정이면{' '}
                <a
                  href="https://aistudio.google.com/usage"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-blue-500 hover:underline"
                >
                  AI Studio → Billing
                </a>
                에 결제 카드 연결 → Tier 1 (2.5 Pro 5→150 RPM, 25→1,000 RPD).
              </span>
            </p>

            {/* Password input */}
            <div className="relative">
              <input
                type={showKey ? 'text' : 'password'}
                value={apiKey}
                onChange={e => handleKeyChange(e.target.value)}
                placeholder="AIza…"
                autoComplete="off"
                className="w-full rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 pr-14 font-mono text-sm text-zinc-900 dark:text-zinc-100 placeholder:text-zinc-400 focus:outline-none focus:ring-2 focus:ring-blue-500 dark:focus:ring-blue-400"
              />
              <button
                type="button"
                onClick={() => setShowKey(s => !s)}
                className="absolute right-2 top-1/2 -translate-y-1/2 rounded px-1.5 py-0.5 text-xs text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200"
              >
                {showKey ? '숨김' : '표시'}
              </button>
            </div>

            {/* Test connection row */}
            <div className="mt-3 flex items-center gap-3 flex-wrap">
              <button
                type="button"
                onClick={handleTestConnection}
                disabled={!keyValid || testState === 'loading'}
                className="rounded-lg bg-zinc-800 dark:bg-zinc-700 px-3.5 py-1.5 text-xs font-medium text-white transition-colors hover:bg-zinc-700 dark:hover:bg-zinc-600 disabled:cursor-not-allowed disabled:opacity-40"
              >
                {testState === 'loading' ? (
                  <span className="flex items-center gap-1.5">
                    <Spinner /> 연결 중…
                  </span>
                ) : '연결 테스트'}
              </button>

              {testState === 'success' && (
                <span className="text-xs font-medium text-green-600 dark:text-green-400">
                  ✓ 연결 성공
                </span>
              )}
              {testState === 'error' && (
                <span className="text-xs font-medium text-red-500">
                  ✗ 연결 실패
                </span>
              )}
            </div>

            {testState === 'error' && testError && (
              <p className="mt-2 text-xs text-red-500 break-all leading-relaxed">
                {testError}
              </p>
            )}
          </section>

          {/* ── Start button ──────────────────────────────────────────────── */}
          <button
            type="button"
            onClick={handleStart}
            disabled={!canStart || saving}
            className="w-full rounded-xl bg-blue-600 py-3 text-sm font-semibold text-white transition-colors hover:bg-blue-700 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {saving ? (
              <span className="flex items-center justify-center gap-2">
                <Spinner /> 시작 중…
              </span>
            ) : '시작하기 →'}
          </button>

          {/* Hint text */}
          {!canStart && !saving && (
            <p className="text-center text-xs text-zinc-400">
              {!folderValid && !keyValid
                ? 'Zotero PDF 폴더와 API 키를 입력하세요'
                : !folderValid
                  ? 'Zotero PDF 폴더 경로를 입력하세요'
                  : !keyValid
                    ? 'API 키를 입력하세요'
                    : '연결 테스트를 먼저 완료하세요'}
            </p>
          )}

          {saveError && (
            <p className="text-center text-xs text-red-500 break-all">{saveError}</p>
          )}
        </div>
      </div>
    </div>
  )
}

// ── Small helpers ─────────────────────────────────────────────────────────────

function StepBadge({ n, done }: { n: number; done: boolean }) {
  return (
    <span
      className={`flex h-6 w-6 flex-shrink-0 items-center justify-center rounded-full text-xs font-bold ${
        done
          ? 'bg-green-500 text-white'
          : 'bg-blue-600 text-white'
      }`}
    >
      {done ? '✓' : n}
    </span>
  )
}

function Spinner() {
  return (
    <span className="inline-block h-3 w-3 rounded-full border-2 border-current border-t-transparent animate-spin" />
  )
}
