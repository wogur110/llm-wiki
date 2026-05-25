#!/usr/bin/env node
/**
 * build-search-index.js
 *
 * Reads YAML frontmatter from every paper under `content/papers/`
 * (excluding `unclassified/` and `.staging/`) and emits a Fuse.js-ready
 * index at `wiki/public/search-index.json`.
 *
 * Each entry is:
 *   {
 *     "title":    string,
 *     "tags":     string[],
 *     "summary":  string,
 *     "category": string,        // derived from path: papers/<category>/file.md
 *     "slug":     string,        // filename without .md (or fm.slug)
 *     "year":     number | null  // fm.year, or year parsed from fm.date
 *   }
 *
 * Usage:  node scripts/build-search-index.js
 */

import { readFileSync, writeFileSync, readdirSync, statSync, mkdirSync } from "fs";
import { join, basename, dirname, extname, relative } from "path";
import { fileURLToPath } from "url";
import matter from "gray-matter";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const ROOT = join(__dirname, "..", "..");
const PAPERS_DIR = join(ROOT, "content", "papers");
const OUT_FILE = join(__dirname, "..", "public", "search-index.json");

const EXCLUDED_TOP_LEVEL = new Set(["unclassified", ".staging"]);

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

function stripMarkdown(text) {
  return text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`[^`]+`/g, " ")
    .replace(/!\[.*?\]\(.*?\)/g, " ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/[*_~>]+/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function firstParagraph(body) {
  const cleaned = body.replace(/^\s+/, "");
  const para = cleaned.split(/\n\s*\n/)[0] ?? "";
  return stripMarkdown(para).slice(0, 400);
}

/** Extract a 4-digit year from various plausible frontmatter shapes. */
function extractYear(fm) {
  if (typeof fm.year === "number") return fm.year;
  if (typeof fm.year === "string") {
    const y = parseInt(fm.year, 10);
    if (!Number.isNaN(y)) return y;
  }
  const dateLike = fm.date ?? fm.published ?? fm.year_published;
  if (dateLike) {
    const m = String(dateLike).match(/\b(19|20)\d{2}\b/);
    if (m) return parseInt(m[0], 10);
  }
  return null;
}

function asStringArray(value) {
  if (!value) return [];
  if (Array.isArray(value)) return value.map((v) => String(v));
  return [String(value)];
}

const files = collectClassifiedPapers();

const entries = files.map((filePath) => {
  const raw = readFileSync(filePath, "utf8");
  const { data: fm, content } = matter(raw);

  const rel = relative(PAPERS_DIR, filePath).replace(/\\/g, "/");
  const category = rel.split("/")[0] ?? null;

  const slug = (fm.slug ? String(fm.slug) : basename(filePath, ".md")).trim();

  const title = fm.title
    ? String(fm.title)
    : slug.replace(/-/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());

  const summary = fm.summary
    ? String(fm.summary)
    : fm.abstract
      ? String(fm.abstract)
      : firstParagraph(content);

  return {
    title,
    tags: asStringArray(fm.tags),
    summary,
    category,
    slug,
    year: extractYear(fm),
  };
});

entries.sort((a, b) => a.slug.localeCompare(b.slug));

mkdirSync(dirname(OUT_FILE), { recursive: true });
writeFileSync(OUT_FILE, JSON.stringify(entries, null, 2), "utf8");

console.log(`Search index: ${entries.length} entries → ${OUT_FILE}`);
