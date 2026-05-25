/**
 * Server component shell for `/categories/[category]`.
 *
 * Exports `generateStaticParams` so the static export emits one HTML file
 * per known category at build time (read from the search-index manifest).
 * New categories created after build won't have a static route until the
 * next rebuild — the dashboard surfaces them but direct URLs 404.
 *
 * All interactive behaviour (sort/filter/Tauri data load) lives in the
 * `CategoryView` Client Component child.
 */

import fs from 'node:fs'
import path from 'node:path'
import CategoryView from './CategoryView'

interface SearchEntry {
  category: string | null
}

export async function generateStaticParams() {
  // `output: "export"` requires at least one prerendered path per dynamic
  // segment; an empty array causes the build to fail.  When the search
  // index has no real categories yet (fresh install), we emit a single
  // placeholder route that simply renders the empty-state UI.
  const fallback = [{ category: '_placeholder' }]
  try {
    const file = path.join(process.cwd(), 'public', 'search-index.json')
    const raw = JSON.parse(fs.readFileSync(file, 'utf8')) as SearchEntry[]
    const cats = new Set<string>()
    for (const e of raw) {
      if (e.category) cats.add(e.category)
    }
    const params = Array.from(cats).map((category) => ({ category }))
    return params.length > 0 ? params : fallback
  } catch {
    return fallback
  }
}

export default async function CategoryPage({
  params,
}: {
  params: Promise<{ category: string }>
}) {
  const { category } = await params
  return <CategoryView category={decodeURIComponent(category)} />
}
