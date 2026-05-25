# CLAUDE.md — LLM-Wiki Full Specification

## Project Overview
Tauri 2 + Next.js 15 desktop app for research paper wiki.
Reads local Zotero-exported markdown files.
AI classification via Gemini API is MANDATORY.
Full Zotero local API sync via ZotMoov integration.

## Environment
- Node.js: 20 LTS
- Rust: latest stable
- Framework: Next.js 15 App Router + Tauri 2
- Package Manager: npm

## Directory Structure
llm-wiki/
├── CLAUDE.md
├── private/               # NEVER read, NEVER deploy
├── logs/                  # Processing logs
├── content/
│   ├── posts/
│   ├── meta/
│   │   ├── backlinks.json
│   │   ├── search-index.json
│   │   └── pending-zotero-sync.json
│   └── papers/
│       ├── unclassified/
│       ├── .staging/
│       └── [category]/
└── wiki/
    ├── src/
    ├── src-tauri/
    │   └── src/
    │       ├── main.rs
    │       ├── keychain.rs
    │       ├── gemini.rs
    │       ├── zotero.rs
    │       ├── organizer.rs
    │       ├── transaction.rs
    │       └── pending_sync.rs
    └── scripts/
        ├── organize-papers.js
        ├── build-backlink-index.js
        └── build-search-index.js

## LLM Provider
- Provider: Gemini only (gemini-2.5-pro)
- Auth: API Key stored in OS Keychain
- Key name: "llm-wiki-gemini-key"
- Source: aistudio.google.com (free tier)
- AI features are MANDATORY — app cannot start without valid key

## Onboarding Rules
- Step 1: Folder path selection (mandatory)
- Step 2: Gemini API Key input (mandatory)
- Step 3: Test connection (must pass)
- Start button disabled until both pass
- Redirect to onboarding if either missing on app start

## Category Mapping Rules (STRICT)
- Single Source of Truth: Gemini output
- Format: lower-case kebab-case ONLY
- LLM-Wiki folder = Zotero Collection name = ZotMoov folder
- Names must be IDENTICAL across all systems
- No mapping table allowed
- ZotMoov folder pattern must be {collection}

## Transaction Steps & Rollback
Steps:
1. Move to .staging/           → rollback: .staging → unclassified
2. Gemini classification       → rollback: none needed
3. Move to target category/    → rollback: target → unclassified
4. Zotero Collection update    → rollback: revert to previous collection
5. ZotMoov PDF confirmation    → rollback: revert step 4

Rules:
- Any failure → reverse completed steps in reverse order
- File ALWAYS returns to unclassified/ on any failure
- All failures logged to logs/organize-YYYY-MM-DD.json

## Zotero Integration
- API endpoint: http://localhost:23119/api
- Poll interval: 30 seconds
- Pending queue: content/meta/pending-zotero-sync.json
- Auto-sync when Zotero reconnects
- ZotMoov PDF confirmation timeout: 10 seconds with smart polling

## Security
- private/: never read, never deploy, in .gitignore
- API key: OS Keychain only, never in any file
- .staging/: excluded from git