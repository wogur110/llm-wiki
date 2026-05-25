/**
 * Markdown LaTeX rendering tests.
 *
 * Verifies the remark-math + rehype-katex pipeline configured in
 * `src/lib/markdown.ts`:
 *   * `$$ … $$`  → block math   (KaTeX wraps in `.katex-display`)
 *   * `$ … $`    → inline math  (KaTeX wraps in `.katex` without `-display`)
 *   * malformed input → renderer must NOT throw (KaTeX is configured with
 *     `strict: 'ignore'`, but we want a regression test that protects the
 *     `try/catch`-free renderer contract).
 */

import React from 'react'
import { describe, it, expect } from 'vitest'
import { render } from '@testing-library/react'
import { renderMarkdown } from '../lib/markdown'

async function renderMd(md: string): Promise<HTMLElement> {
  const html = await renderMarkdown(md)
  const { container } = render(
    <div data-testid="md" dangerouslySetInnerHTML={{ __html: html }} />,
  )
  return container.querySelector('[data-testid="md"]') as HTMLElement
}

describe('LaTeX rendering', () => {
  it('$$E = mc^2$$ renders as a block math element', async () => {
    // remark-math treats `$$ ... $$` as display math when the body sits
    // between newlines (the conventional Pandoc / Obsidian "block math"
    // syntax).  KaTeX wraps display math in `<span class="katex-display">`.
    const root = await renderMd('$$\nE = mc^2\n$$')

    const block = root.querySelector('.katex-display')
    expect(block).not.toBeNull()

    // KaTeX always emits a `.katex` node inside the display wrapper; we
    // assert the rendered output references the source variables.
    const katex = block!.querySelector('.katex')
    expect(katex).not.toBeNull()
    // KaTeX leaves a hidden MathML annotation containing the source LaTeX.
    const annotation = block!.querySelector(
      'annotation[encoding="application/x-tex"]',
    )
    expect(annotation?.textContent?.trim()).toBe('E = mc^2')
  })

  it('$x^2$ renders as an inline math element (not display)', async () => {
    const root = await renderMd('The quadratic is $x^2$ in this sentence.')

    // Inline math should produce `.katex` WITHOUT being inside `.katex-display`.
    const inline = root.querySelector('.katex')
    expect(inline).not.toBeNull()
    expect(inline!.closest('.katex-display')).toBeNull()

    const annotation = inline!.querySelector(
      'annotation[encoding="application/x-tex"]',
    )
    expect(annotation?.textContent?.trim()).toBe('x^2')

    // The surrounding paragraph text should still be present.
    expect(root.textContent).toContain('The quadratic is')
    expect(root.textContent).toContain('in this sentence.')
  })

  it('malformed "$$ unclosed $$" markup does not crash the renderer', async () => {
    // Three potentially-malformed inputs are exercised:
    //   1. Single $$ with no closing pair on the same paragraph.
    //   2. Empty math block.
    //   3. Math command that KaTeX cannot parse (\notACommand).
    const inputs = [
      'Paragraph $$ unclosed $$ more text',
      '$$$$',
      '$\\notACommand{x}$',
    ]

    for (const md of inputs) {
      // The renderer must complete without throwing.  Use `expect().resolves`
      // so a rejected promise (i.e. uncaught throw) fails the test loudly.
      await expect(renderMarkdown(md)).resolves.toEqual(expect.any(String))
    }
  })
})
