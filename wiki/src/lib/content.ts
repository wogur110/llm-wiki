/**
 * Content folder client — typed wrappers around the Tauri `content::*` commands.
 *
 * All paper data flows through this module so pages don't have to deal with
 * raw `invoke()` calls or YAML frontmatter parsing.  Frontmatter is parsed
 * here with `gray-matter` (returning a strongly-typed `PaperMeta`).
 */

import { invoke } from '@tauri-apps/api/core'
import matter from 'gray-matter'

// ── Wire types (must match the Rust `Serialize` structs) ─────────────────────

export interface CategoryInfo {
  name: string
  paper_count: number
  latest_paper_date: string | null
}

/**
 * One node in the nested category tree returned by `list_category_tree`.
 *
 * A node is a **branch** when `children.length > 0` (clicking expands it) or a
 * **leaf** when `children` is empty (clicking shows that category's papers).
 */
export interface CategoryNode {
  name: string
  /** Relative path from `content/papers/` using `/` separators. */
  path: string
  /** Papers directly in this folder (not in child folders). */
  paper_count: number
  /** Papers in this folder AND all descendants. */
  total_paper_count: number
  latest_paper_date: string | null
  children: CategoryNode[]
}

interface RawPaperFrontmatter {
  slug: string
  category: string
  frontmatter: string
  created_at: string
}

interface RawPaperFile {
  slug: string
  category: string
  content: string
  created_at: string
}

export interface UnclassifiedPaper {
  path: string
  name: string
  created_at: string
}

// ── Frontend-facing types ────────────────────────────────────────────────────

/**
 * Strongly typed frontmatter for a paper.  Unknown extra keys are preserved
 * in `extra` so the schema can evolve without breaking the type system.
 */
export interface PaperMeta {
  slug: string
  category: string
  created_at: string

  title: string
  year: number | null
  authors: string[]
  publication: string | null
  doi: string | null
  zotero_key: string | null
  tags: string[]
  summary: string | null
  extra: Record<string, unknown>
}

/** Full paper payload — frontmatter plus the markdown body. */
export interface PaperContent extends PaperMeta {
  body: string
}

// ── Frontmatter parsing ──────────────────────────────────────────────────────

function asStringArray(v: unknown): string[] {
  if (!v) return []
  if (Array.isArray(v)) return v.map(String)
  return [String(v)]
}

function asNumberOrNull(v: unknown): number | null {
  if (typeof v === 'number' && Number.isFinite(v)) return v
  if (typeof v === 'string') {
    const m = v.match(/\b(19|20)\d{2}\b/)
    if (m) return parseInt(m[0], 10)
    const n = parseInt(v, 10)
    if (!Number.isNaN(n)) return n
  }
  return null
}

function asStringOrNull(v: unknown): string | null {
  if (v == null) return null
  const s = String(v).trim()
  return s.length > 0 ? s : null
}

function buildMeta(
  slug: string,
  category: string,
  created_at: string,
  fm: Record<string, unknown>,
): PaperMeta {
  const known = new Set([
    'title',
    'year',
    'date',
    'authors',
    'author',
    'publication',
    'venue',
    'journal',
    'doi',
    'DOI',
    'zotero_key',
    'zoteroKey',
    'tags',
    'summary',
    'abstract',
  ])
  const extra: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(fm)) {
    if (!known.has(k)) extra[k] = v
  }

  const yearRaw = fm.year ?? fm.date
  return {
    slug,
    category,
    created_at,
    title:
      asStringOrNull(fm.title) ??
      slug.replace(/-/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase()),
    year: asNumberOrNull(yearRaw),
    authors: asStringArray(fm.authors ?? fm.author),
    publication:
      asStringOrNull(fm.publication) ??
      asStringOrNull(fm.venue) ??
      asStringOrNull(fm.journal),
    doi: asStringOrNull(fm.doi ?? fm.DOI),
    zotero_key: asStringOrNull(fm.zotero_key ?? fm.zoteroKey),
    tags: asStringArray(fm.tags),
    summary:
      asStringOrNull(fm.summary) ??
      asStringOrNull(fm.abstract),
    extra,
  }
}

/** Parse a raw YAML frontmatter block (no surrounding `---`). */
function parseFrontmatterBlock(yaml: string): Record<string, unknown> {
  if (!yaml.trim()) return {}
  // gray-matter expects the leading/trailing `---`; we add them back.
  const wrapped = `---\n${yaml}\n---\n`
  try {
    return matter(wrapped).data ?? {}
  } catch {
    return {}
  }
}

// ── Tauri calls ──────────────────────────────────────────────────────────────

export function getContentRoot(): string | null {
  if (typeof window === 'undefined') return null
  return window.localStorage.getItem('content-root')
}

function requireRoot(): string {
  const root = getContentRoot()
  if (!root) throw new Error('content-root not set — finish onboarding first')
  return root
}

export async function listCategories(): Promise<CategoryInfo[]> {
  const contentRoot = requireRoot()
  return await invoke<CategoryInfo[]>('list_categories', { contentRoot })
}

/**
 * Return the full nested category tree.  Each node carries a `path` relative
 * to `content/papers/` (e.g. `"machine-learning/deep-learning"`) that can be
 * passed directly to `listPapersInCategory`.
 */
export async function listCategoryTree(): Promise<CategoryNode[]> {
  const contentRoot = requireRoot()
  return await invoke<CategoryNode[]>('list_category_tree', { contentRoot })
}

export async function listPapersInCategory(
  category: string,
): Promise<PaperMeta[]> {
  const contentRoot = requireRoot()
  const raw = await invoke<RawPaperFrontmatter[]>('list_papers_in_category', {
    contentRoot,
    category,
  })
  return raw.map((r) =>
    buildMeta(
      r.slug,
      r.category,
      r.created_at,
      parseFrontmatterBlock(r.frontmatter),
    ),
  )
}

export async function listRecentPapers(limit = 10): Promise<PaperMeta[]> {
  const contentRoot = requireRoot()
  const raw = await invoke<RawPaperFrontmatter[]>('list_recent_papers', {
    contentRoot,
    limit,
  })
  return raw.map((r) =>
    buildMeta(
      r.slug,
      r.category,
      r.created_at,
      parseFrontmatterBlock(r.frontmatter),
    ),
  )
}

export async function listAllPapers(): Promise<PaperMeta[]> {
  // 100k papers is well beyond any realistic library size — used as "all".
  return await listRecentPapers(100_000)
}

export async function readPaper(slug: string): Promise<PaperContent> {
  const contentRoot = requireRoot()
  const category = await invoke<string>('find_paper_category', {
    contentRoot,
    slug,
  })
  const raw = await invoke<RawPaperFile>('read_paper_file', {
    contentRoot,
    category,
    slug,
  })
  const parsed = matter(raw.content)
  const meta = buildMeta(
    raw.slug,
    raw.category,
    raw.created_at,
    parsed.data as Record<string, unknown>,
  )
  return { ...meta, body: parsed.content }
}

export async function listUnclassified(): Promise<UnclassifiedPaper[]> {
  const contentRoot = requireRoot()
  return await invoke<UnclassifiedPaper[]>('list_unclassified', { contentRoot })
}

export async function readBacklinks(): Promise<Record<string, string[]>> {
  const contentRoot = requireRoot()
  const json = await invoke<string>('read_backlinks', { contentRoot })
  try {
    const parsed = JSON.parse(json) as Record<string, string[]>
    return parsed ?? {}
  } catch {
    return {}
  }
}

// ── Formatting helpers ───────────────────────────────────────────────────────

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return d.toLocaleDateString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  })
}

export function formatRelative(iso: string | null | undefined): string {
  if (!iso) return ''
  const d = new Date(iso).getTime()
  if (Number.isNaN(d)) return ''
  const diffMs = Date.now() - d
  const sec = Math.round(diffMs / 1000)
  if (sec < 60) return '방금 전'
  const min = Math.round(sec / 60)
  if (min < 60) return `${min}분 전`
  const hr = Math.round(min / 60)
  if (hr < 24) return `${hr}시간 전`
  const day = Math.round(hr / 24)
  if (day < 30) return `${day}일 전`
  return formatDate(iso)
}
