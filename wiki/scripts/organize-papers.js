#!/usr/bin/env node
/**
 * organize-papers.js
 *
 * Node CLI counterpart to the Tauri `process_paper` Rust command.
 * Drives the same 5-step organise pipeline for batch / headless use:
 *
 *   1. unclassified/  →  .staging/
 *   2. Gemini classification           (rate-limited, max 5 concurrent)
 *   3. .staging/      →  {category}/
 *   4. Zotero collection update        (best-effort, queued on failure)
 *   5. ZotMoov PDF confirmation        (deferred — handled in Rust path)
 *
 * Modes
 *   one-shot       node scripts/organize-papers.js
 *   single file    node scripts/organize-papers.js --file path/to/paper.md
 *   dry run        node scripts/organize-papers.js --dry-run
 *   watch loop     node scripts/organize-papers.js --watch
 *
 * Tauri integration
 *   When the Tauri app spawns this script via its shell plugin it forwards
 *   `GEMINI_API_KEY` from the OS keychain in the child env.  The script
 *   therefore always reads its key from `process.env.GEMINI_API_KEY`; the
 *   keychain lookup itself stays in the Tauri main process.
 *
 * Side effects
 *   - Writes daily results to `logs/organize-YYYY-MM-DD.json`
 *   - Records processed DOIs to `content/meta/.processed-dois.json`
 *   - Appends failures to `content/meta/pending-zotero-sync.json`
 */

import {
  readdirSync,
  renameSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  existsSync,
} from "fs";
import { dirname, join, basename, extname } from "path";
import { fileURLToPath } from "url";
import { createHash } from "crypto";

import matter from "gray-matter";
import chokidar from "chokidar";

// ── paths ──────────────────────────────────────────────────────────────────────
// Tests may override CONTENT_ROOT and LOG_DIR via env vars so that file
// operations target a temp directory instead of the real project tree.

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const ROOT          = process.env.ORGANIZE_ROOT         ?? join(__dirname, "..", "..");
const CONTENT_ROOT  = process.env.ORGANIZE_CONTENT_ROOT ?? join(ROOT, "content");
const LOG_DIR       = process.env.ORGANIZE_LOG_DIR      ?? join(ROOT, "logs");
const META_DIR      = join(CONTENT_ROOT, "meta");
const PAPERS_DIR    = join(CONTENT_ROOT, "papers");
const UNCLASSIFIED  = join(PAPERS_DIR, "unclassified");
const STAGING       = join(PAPERS_DIR, ".staging");
const PENDING_QUEUE = join(META_DIR, "pending-zotero-sync.json");
const PROCESSED_DOIS = join(META_DIR, ".processed-dois.json");

// ── constants ──────────────────────────────────────────────────────────────────

const GEMINI_MODEL = "gemini-2.5-pro";
const GEMINI_BASE = "https://generativelanguage.googleapis.com/v1beta/models";
const ZOTERO_API = "http://localhost:23119/api";
const MAX_CONCURRENT_GEMINI = 5;
const WATCH_DEBOUNCE_MS = 750;

// ── CLI flags ──────────────────────────────────────────────────────────────────

const argv = process.argv.slice(2);
const DRY_RUN = argv.includes("--dry-run");
const WATCH = argv.includes("--watch");

const fileArgIdx = argv.indexOf("--file");
const SINGLE_FILE =
  fileArgIdx !== -1 && argv[fileArgIdx + 1] ? argv[fileArgIdx + 1] : null;

// ── small helpers ──────────────────────────────────────────────────────────────

const today = () => new Date().toISOString().slice(0, 10);
const nowIso = () => new Date().toISOString();

function ensureDir(p) {
  mkdirSync(p, { recursive: true });
}

function readJson(path, fallback) {
  if (!existsSync(path)) return fallback;
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch {
    return fallback;
  }
}

function writeJson(path, value) {
  ensureDir(dirname(path));
  writeFileSync(path, JSON.stringify(value, null, 2), "utf8");
}

function moveFile(src, dst) {
  ensureDir(dirname(dst));
  renameSync(src, dst);
}

// ── result log (one JSON array per day, success + failure + skipped) ───────────

function appendLog(entry) {
  ensureDir(LOG_DIR);
  const logFile = join(LOG_DIR, `organize-${today()}.json`);
  const existing = readJson(logFile, []);
  existing.push({ timestamp: nowIso(), ...entry });
  writeJson(logFile, existing);
}

// ── pending Zotero queue ───────────────────────────────────────────────────────

function loadQueue() {
  return readJson(PENDING_QUEUE, { items: [] });
}

function saveQueue(q) {
  writeJson(PENDING_QUEUE, q);
}

function queueForSync(paperFile, targetCollection, zoteroKey) {
  if (!zoteroKey) return;
  const q = loadQueue();
  q.items.push({
    paper_file: paperFile,
    zotero_item_key: zoteroKey,
    target_collection: targetCollection,
    queued_at: nowIso(),
  });
  saveQueue(q);
}

// ── DOI dedup ──────────────────────────────────────────────────────────────────

function loadProcessedDois() {
  return readJson(PROCESSED_DOIS, { dois: {} });
}

function saveProcessedDois(state) {
  writeJson(PROCESSED_DOIS, state);
}

function normalizeDoi(raw) {
  return String(raw)
    .toLowerCase()
    .trim()
    .replace(/^doi:\s*/, "")
    .replace(/^https?:\/\/(dx\.)?doi\.org\//, "");
}

function hashDoi(raw) {
  return createHash("sha256").update(normalizeDoi(raw)).digest("hex");
}

function extractDoi(fm, body) {
  const direct = fm.doi ?? fm.DOI;
  if (direct) return String(direct);
  const m = body.match(/(10\.\d{4,9}\/[-._;()/:A-Z0-9]+)/i);
  return m ? m[1] : null;
}

// ── concurrency limiter (simple p-limit) ───────────────────────────────────────

class Limiter {
  constructor(max) {
    this.max = max;
    this.running = 0;
    this.queue = [];
  }
  run(fn) {
    return new Promise((resolve, reject) => {
      const task = () => {
        this.running++;
        Promise.resolve()
          .then(fn)
          .then(resolve, reject)
          .finally(() => {
            this.running--;
            const next = this.queue.shift();
            if (next) next();
          });
      };
      if (this.running < this.max) task();
      else this.queue.push(task);
    });
  }
}

const geminiLimiter = new Limiter(MAX_CONCURRENT_GEMINI);

// ── Gemini classification ──────────────────────────────────────────────────────

async function classifyPaper(apiKey, title, abstractText) {
  const prompt =
    `Classify the following research paper into a single lower-case kebab-case ` +
    `category (e.g. "large-language-models", "reinforcement-learning", "computer-vision"). ` +
    `Return ONLY the category string, nothing else.\n\n` +
    `Title: ${title}\n\nAbstract: ${abstractText}`;

  const url = `${GEMINI_BASE}/${GEMINI_MODEL}:generateContent?key=${apiKey}`;
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ contents: [{ parts: [{ text: prompt }] }] }),
  });

  if (!res.ok) {
    throw new Error(`Gemini HTTP ${res.status}: ${await res.text()}`);
  }
  const json = await res.json();
  const raw = json?.candidates?.[0]?.content?.parts?.[0]?.text ?? "";
  const category = raw
    .trim()
    .replace(/[`"']/g, "")
    .toLowerCase()
    .replace(/[\s_/\\]+/g, "-")   // spaces, underscores, slashes → hyphens
    .replace(/[^a-z0-9-]/g, "")  // strip remaining non-kebab characters
    .replace(/-+/g, "-")          // collapse consecutive hyphens
    .replace(/^-|-$/g, "");       // strip leading/trailing hyphens
  if (!category) throw new Error("Gemini returned empty category");
  if (!/^[a-z0-9][a-z0-9-]*$/.test(category)) {
    throw new Error(`Gemini returned invalid category: "${category}"`);
  }
  return category;
}

// ── Zotero helpers (best-effort, optional) ─────────────────────────────────────

async function zoteroReachable() {
  try {
    const r = await fetch(`${ZOTERO_API}/`, {
      signal: AbortSignal.timeout(2000),
    });
    return r.ok;
  } catch {
    return false;
  }
}

async function ensureCollection(name) {
  const res = await fetch(`${ZOTERO_API}/collections`);
  if (!res.ok) throw new Error(`Zotero collections fetch failed: ${res.status}`);
  const cols = await res.json();
  const existing = cols.find((c) => c.data?.name === name);
  if (existing) return existing.key;

  const create = await fetch(`${ZOTERO_API}/collections`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify([{ name }]),
  });
  if (!create.ok) throw new Error(`Create collection failed: ${create.status}`);
  const created = await create.json();
  const key = created?.success?.["0"];
  if (!key) throw new Error("Zotero create-collection response missing key");
  return key;
}

async function setItemCollection(itemKey, collectionKey) {
  const res = await fetch(`${ZOTERO_API}/items/${itemKey}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ collections: [collectionKey] }),
  });
  if (!res.ok) throw new Error(`Zotero PATCH failed: ${res.status}`);
}

// ── one-paper transaction ──────────────────────────────────────────────────────

async function organizeOne(paperPath, apiKey) {
  const name = basename(paperPath);

  let raw;
  try {
    raw = readFileSync(paperPath, "utf8");
  } catch (e) {
    appendLog({ paper: name, result: "failure", step: "read", error: String(e) });
    console.error(`[FAIL] ${name}: read — ${e}`);
    return;
  }

  const { data: fm, content: body } = matter(raw);
  const doi = extractDoi(fm, body);
  const doiHash = doi ? hashDoi(doi) : null;

  // Duplicate check (DOI hash)
  if (doiHash) {
    const seen = loadProcessedDois();
    if (seen.dois[doiHash]) {
      appendLog({
        paper: name,
        result: "skipped",
        reason: "duplicate-doi",
        doi,
        doi_hash: doiHash,
      });
      console.log(`[SKIP] ${name}: duplicate DOI (${doi})`);
      return;
    }
  }

  // Title + abstract for Gemini
  const title =
    fm.title ??
    raw.match(/^#\s+(.+)$/m)?.[1]?.trim() ??
    name.replace(/\.md$/, "");
  const abstract =
    fm.abstract ??
    body.slice(0, 800).trim();

  // Dry-run short-circuits: no Gemini call, no file moves.
  if (DRY_RUN) {
    appendLog({
      paper: name,
      result: "dry-run",
      doi,
      doi_hash: doiHash,
      title,
    });
    console.log(`[DRY] ${name}${doi ? ` (DOI: ${doi})` : ""}`);
    return;
  }

  const tx = { stagingPath: null, targetPath: null, completed: [] };

  // Step 1 — move to .staging/
  try {
    ensureDir(STAGING);
    tx.stagingPath = join(STAGING, name);
    moveFile(paperPath, tx.stagingPath);
    tx.completed.push("move-to-staging");
  } catch (e) {
    appendLog({
      paper: name,
      result: "failure",
      step: "move-to-staging",
      error: String(e),
    });
    console.error(`[FAIL] ${name}: move-to-staging — ${e}`);
    return;
  }

  // Step 2 — Gemini classification (rate limited)
  let category;
  try {
    category = await geminiLimiter.run(() =>
      classifyPaper(apiKey, title, abstract)
    );
    tx.completed.push("gemini-classify");
  } catch (e) {
    rollback(tx, name);
    appendLog({
      paper: name,
      result: "failure",
      step: "gemini-classify",
      error: String(e),
    });
    console.error(`[FAIL] ${name}: gemini-classify — ${e}`);
    return;
  }

  // Step 3 — move to {category}/
  try {
    tx.targetPath = join(PAPERS_DIR, category, name);
    moveFile(tx.stagingPath, tx.targetPath);
    tx.completed.push("move-to-category");
  } catch (e) {
    rollback(tx, name);
    appendLog({
      paper: name,
      result: "failure",
      step: "move-to-category",
      category,
      error: String(e),
    });
    console.error(`[FAIL] ${name}: move-to-category — ${e}`);
    return;
  }

  // Step 4 — Zotero collection update (best-effort)
  let zoteroSynced = false;
  let zoteroPending = false;
  const zoteroKey = fm.zotero_key ?? fm.zoteroKey ?? null;
  if (zoteroKey) {
    const up = await zoteroReachable();
    if (up) {
      try {
        const colKey = await ensureCollection(category);
        await setItemCollection(String(zoteroKey), colKey);
        tx.completed.push("zotero-collection-update");
        zoteroSynced = true;
      } catch (e) {
        queueForSync(name, category, String(zoteroKey));
        zoteroPending = true;
        console.warn(`[Zotero warn] ${name}: ${e.message} — queued`);
      }
    } else {
      queueForSync(name, category, String(zoteroKey));
      zoteroPending = true;
    }
  }

  // Step 5 — ZotMoov confirmation: handled in the Rust path with timeout +
  // smart polling.  Skipped in the Node CLI to avoid duplicating that logic.

  // Record DOI as processed (only after a successful move).
  if (doiHash) {
    const seen = loadProcessedDois();
    seen.dois[doiHash] = {
      paper: name,
      category,
      processed_at: nowIso(),
    };
    saveProcessedDois(seen);
  }

  appendLog({
    paper: name,
    result: "success",
    category,
    doi,
    doi_hash: doiHash,
    zotero_synced: zoteroSynced,
    zotero_pending: zoteroPending,
    steps_completed: tx.completed,
  });
  console.log(`[OK] ${name} → ${category}`);
}

function rollback(tx, name) {
  for (const step of [...tx.completed].reverse()) {
    try {
      if (step === "move-to-category" && tx.targetPath) {
        moveFile(tx.targetPath, join(UNCLASSIFIED, name));
      } else if (step === "move-to-staging" && tx.stagingPath) {
        moveFile(tx.stagingPath, join(UNCLASSIFIED, name));
      }
    } catch (e) {
      appendLog({
        paper: name,
        result: "rollback-error",
        step,
        error: String(e),
      });
    }
  }
}

// ── batch + watch entry points ─────────────────────────────────────────────────

function listUnclassified() {
  ensureDir(UNCLASSIFIED);
  return readdirSync(UNCLASSIFIED)
    .filter((f) => extname(f) === ".md")
    .map((f) => join(UNCLASSIFIED, f));
}

async function runBatch(paths, apiKey) {
  if (paths.length === 0) {
    console.log("No papers to organize.");
    return;
  }
  console.log(
    `Organizing ${paths.length} paper(s)${DRY_RUN ? " (dry run)" : ""}…`
  );
  // Kick off all in parallel; the limiter caps concurrent Gemini calls.
  await Promise.all(paths.map((p) => organizeOne(p, apiKey)));
  console.log("Done.");
}

function runWatch(apiKey) {
  ensureDir(UNCLASSIFIED);
  console.log(
    `Watching ${UNCLASSIFIED} for new .md files${DRY_RUN ? " (dry run)" : ""}…`
  );

  // Debounce per-file: editors often emit several "add" events as the file
  // is being written.  chokidar's awaitWriteFinish handles size stability,
  // but a small in-memory cooldown protects against duplicate processing
  // when files are dropped in rapid succession.
  const lastSeen = new Map();
  const watcher = chokidar.watch(UNCLASSIFIED, {
    persistent: true,
    ignoreInitial: false,
    awaitWriteFinish: { stabilityThreshold: 500, pollInterval: 100 },
    ignored: (path) => basename(path).startsWith("."),
  });

  watcher.on("add", (filePath) => {
    if (extname(filePath) !== ".md") return;
    const now = Date.now();
    const prev = lastSeen.get(filePath) ?? 0;
    if (now - prev < WATCH_DEBOUNCE_MS) return;
    lastSeen.set(filePath, now);
    organizeOne(filePath, apiKey).catch((e) => {
      console.error(`[watch] unhandled error for ${filePath}:`, e);
    });
  });

  watcher.on("error", (e) => console.error("[watch] watcher error:", e));

  const shutdown = async () => {
    console.log("\nShutting down watcher…");
    await watcher.close();
    process.exit(0);
  };
  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);
}

// ── main ───────────────────────────────────────────────────────────────────────
// Guard: skip execution when this file is imported as a module (e.g. by tests).
// `import.meta.url` is the file:// URL of this script; `process.argv[1]` is the
// path Node received on the command line.  They match only when run directly.

const isMain = Boolean(
  process.argv[1] &&
    fileURLToPath(import.meta.url) === process.argv[1]
);

if (isMain) {
  const apiKey = process.env.GEMINI_API_KEY;
  if (!apiKey && !DRY_RUN) {
    console.error(
      "GEMINI_API_KEY not set.  Export it for standalone use, or run with --dry-run."
    );
    process.exit(1);
  }

  if (WATCH) {
    runWatch(apiKey ?? "");
  } else {
    const targets = SINGLE_FILE ? [SINGLE_FILE] : listUnclassified();
    await runBatch(targets, apiKey ?? "");
  }
}

// ── exports (consumed by unit tests) ───────────────────────────────────────────
export {
  normalizeDoi,
  extractDoi,
  hashDoi,
  Limiter,
  classifyPaper,
};
