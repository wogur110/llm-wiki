'use client'

/**
 * AuthGuard — runs on every Client-side navigation.
 *
 * The Zotero-driven PDF importer means the user only needs **one** thing
 * before the app is usable:
 *   * Gemini API key stored in OS Keychain (checked via `has_api_key`).
 *
 * The wiki content folder is auto-resolved to `<AppData>/content` during
 * Tauri `setup()` and cached into `localStorage['content-root']` here so
 * `lib/content.ts` (which still reads from localStorage) keeps working.
 *
 * If the key is missing → redirect to /onboarding.
 * Children are not rendered until the check passes to avoid a brief flash
 * of the unauthorised page.
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

    ;(async () => {
      if (pathname === '/onboarding') {
        await Promise.resolve()
        if (!cancelled) setReady(true)
        return
      }

      try {
        const hasKey = await invoke<boolean>('has_api_key')

        if (!hasKey) {
          if (!cancelled) router.replace('/onboarding')
          return
        }

        // Cache the auto-resolved AppData content root so content.ts (which
        // reads from localStorage) can keep working unchanged.
        const contentRoot = await invoke<string | null>('get_content_root')
          .catch(() => null)
        if (contentRoot) {
          localStorage.setItem('content-root', contentRoot)
        }

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
