'use client'

/**
 * AuthGuard — runs on every Client-side navigation.
 *
 * Checks two conditions:
 *   1. Gemini API key exists in OS Keychain  (has_api_key command)
 *   2. content-root path exists in localStorage
 *
 * If either is missing → redirect to /onboarding.
 * If both are present  → restore content_root into Rust AppState
 *                        (AppState is in-memory; re-setting it here
 *                        ensures correctness after hot-reload or restart).
 *
 * Children are not rendered until the check passes to avoid a
 * brief flash of the unauthorised page.
 */

import { useEffect, useState } from 'react'
import { usePathname, useRouter } from 'next/navigation'
import { invoke } from '@tauri-apps/api/core'

export default function AuthGuard({ children }: { children: React.ReactNode }) {
  const router   = useRouter()
  const pathname = usePathname()
  const [ready, setReady] = useState(false)

  useEffect(() => {
    let cancelled = false

    // Async IIFE: all setState / router calls happen after at least one
    // await, keeping the synchronous effect body free of state mutations
    // (satisfies react-hooks/set-state-in-effect).
    ;(async () => {
      if (pathname === '/onboarding') {
        // Yield to the microtask queue so setReady is not synchronous.
        await Promise.resolve()
        if (!cancelled) setReady(true)
        return
      }

      try {
        const hasKey = await invoke<boolean>('has_api_key')
        const root   = localStorage.getItem('content-root')

        if (!hasKey || !root) {
          if (!cancelled) router.replace('/onboarding')
          return
        }

        // Restore Rust AppState.content_root on every page load — AppState is
        // in-memory and is lost when the webview reloads (dev hot-reload, etc.).
        await invoke('set_content_root', { path: root }).catch(() => {})

        if (!cancelled) setReady(true)
      } catch {
        if (!cancelled) router.replace('/onboarding')
      }
    })()

    return () => { cancelled = true }
  // Re-run when the route changes so every navigation is guarded.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname])

  if (!ready) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="text-sm text-zinc-400 dark:text-zinc-500">불러오는 중…</div>
      </div>
    )
  }

  return <>{children}</>
}
