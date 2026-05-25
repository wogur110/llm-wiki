/**
 * Server component shell for `/papers/[slug]`.
 *
 * Enumerates known slugs at build time from `public/search-index.json` so the
 * static export can ship a route per paper.  The interactive view —
 * two-column layout with the markdown body and the sidebar (Zotero
 * launcher, Ask Gemini, backlinks, related) — lives in `PaperView`.
 */

import fs from 'node:fs'
import path from 'node:path'
import PaperView from './PaperView'

interface SearchEntry {
  slug: string
}

export async function generateStaticParams() {
  // Same constraint as the category route: an empty array breaks the
  // static export build, so emit a `_placeholder` slug when there are no
  // papers yet.  The PaperView handles the missing-paper case gracefully.
  const fallback = [{ slug: '_placeholder' }]
  try {
    const file = path.join(process.cwd(), 'public', 'search-index.json')
    const raw = JSON.parse(fs.readFileSync(file, 'utf8')) as SearchEntry[]
    const params = raw.map((e) => ({ slug: e.slug }))
    return params.length > 0 ? params : fallback
  } catch {
    return fallback
  }
}

export default async function PaperPage({
  params,
}: {
  params: Promise<{ slug: string }>
}) {
  const { slug } = await params
  return <PaperView slug={decodeURIComponent(slug)} />
}
