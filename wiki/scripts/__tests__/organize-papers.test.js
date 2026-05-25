/**
 * organize-papers.test.js
 *
 * Integration + unit tests for scripts/organize-papers.js.
 *
 * CLI tests (dry-run, duplicate, log) run the script as a child process with
 * env-var path overrides so they operate on a throw-away temp directory.
 *
 * Unit tests (frontmatter parsing, category normalisation, rate-limiter) import
 * the exported pure functions via a top-level ESM dynamic import.
 */

import {
  describe,
  test,
  expect,
  beforeEach,
  afterEach,
  jest,
} from "@jest/globals";
import { execSync } from "child_process";
import {
  mkdirSync,
  mkdtempSync,
  writeFileSync,
  readFileSync,
  existsSync,
  rmSync,
} from "fs";
import { tmpdir } from "os";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { createHash } from "crypto";

// ── module-level imports (top-level await is valid in ESM) ────────────────────

const __dirname = dirname(fileURLToPath(import.meta.url));
const SCRIPT    = join(__dirname, "..", "organize-papers.js");

// Import pure-function exports from the script (does NOT run main()).
const scriptModule = await import(SCRIPT);
const { extractDoi, hashDoi, Limiter, classifyPaper } = scriptModule;

// gray-matter is a prod dep; import separately for frontmatter assertions.
const { default: matter } = await import("gray-matter");

// ── shared helpers ────────────────────────────────────────────────────────────

/** Create an isolated temp tree and return paths. */
function makeTmpTree() {
  const root         = mkdtempSync(join(tmpdir(), "org-test-"));
  const content      = join(root, "content");
  const unclassified = join(content, "papers", "unclassified");
  const meta         = join(content, "meta");
  const logDir       = join(root, "logs");

  mkdirSync(unclassified,                          { recursive: true });
  mkdirSync(join(content, "papers", ".staging"),   { recursive: true });
  mkdirSync(meta,                                  { recursive: true });
  mkdirSync(logDir,                                { recursive: true });

  return { root, content, unclassified, meta, logDir };
}

/** Env block pointing the script at the temp tree. */
function testEnv(content, logDir, extra = {}) {
  return { ...process.env, ORGANIZE_CONTENT_ROOT: content, ORGANIZE_LOG_DIR: logDir, NO_COLOR: "1", ...extra };
}

/** Write a minimal .md paper. */
function writePaper(dir, name, { doi = null, title = "Test Paper" } = {}) {
  const lines = [
    "---",
    `title: "${title}"`,
    `abstract: "We study something important. Results are significant. Conclusion follows."`,
    ...(doi ? [`doi: "${doi}"`] : []),
    `authors: "Test Author"`,
    `year: 2024`,
    "---",
    "",
    "# Introduction",
    "Body text.",
  ];
  writeFileSync(join(dir, name), lines.join("\n"));
}

/** Today's log file entries (or []). */
function readLog(logDir) {
  const date    = new Date().toISOString().slice(0, 10);
  const logFile = join(logDir, `organize-${date}.json`);
  if (!existsSync(logFile)) return [];
  return JSON.parse(readFileSync(logFile, "utf8"));
}

/** Run the script as a child process; return { stdout, stderr, exitCode }. */
function runScript(args, env) {
  try {
    const stdout = execSync(`node ${SCRIPT} ${args}`, {
      env,
      encoding: "utf8",
      stdio: ["pipe", "pipe", "pipe"],
    });
    return { stdout, stderr: "", exitCode: 0 };
  } catch (err) {
    return { stdout: err.stdout ?? "", stderr: err.stderr ?? "", exitCode: err.status ?? 1 };
  }
}

// ── 1. dry-run mode ───────────────────────────────────────────────────────────

describe("dry-run mode", () => {
  let tree;

  beforeEach(() => {
    tree = makeTmpTree();
    writePaper(tree.unclassified, "paper-a.md", { doi: "10.1/dra", title: "Paper A" });
    writePaper(tree.unclassified, "paper-b.md", { doi: "10.1/drb", title: "Paper B" });
  });

  afterEach(() => rmSync(tree.root, { recursive: true, force: true }));

  test("files are NOT moved in dry-run mode", () => {
    runScript("--dry-run", testEnv(tree.content, tree.logDir));
    expect(existsSync(join(tree.unclassified, "paper-a.md"))).toBe(true);
    expect(existsSync(join(tree.unclassified, "paper-b.md"))).toBe(true);
  });

  test("log shows what WOULD happen (result: dry-run)", () => {
    runScript("--dry-run", testEnv(tree.content, tree.logDir));

    const entries = readLog(tree.logDir);
    expect(entries.length).toBeGreaterThanOrEqual(2);

    entries.forEach((e) => expect(e.result).toBe("dry-run"));

    const papers = entries.map((e) => e.paper);
    expect(papers).toContain("paper-a.md");
    expect(papers).toContain("paper-b.md");
  });
});

// ── 2. frontmatter parsing ────────────────────────────────────────────────────

describe("frontmatter parsing", () => {
  test("title, doi, authors correctly extracted from valid frontmatter", () => {
    const md = `---
title: "Attention Is All You Need"
doi: "10.48550/arXiv.1706.03762"
authors: "Vaswani et al."
abstract: "We propose the Transformer."
year: 2017
---

Body.
`;
    const { data: fm, content: body } = matter(md);

    expect(fm.title).toBe("Attention Is All You Need");
    expect(fm.authors).toBe("Vaswani et al.");
    expect(fm.year).toBe(2017);
    expect(extractDoi(fm, body)).toBe("10.48550/arXiv.1706.03762");
  });

  test("missing doi field handled gracefully — returns null, no crash", () => {
    const md = `---
title: "No DOI Paper"
authors: "Author One"
abstract: "No DOI here."
---

Body.
`;
    const { data: fm, content: body } = matter(md);
    let doi;
    expect(() => { doi = extractDoi(fm, body); }).not.toThrow();
    expect(doi).toBeNull();
  });

  test("doi extracted from body when absent from frontmatter", () => {
    const md = `---
title: "Body DOI Paper"
abstract: "Mentions a DOI inline."
---

See https://doi.org/10.1234/body.doi for details.
`;
    const { data: fm, content: body } = matter(md);
    expect(extractDoi(fm, body)).toBe("10.1234/body.doi");
  });
});

// ── 3. category name validation ───────────────────────────────────────────────

describe("category name validation", () => {
  let savedFetch;

  beforeEach(() => { savedFetch = global.fetch; });
  afterEach(() => { global.fetch = savedFetch; });

  function mockGemini(rawText) {
    global.fetch = jest.fn().mockResolvedValue({
      ok: true,
      json: async () => ({
        candidates: [{ content: { parts: [{ text: rawText }] } }],
      }),
    });
  }

  test('"Computer Vision" → "computer-vision"', async () => {
    mockGemini("Computer Vision");
    expect(await classifyPaper("fake-key", "T", "A")).toBe("computer-vision");
  });

  test('"ML_Research" → "ml-research"', async () => {
    mockGemini("ML_Research");
    expect(await classifyPaper("fake-key", "T", "A")).toBe("ml-research");
  });

  test('"MEDICAL IMAGING" → "medical-imaging"', async () => {
    mockGemini("MEDICAL IMAGING");
    expect(await classifyPaper("fake-key", "T", "A")).toBe("medical-imaging");
  });

  test("already-valid category passes through unchanged", async () => {
    mockGemini("large-language-models");
    expect(await classifyPaper("fake-key", "T", "A")).toBe("large-language-models");
  });
});

// ── 4. duplicate detection ────────────────────────────────────────────────────

describe("duplicate detection", () => {
  let tree;

  beforeEach(() => { tree = makeTmpTree(); });
  afterEach(() => rmSync(tree.root, { recursive: true, force: true }));

  test("file with already-processed DOI is skipped (result: skipped)", () => {
    const doi     = "10.9999/already-seen";
    const doiHash = hashDoi(doi);

    // Pre-populate the processed-DOIs record.
    writeFileSync(
      join(tree.meta, ".processed-dois.json"),
      JSON.stringify({
        dois: {
          [doiHash]: {
            paper: "old.md",
            category: "old-category",
            processed_at: "2026-01-01T00:00:00Z",
          },
        },
      })
    );

    writePaper(tree.unclassified, "dup-paper.md", { doi });

    // Run WITHOUT --dry-run; provide a fake API key so the process doesn't
    // exit before processing files.  The duplicate check fires before any
    // Gemini call, so the fake key is never used.
    runScript("", testEnv(tree.content, tree.logDir, { GEMINI_API_KEY: "fake-key-not-real" }));

    // File must still be in unclassified/
    expect(existsSync(join(tree.unclassified, "dup-paper.md"))).toBe(true);

    // Log entry must be "skipped" with reason "duplicate-doi"
    const entries = readLog(tree.logDir);
    const entry   = entries.find((e) => e.paper === "dup-paper.md");
    expect(entry).toBeDefined();
    expect(entry.result).toBe("skipped");
    expect(entry.reason).toBe("duplicate-doi");
  });
});

// ── 5. rate limiting ─────────────────────────────────────────────────────────

describe("rate limiting", () => {
  test("Limiter never exceeds max concurrent tasks", async () => {
    const MAX     = 5;
    const limiter = new Limiter(MAX);

    let concurrent    = 0;
    let maxConcurrent = 0;

    const tasks = Array.from({ length: 10 }, () =>
      limiter.run(async () => {
        concurrent++;
        maxConcurrent = Math.max(maxConcurrent, concurrent);
        await new Promise((r) => setTimeout(r, 50));
        concurrent--;
      })
    );

    await Promise.all(tasks);

    expect(maxConcurrent).toBeLessThanOrEqual(MAX);
    expect(maxConcurrent).toBeGreaterThan(0);
    expect(concurrent).toBe(0); // all tasks finished
  });

  test("Limiter(1) serialises tasks in submission order", async () => {
    const limiter = new Limiter(1);
    const order   = [];

    await Promise.all([
      limiter.run(async () => { await new Promise((r) => setTimeout(r, 30)); order.push(1); }),
      limiter.run(async () => { await new Promise((r) => setTimeout(r, 10)); order.push(2); }),
      limiter.run(async () => { order.push(3); }),
    ]);

    expect(order).toEqual([1, 2, 3]);
  });
});

// ── 6. log file creation ──────────────────────────────────────────────────────

describe("log file creation", () => {
  let tree;

  beforeEach(() => { tree = makeTmpTree(); });
  afterEach(() => rmSync(tree.root, { recursive: true, force: true }));

  test("logs/organize-YYYY-MM-DD.json is created after a run", () => {
    writePaper(tree.unclassified, "lp1.md", { doi: "10.1/l1" });
    writePaper(tree.unclassified, "lp2.md", { doi: "10.1/l2" });

    runScript("--dry-run", testEnv(tree.content, tree.logDir));

    const date    = new Date().toISOString().slice(0, 10);
    const logFile = join(tree.logDir, `organize-${date}.json`);
    expect(existsSync(logFile)).toBe(true);
  });

  test("log contains one entry per paper", () => {
    writePaper(tree.unclassified, "la.md", { doi: "10.2/a" });
    writePaper(tree.unclassified, "lb.md", { doi: "10.2/b" });

    runScript("--dry-run", testEnv(tree.content, tree.logDir));

    const entries = readLog(tree.logDir);
    expect(entries.length).toBe(2);

    const papers = entries.map((e) => e.paper);
    expect(papers).toContain("la.md");
    expect(papers).toContain("lb.md");
  });

  test("each log entry has a valid ISO-8601 timestamp", () => {
    writePaper(tree.unclassified, "ts.md", { doi: "10.3/ts" });

    runScript("--dry-run", testEnv(tree.content, tree.logDir));

    readLog(tree.logDir).forEach((e) => {
      expect(typeof e.timestamp).toBe("string");
      expect(() => new Date(e.timestamp).toISOString()).not.toThrow();
    });
  });
});
