/**
 * Markdown renderer — unified pipeline that produces HTML strings for the
 * paper view.  Handles:
 *
 *   * GitHub-flavoured paragraphs / headings / lists / code
 *   * `$inline$` and `$$block$$` LaTeX via remark-math + rehype-katex
 *   * `[[wikilink]]` and `[[slug|label]]` → `<a href="/papers/slug">…</a>`
 *
 * Output is a string; render with `dangerouslySetInnerHTML` and pair with
 * the KaTeX stylesheet (imported once in `layout.tsx`).
 */

import { unified } from 'unified'
import remarkParse from 'remark-parse'
import remarkMath from 'remark-math'
import remarkRehype from 'remark-rehype'
import rehypeKatex from 'rehype-katex'
import rehypeStringify from 'rehype-stringify'
import { visit } from 'unist-util-visit'
import type { Plugin } from 'unified'
import type { Root, Text, Link, PhrasingContent, Parent } from 'mdast'

/** Match `[[slug]]` or `[[slug|label]]` (slug may include `-`, `_`, `.`). */
const WIKILINK_RE = /\[\[([^\]|#]+?)(?:#[^\]|]*)?(?:\|([^\]]+))?\]\]/g

/**
 * Remark plugin that walks every text node and splits embedded `[[wikilink]]`
 * references into proper link nodes pointing at `/papers/<slug>`.
 */
const remarkWikilinks: Plugin<[], Root> = () => {
  return (tree) => {
    visit(tree, 'text', (node: Text, index, parent: Parent | undefined) => {
      if (!parent || index == null) return
      const value = node.value
      if (!value.includes('[[')) return

      const pieces: PhrasingContent[] = []
      let lastEnd = 0

      WIKILINK_RE.lastIndex = 0
      for (let m = WIKILINK_RE.exec(value); m; m = WIKILINK_RE.exec(value)) {
        const [full, rawSlug, label] = m
        const start = m.index
        const end = start + full.length

        if (start > lastEnd) {
          pieces.push({ type: 'text', value: value.slice(lastEnd, start) })
        }

        const slug = rawSlug.trim().replace(/\.md$/i, '')
        const display = (label?.trim() || slug).trim()

        const link: Link = {
          type: 'link',
          url: `/papers/${encodeURIComponent(slug)}`,
          title: null,
          children: [{ type: 'text', value: display }],
          data: { hProperties: { className: ['wikilink'], 'data-slug': slug } },
        }
        pieces.push(link)
        lastEnd = end
      }

      if (lastEnd === 0) return
      if (lastEnd < value.length) {
        pieces.push({ type: 'text', value: value.slice(lastEnd) })
      }

      parent.children.splice(index, 1, ...pieces)
      return index + pieces.length
    })
  }
}

const processor = unified()
  .use(remarkParse)
  .use(remarkMath)
  .use(remarkWikilinks)
  .use(remarkRehype, { allowDangerousHtml: false })
  .use(rehypeKatex, { strict: 'ignore' })
  .use(rehypeStringify)

/** Convert a markdown string to a self-contained HTML fragment. */
export async function renderMarkdown(md: string): Promise<string> {
  const file = await processor.process(md)
  return String(file)
}
