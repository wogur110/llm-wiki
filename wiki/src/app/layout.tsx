/**
 * Root layout — Server Component.
 *
 * Exports `metadata` (only works in Server Components).
 * All interactive logic (auth guard, Zotero status, progress overlay)
 * lives in Client Component children so this file stays static.
 *
 * Auth flow:
 *   AuthGuard checks `has_api_key` + localStorage['content-root'] on
 *   every Client-side navigation.  Missing either → redirect /onboarding.
 */

import type { Metadata } from 'next'
import { Geist, Geist_Mono } from 'next/font/google'
import './globals.css'
// KaTeX stylesheet — imported once so every <span class="katex">…</span>
// emitted by rehype-katex is styled correctly across the app.
import 'katex/dist/katex.min.css'

import AuthGuard       from '@/components/AuthGuard'
import ZoteroStatusBar from '@/components/ZoteroStatusBar'
import OrganizeProgress from '@/components/OrganizeProgress'
import Header          from '@/components/Header'

const geistSans = Geist({
  variable: '--font-geist-sans',
  subsets: ['latin'],
})

const geistMono = Geist_Mono({
  variable: '--font-geist-mono',
  subsets: ['latin'],
})

export const metadata: Metadata = {
  title: 'LLM Wiki',
  description: '연구 논문 위키 — AI 분류 + Zotero 연동',
}

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html
      lang="ko"
      className={`${geistSans.variable} ${geistMono.variable} h-full antialiased`}
    >
      <body className="min-h-full flex flex-col bg-background text-foreground">
        {/*
          AuthGuard: redirects to /onboarding when key or content-root is
          missing; also restores Tauri AppState.content_root after reloads.

          ZoteroStatusBar + OrganizeProgress are Client Components hidden
          on /onboarding (they check pathname internally).
        */}
        <AuthGuard>
          <ZoteroStatusBar />
          <Header />
          <main className="flex-1">
            {children}
          </main>
          <OrganizeProgress />
        </AuthGuard>
      </body>
    </html>
  )
}
