'use client'

/**
 * Header — site-wide nav strip shown on every page except `/onboarding`.
 *
 * Contains the logo/home link, primary nav, the global SearchBar
 * (Cmd/Ctrl+K), and the settings link.
 */

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import SearchBar from './SearchBar'

interface NavItem {
  href: string
  label: string
  match?: (pathname: string) => boolean
}

const NAV: NavItem[] = [
  { href: '/', label: '대시보드', match: (p) => p === '/' },
  {
    href: '/settings',
    label: '설정',
    match: (p) => p.startsWith('/settings'),
  },
]

export default function Header() {
  const pathname = usePathname()
  if (pathname === '/onboarding') return null

  return (
    <header className="border-b border-zinc-200 dark:border-zinc-800 bg-white/80 dark:bg-zinc-950/80 backdrop-blur sticky top-0 z-40">
      <div className="mx-auto flex max-w-6xl items-center gap-4 px-6 py-3">
        {/* Logo */}
        <Link
          href="/"
          className="flex items-center gap-2 text-sm font-semibold text-zinc-900 dark:text-zinc-100 hover:opacity-80 transition-opacity"
        >
          <span className="flex h-7 w-7 items-center justify-center rounded-lg bg-blue-600 text-white text-xs font-bold">
            W
          </span>
          <span className="hidden sm:inline">LLM Wiki</span>
        </Link>

        {/* Primary nav */}
        <nav className="hidden md:flex items-center gap-1 ml-2">
          {NAV.map((item) => {
            const active = item.match?.(pathname) ?? pathname === item.href
            return (
              <Link
                key={item.href}
                href={item.href}
                className={`rounded-md px-2.5 py-1 text-sm transition-colors ${
                  active
                    ? 'bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 font-medium'
                    : 'text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100'
                }`}
              >
                {item.label}
              </Link>
            )
          })}
        </nav>

        {/* Search — flex-grow takes the remaining width */}
        <div className="ml-auto flex-1 max-w-md">
          <SearchBar />
        </div>
      </div>
    </header>
  )
}
