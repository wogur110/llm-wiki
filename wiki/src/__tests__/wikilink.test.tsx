/**
 * Markdown `[[wikilink]]` rendering tests.
 *
 * Exercises `renderMarkdown` (src/lib/markdown.ts) — specifically the
 * `remarkWikilinks` plugin that converts `[[slug]]` / `[[slug|label]]`
 * references into `<a href="/papers/<kebab-case-slug>" class="wikilink">`.
 *
 * Per CLAUDE.md every paper filename is lower-case kebab-case, so the
 * renderer normalises CamelCase / spaced wikilinks down to that form.
 */

import React from 'react'
import { describe, it, expect } from 'vitest'
import { render } from '@testing-library/react'
import { renderMarkdown } from '../lib/markdown'

// ── helpers ──────────────────────────────────────────────────────────────────

/**
 * Render markdown to HTML and mount it into a detached `<div>` so we can use
 * standard DOM queries.  Returns the container element.
 */
async function renderMd(md: string): Promise<HTMLElement> {
  const html = await renderMarkdown(md)
  const { container } = render(
    <div data-testid="md" dangerouslySetInnerHTML={{ __html: html }} />,
  )
  return container.querySelector('[data-testid="md"]') as HTMLElement
}

// ── tests ────────────────────────────────────────────────────────────────────

describe('wikilink rendering', () => {
  it('[[SomePaper]] renders as an <a> tag with the wikilink class', async () => {
    const root = await renderMd('See [[SomePaper]] for details.')

    const anchors = root.querySelectorAll('a')
    expect(anchors.length).toBe(1)

    const a = anchors[0]
    expect(a.tagName).toBe('A')
    expect(a.classList.contains('wikilink')).toBe(true)
    // Display text preserves the author-typed slug (less the `.md`).
    expect(a.textContent).toBe('SomePaper')
  })

  it('[[SomePaper]] href is /papers/some-paper (kebab-cased)', async () => {
    const root = await renderMd('[[SomePaper]]')
    const a = root.querySelector('a')
    expect(a).not.toBeNull()
    // `getAttribute('href')` keeps the raw attribute value (no origin prefix
    // that `a.href` would add via JSDOM's URL resolution).
    expect(a!.getAttribute('href')).toBe('/papers/some-paper')
    expect(a!.getAttribute('data-slug')).toBe('some-paper')
  })

  it('[[Missing Paper]] still renders as a (dead) wikilink', async () => {
    // The renderer has no notion of "missing" — it produces an <a> for every
    // [[ref]].  When the target doesn't exist on disk the link is just a
    // dead link, which we model by checking the wikilink class is present
    // and the href points at the slugified target.
    const root = await renderMd('Here is [[Missing Paper]] in prose.')

    const a = root.querySelector('a.wikilink')
    expect(a).not.toBeNull()
    expect(a!.getAttribute('href')).toBe('/papers/missing-paper')
    expect(a!.textContent).toBe('Missing Paper')
  })

  it('nested [[link]] inside bold + list renders correctly', async () => {
    const md = [
      '# Heading',
      '',
      '- Bold wikilink: **[[Attention Is All You Need]]** in a bullet.',
      '- Inline wikilink: [[lora]] mid-sentence.',
      '',
      '> Quote with [[BERT]] reference.',
    ].join('\n')

    const root = await renderMd(md)

    // The bold + list combination should still produce three working anchors,
    // one of which lives inside a <strong>, one inside a plain <li>, one in
    // a <blockquote>.
    const anchors = Array.from(root.querySelectorAll('a.wikilink'))
    expect(anchors).toHaveLength(3)

    const hrefs = anchors.map((a) => a.getAttribute('href'))
    expect(hrefs).toEqual([
      '/papers/attention-is-all-you-need',
      '/papers/lora',
      '/papers/bert',
    ])

    // First anchor sits inside a <strong> inside an <li>.
    expect(anchors[0].closest('strong')).not.toBeNull()
    expect(anchors[0].closest('li')).not.toBeNull()

    // Third anchor lives inside a blockquote.
    expect(anchors[2].closest('blockquote')).not.toBeNull()
  })
})
