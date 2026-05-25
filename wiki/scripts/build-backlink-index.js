#!/usr/bin/env node
/**
 * build-backlink-index.js
 *
 * Walks every classified paper under `content/papers/` (excluding
 * `unclassified/` and `.staging/`), extracts every `[[wikilink]]`
 * reference, and produces a flat slug → backlinks map.
 *
 *   content/meta/backlinks.json
 *   {
 *     "attention-is-all-you-need": ["lora", "retrieval-augmented-generation"],
 *     ...
 *   }
 *
 * Slugs are the markdown filename without `.md`.
 *
 * Usage:  node scripts/build-backlink-index.js
 */

import { readFileSync, writeFileSync, readdirSync, statSync, mkdirSync } from "fs";
import { join, basename, dirname, extname } from "path";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const ROOT = join(__dirname, "..", "..");
const PAPERS_DIR = join(ROOT, "content", "papers");
const OUT_FILE = join(ROOT, "content", "meta", "backlinks.json");
const PUBLIC_MIRROR = join(__dirname, "..", "public", "backlinks.json");

const EXCLUDED_TOP_LEVEL = new Set(["unclassified", ".staging"]);

/** Recursively collect classified paper .md files. */
function collectClassifiedPapers() {
  const results = [];

  let topEntries;
  try {
    topEntries = readdirSync(PAPERS_DIR);
  } catch {
    return results;
  }

  for (const entry of topEntries) {
    if (entry.startsWith(".")) continue;
    if (EXCLUDED_TOP_LEVEL.has(entry)) continue;

    const full = join(PAPERS_DIR, entry);
    if (!statSync(full).isDirectory()) continue;

    walk(full, results);
  }

  return results;
}

function walk(dir, out) {
  for (const entry of readdirSync(dir)) {
    if (entry.startsWith(".")) continue;
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      walk(full, out);
    } else if (extname(entry) === ".md") {
      out.push(full);
    }
  }
}

/** Extract [[wikilink]] targets (supports `[[target|label]]`). */
function extractWikilinks(text) {
  const targets = new Set();
  for (const m of text.matchAll(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]+)?\]\]/g)) {
    const slug = m[1].trim().replace(/\.md$/i, "");
    if (slug) targets.add(slug);
  }
  return [...targets];
}

const files = collectClassifiedPapers();

/** @type {Record<string, string[]>} */
const backlinks = {};

for (const filePath of files) {
  const sourceSlug = basename(filePath, ".md");
  const raw = readFileSync(filePath, "utf8");

  for (const target of extractWikilinks(raw)) {
    if (target === sourceSlug) continue; // ignore self-links
    if (!backlinks[target]) backlinks[target] = [];
    if (!backlinks[target].includes(sourceSlug)) {
      backlinks[target].push(sourceSlug);
    }
  }
}

for (const key of Object.keys(backlinks)) {
  backlinks[key].sort();
}

mkdirSync(dirname(OUT_FILE), { recursive: true });
writeFileSync(OUT_FILE, JSON.stringify(backlinks, null, 2), "utf8");

// Mirror to wiki/public/ so the static export can fetch it without a Tauri call.
mkdirSync(dirname(PUBLIC_MIRROR), { recursive: true });
writeFileSync(PUBLIC_MIRROR, JSON.stringify(backlinks, null, 2), "utf8");

const totalRefs = Object.values(backlinks).reduce((n, arr) => n + arr.length, 0);
console.log(
  `Backlink index: ${Object.keys(backlinks).length} target(s), ` +
    `${totalRefs} reference(s) across ${files.length} file(s) → ${OUT_FILE}`
);
