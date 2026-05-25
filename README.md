# LLM Wiki

A desktop research-paper wiki that classifies your Zotero markdown notes with
Gemini and keeps them in lockstep with your Zotero collections.

* **Frontend** — Next.js 15 (App Router, static export) + Tailwind v4
* **Shell** — Tauri 2 (Rust 1.77+)
* **AI** — Gemini 2.5 Pro via the Google AI Studio API
* **Reference manager** — Zotero local API (port 23119) + the
  [ZotMoov](https://github.com/wileyyugioh/zotmoov) plugin

The full project specification lives in [`CLAUDE.md`](./CLAUDE.md).

---

## Features

* Drop a Zotero-exported `.md` file into `content/papers/unclassified/` and
  Gemini auto-categorises it into a lower-case kebab-case folder.
* The same category name is used as the Zotero collection name **and** the
  ZotMoov target folder — one source of truth, no mapping tables.
* Live dashboard with category cards, recent additions, and a
  one-click "Organize Now" pipeline.
* Two-column paper view with KaTeX math, `[[wikilink]]` references,
  backlinks, related-paper suggestions, an "Ask Gemini" panel that streams
  answers about the open paper, and a one-click Zotero deep-link.
* Cmd / Ctrl + K fuzzy search across every paper's title, tags, and
  summary (Fuse.js).
* Robust 5-step organise transaction with rollback — failed files always
  land back in `unclassified/`.
* Resilient Zotero sync: if Zotero is offline mid-organise the pending
  collection update is queued and replayed on reconnect.

---

## Installation

Pre-built installers are published as draft releases by the
[`build`](./.github/workflows/build.yml) workflow.  Download the asset that
matches your platform from
[**Releases**](../../releases).

### Linux

```bash
# Debian / Ubuntu / Mint / Pop!_OS
sudo apt install ./LLM-Wiki_*.deb

# Portable AppImage (no install required)
chmod +x LLM-Wiki_*.AppImage
./LLM-Wiki_*.AppImage
```

Runtime dependencies are pulled in by the `.deb` (`libwebkit2gtk-4.1-0`,
`libayatana-appindicator3-1`, `librsvg2-2`).

### Windows

Run `LLM-Wiki_*-setup.exe` (NSIS installer).  Windows 10 1809+ is required;
the installer pulls in **WebView2** automatically if it is not already
installed.

### macOS

Open `LLM-Wiki_*.dmg` and drag the app to `/Applications`.
The release is **not** code-signed yet, so the first launch needs:

```text
Right-click LLM Wiki.app → Open → Open anyway
```

(Alternatively, `xattr -dr com.apple.quarantine /Applications/LLM\ Wiki.app`.)

---

## Setup

### 1. Get a Gemini API key

LLM Wiki uses **gemini-2.5-pro**.  Free-tier keys work fine for personal use.

1. Sign in at <https://aistudio.google.com>.
2. Click **Get API key** → **Create API key in new project**.
3. Copy the value (starts with `AIza…`).

The key is stored in the **OS keychain** (Keychain on macOS, Credential
Manager on Windows, Secret Service on Linux) under the entry
`llm-wiki / llm-wiki-gemini-key`.  It is never written to any file.

### 2. Install and configure Zotero + ZotMoov

LLM Wiki keeps your markdown folder, your Zotero collections, and your PDF
files in sync.  All three rely on **one canonical name** per category
(lower-case, kebab-case), so ZotMoov **must** be configured to use it.

1. Install [Zotero](https://www.zotero.org/download/) (version 7+).
2. Install the [ZotMoov](https://github.com/wileyyugioh/zotmoov/releases)
   plugin from its `.xpi` file: *Tools → Add-ons → ⚙ → Install Add-on
   From File*.
3. Open **Edit → Settings → ZotMoov** and set:
   * **Destination Folder** — somewhere outside your Zotero data
     directory, e.g. `~/papers/`.
   * **Folder Pattern** — exactly:

     ```text
     {collection}
     ```

   That single token is what guarantees the LLM Wiki folder, Zotero
   collection, and ZotMoov target directory all share the same name.
4. Leave the Zotero local connector server enabled (it listens on
   `http://localhost:23119` by default).  The status indicator at the top
   of the LLM Wiki window will turn green once Zotero is running.

### 3. First-run guide

1. Launch **LLM Wiki**.
2. **Onboarding step 1** — paste the absolute path to your `content/`
   folder (it must contain `papers/` and `meta/` siblings; the app
   creates them if they are missing).
3. **Onboarding step 2** — paste your Gemini API key and click
   **연결 테스트** (Test Connection).  The button must turn green
   before the **시작하기** (Start) button enables.
4. The dashboard appears.  Drop a Zotero-exported markdown file into
   `content/papers/unclassified/`, click **지금 정리** (Organize Now),
   and watch the pipeline progress in the lower-right corner.
5. Use the global search (Cmd / Ctrl + K) once you have a few papers
   indexed.

The classification pipeline runs five steps with automatic rollback —
see [`wiki/src-tauri/src/organizer.rs`](./wiki/src-tauri/src/organizer.rs)
for the full transaction model.

---

## Markdown conventions

LLM Wiki reads YAML frontmatter on every paper.  Most fields are optional;
the only one Gemini truly needs is some form of title.

```markdown
---
title: "Attention Is All You Need"
authors:
  - Ashish Vaswani
  - Noam Shazeer
year: 2017
publication: NeurIPS
doi: 10.48550/arXiv.1706.03762
zotero_key: ABCD1234        # required for Zotero collection sync
tags: [transformer, attention]
summary: One-line takeaway shown in the dashboard and search results.
abstract: |
  Full abstract used as additional context for Gemini classification.
---

# Notes

Refer to [[layer-normalization]] and [[positional-encoding]].

Inline math: $f(x) = \\sum_i w_i x_i$.

Block math:

$$
\\text{Attention}(Q, K, V) = \\text{softmax}\\!\\left(\\frac{QK^\\top}{\\sqrt{d_k}}\\right) V
$$
```

* `[[slug]]` and `[[slug|label]]` link to `/papers/<slug>`.
* Inline `$ … $` and block `$$ … $$` math render with KaTeX.
* `zotero_key` is required only for the Zotero collection-update step;
  papers without one still classify and move into the right folder.

---

## Development

```bash
cd wiki
npm ci                # install JS deps
npm run dev           # = tauri dev — runs Next.js + opens the desktop window
```

The Tauri dev window points at `http://localhost:3000`; the Next.js
hot-reload picks up edits as usual.

### Building a desktop bundle locally

```bash
cd wiki
npm run build         # = next build && tauri build
```

Bundles land in `wiki/src-tauri/target/release/bundle/`.

### CLI scripts

| Command                       | What it does                                                              |
| ----------------------------- | ------------------------------------------------------------------------- |
| `npm run wiki:organize`       | One-shot batch organize for every file in `unclassified/`.                |
| `npm run wiki:organize:dry`   | Same, but no file moves and no Gemini calls — logs what would happen.     |
| `npm run wiki:watch`          | `chokidar` watcher on `unclassified/`; auto-organizes new files.          |
| `npm run wiki:backlinks`      | Rebuild `content/meta/backlinks.json` from every `[[wikilink]]`.          |
| `npm run wiki:search`         | Rebuild `wiki/public/search-index.json` for the Cmd/Ctrl+K search bar.    |

`npm run wiki:backlinks` and `npm run wiki:search` also run automatically
via the `prebuild` / `prenext:build` hooks.

---

## Directory layout

```text
llm-wiki/
├── CLAUDE.md                       # full project specification
├── content/
│   ├── meta/
│   │   ├── backlinks.json          # generated; flat slug → references map
│   │   ├── search-index.json
│   │   └── pending-zotero-sync.json
│   └── papers/
│       ├── unclassified/           # drop new .md files here
│       ├── .staging/               # transient — never commit
│       └── <category>/             # auto-created by Gemini classification
├── logs/
│   └── organize-YYYY-MM-DD.json    # daily organise result log
└── wiki/
    ├── scripts/                    # organize-papers.js + index builders
    ├── src/                        # Next.js frontend
    └── src-tauri/                  # Rust desktop shell (organizer, gemini, zotero, …)
```

---

## CI

The [`build` workflow](./.github/workflows/build.yml) runs on every push to
`main` and on manual dispatch.  It builds installers in parallel on
`ubuntu-latest`, `windows-latest`, and `macos-latest`, then uploads every
artifact to the same draft GitHub Release (`app-v<version>`) so the assets
are ready for promotion to a public release.

---

## License

Personal project — choose a license before publishing.
