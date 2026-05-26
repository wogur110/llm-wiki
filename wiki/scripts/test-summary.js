#!/usr/bin/env node
/**
 * scripts/test-summary.js
 *
 * Aggregated test + coverage report for LLM-Wiki.
 *
 * Runs every suite once (so the script doubles as a CI gate) and then reads
 * the on-disk coverage reports for the per-module breakdown.  Output:
 *
 *   Test summary
 *   ────────────────────────────────────────────────
 *     Rust         N/N passed   (exit 0)
 *     Node scripts N/N passed   (exit 0)
 *     Frontend     N/N passed   (exit 0)
 *     TOTAL        N/N passed,  0 failed
 *
 *   Coverage
 *   ────────────────────────────────────────────────
 *     Frontend  XX.X% lines  ████████░░░░░░░░░░
 *     Rust      XX.X% lines  ████████░░░░░░░░░░
 *
 *   Frontend per-file coverage (low → high)
 *     ...
 *
 * Exit code:
 *   0 — every suite passed AND both languages ≥ 70% line coverage.
 *   1 — otherwise.
 *
 * Files consulted:
 *   - wiki/coverage/frontend/coverage-summary.json  (vitest v8, json-summary reporter)
 *   - coverage/rust/tarpaulin-report.json           (cargo-tarpaulin --out Json)
 */

import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

// ── Paths ─────────────────────────────────────────────────────────────────────

const __dirname = dirname(fileURLToPath(import.meta.url))
const WIKI_ROOT = resolve(__dirname, '..')   //   .../wiki
const REPO_ROOT = resolve(WIKI_ROOT, '..')   //   .../llm-wiki

const COVERAGE_THRESHOLD = 70
/**
 * Rust gate uses only testable business-logic modules.
 * Excludes lib/main/gemini HTTP and pending_sync (sync_all needs a live
 * Tauri window + Zotero — covered by integration/manual runs, not unit tests).
 */
const RUST_CORE_MODULE_RE =
  /src-tauri\/src\/(content|organizer|transaction|keychain)\.rs$/
const FRONTEND_COVERAGE_JSON = join(
  WIKI_ROOT,
  'coverage',
  'frontend',
  'coverage-summary.json',
)
// cargo-tarpaulin resolves `--output-dir` relative to the manifest, so
// `--output-dir coverage/rust` (run from `wiki/`) lands the report at
// `wiki/coverage/rust/`.  Same place we keep the frontend report.
const RUST_COVERAGE_JSON = join(WIKI_ROOT, 'coverage', 'rust', 'tarpaulin-report.json')

// ── Runner wrapper ────────────────────────────────────────────────────────────

/**
 * Spawn a child synchronously, capture combined stdout+stderr, and mirror it
 * to the parent terminal so a developer still sees live progress.  We can't
 * use `stdio: 'inherit'` because we need the captured text for regex parsing.
 */
function runCaptured(label, cmd, args, cwd) {
  process.stdout.write(`\n► ${label}\n`)
  const r = spawnSync(cmd, args, { cwd, encoding: 'utf8' })
  const out = (r.stdout ?? '') + (r.stderr ?? '')
  process.stdout.write(out)
  return { exitCode: r.status ?? 1, out }
}

// ── Per-runner output parsers ─────────────────────────────────────────────────

/** Vitest:  "Tests   3 failed | 21 passed (24)"  /  "Tests   24 passed (24)" */
function parseVitest(out) {
  const m = out.match(
    /Tests\s+(?:(\d+)\s+failed\s+\|\s+)?(\d+)\s+passed(?:\s+\|\s+(\d+)\s+skipped)?\s+\((\d+)\)/,
  )
  if (!m) return null
  return {
    failed: parseInt(m[1] ?? '0', 10),
    passed: parseInt(m[2], 10),
    total: parseInt(m[4], 10),
  }
}

/** Jest:    "Tests:       1 failed, 14 passed, 15 total" */
function parseJest(out) {
  const m = out.match(
    /Tests:\s+(?:(\d+)\s+failed,\s+)?(\d+)\s+passed,\s+(\d+)\s+total/,
  )
  if (!m) return null
  return {
    failed: parseInt(m[1] ?? '0', 10),
    passed: parseInt(m[2], 10),
    total: parseInt(m[3], 10),
  }
}

/** Cargo:   "test result: ok. N passed; M failed; ..."  (sum across binaries) */
function parseCargoTest(out) {
  let passed = 0
  let failed = 0
  for (const m of out.matchAll(/test result:.*?(\d+)\s+passed;\s+(\d+)\s+failed/g)) {
    passed += parseInt(m[1], 10)
    failed += parseInt(m[2], 10)
  }
  if (passed === 0 && failed === 0) return null
  return { passed, failed, total: passed + failed }
}

// ── Coverage report readers ───────────────────────────────────────────────────

function readFrontendCoverage() {
  if (!existsSync(FRONTEND_COVERAGE_JSON)) return null
  const j = JSON.parse(readFileSync(FRONTEND_COVERAGE_JSON, 'utf8'))
  const t = j.total
  return {
    lines: t.lines.pct,
    statements: t.statements.pct,
    functions: t.functions.pct,
    branches: t.branches.pct,
    perFile: Object.entries(j)
      .filter(([k]) => k !== 'total')
      .map(([file, v]) => ({
        file: file.replace(WIKI_ROOT + '/', '').replace(WIKI_ROOT, ''),
        lines: v.lines.pct,
      })),
  }
}

function readRustCoverage() {
  if (!existsSync(RUST_COVERAGE_JSON)) return null
  const j = JSON.parse(readFileSync(RUST_COVERAGE_JSON, 'utf8'))

  // tarpaulin's JSON format (0.35.x): paths are stored as an array of
  // segments starting with "/" → we join them and collapse the leading
  // double-slash, then make the path repo-relative for display.
  const formatPath = (raw) => {
    const joined = Array.isArray(raw) ? raw.join('/').replace(/^\/+/, '/') : String(raw)
    return joined.replace(REPO_ROOT + '/', '').replace(WIKI_ROOT + '/', '')
  }

  let covered = 0
  let coverable = 0
  let coreCovered = 0
  let coreCoverable = 0
  const perFile = []
  for (const f of j.files ?? []) {
    const fCovered = f.covered ?? (f.traces?.filter((t) => t.stats?.Line > 0).length ?? 0)
    const fCoverable = f.coverable ?? (f.traces?.length ?? 0)
    covered += fCovered
    coverable += fCoverable
    const file = formatPath(f.path)
    const isCore = RUST_CORE_MODULE_RE.test(file)
    if (isCore) {
      coreCovered += fCovered
      coreCoverable += fCoverable
    }
    perFile.push({
      file,
      lines: fCoverable > 0 ? (fCovered / fCoverable) * 100 : 0,
      coverable: fCoverable,
      isCore,
    })
  }
  const totalPct = coverable > 0 ? (covered / coverable) * 100 : (j.coverage ?? 0)
  const coreLines =
    coreCoverable > 0 ? (coreCovered / coreCoverable) * 100 : totalPct
  const measured = perFile.filter((p) => p.coverable > 0)
  return { lines: totalPct, coreLines, perFile: measured }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function bar(pct, width = 20) {
  const n = Math.max(0, Math.min(width, Math.round((pct / 100) * width)))
  return '█'.repeat(n) + '░'.repeat(width - n)
}

function colour(text, code) {
  if (process.env.NO_COLOR || !process.stdout.isTTY) return text
  return `\x1b[${code}m${text}\x1b[0m`
}
const red    = (t) => colour(t, '31')
const green  = (t) => colour(t, '32')
const yellow = (t) => colour(t, '33')
const dim    = (t) => colour(t, '2')

// ── Run every suite ───────────────────────────────────────────────────────────

const rust = runCaptured(
  'cargo test',
  'cargo',
  ['test', '--manifest-path', 'src-tauri/Cargo.toml'],
  WIKI_ROOT,
)
const rustCounts = parseCargoTest(rust.out) ??
  { passed: 0, failed: rust.exitCode === 0 ? 0 : 1, total: 0 }

const scripts = runCaptured(
  'jest (scripts)',
  'npm',
  ['run', 'test:scripts', '--silent'],
  WIKI_ROOT,
)
const scriptsCounts = parseJest(scripts.out) ??
  { passed: 0, failed: scripts.exitCode === 0 ? 0 : 1, total: 0 }

const frontend = runCaptured(
  'vitest + coverage (frontend)',
  'npm',
  ['run', 'test:coverage:frontend', '--silent'],
  WIKI_ROOT,
)
const frontendCounts = parseVitest(frontend.out) ??
  { passed: 0, failed: frontend.exitCode === 0 ? 0 : 1, total: 0 }

const rustCov = runCaptured(
  'cargo-tarpaulin (rust coverage)',
  'npm',
  ['run', 'test:coverage:rust', '--silent'],
  WIKI_ROOT,
)
// Coverage runs also propagate exit codes — if tarpaulin failed to run we
// still want to know.
const feCov = readFrontendCoverage()
const ruCov = readRustCoverage()

// ── Print consolidated summary ────────────────────────────────────────────────

const totalTests  = rustCounts.total  + scriptsCounts.total  + frontendCounts.total
const totalPassed = rustCounts.passed + scriptsCounts.passed + frontendCounts.passed
const totalFailed = rustCounts.failed + scriptsCounts.failed + frontendCounts.failed

const allTestsExitOk =
  rust.exitCode === 0 && scripts.exitCode === 0 && frontend.exitCode === 0

console.log('')
console.log('═'.repeat(72))
console.log('  Test summary')
console.log('═'.repeat(72))
const row = (name, c, exit) => {
  const status = exit === 0 ? green('✓') : red('✗')
  console.log(
    `  ${status} ${name.padEnd(14)} ${String(c.passed).padStart(3)}/${String(c.total).padEnd(3)} passed` +
      `   ${dim(`(exit ${exit})`)}`,
  )
}
row('Rust',         rustCounts,     rust.exitCode)
row('Node scripts', scriptsCounts,  scripts.exitCode)
row('Frontend',     frontendCounts, frontend.exitCode)
console.log('  ' + '─'.repeat(48))
console.log(
  `  ${allTestsExitOk && totalFailed === 0 ? green('✓') : red('✗')} TOTAL          ` +
    `${String(totalPassed).padStart(3)}/${String(totalTests).padEnd(3)} passed, ${totalFailed} failed`,
)

console.log('')
console.log('  Coverage')
console.log('  ' + '─'.repeat(48))
if (feCov) {
  const ok = feCov.lines >= COVERAGE_THRESHOLD
  console.log(
    `  ${ok ? green('✓') : red('✗')} Frontend   ${feCov.lines.toFixed(1).padStart(5)}%  ${bar(feCov.lines)}` +
      `  ${dim(`(stmts ${feCov.statements.toFixed(0)}%  fns ${feCov.functions.toFixed(0)}%  br ${feCov.branches.toFixed(0)}%)`)}`,
  )
} else {
  console.log(`  ${yellow('?')} Frontend   ${dim('no coverage-summary.json found')}`)
}
if (ruCov) {
  const coreOk = ruCov.coreLines >= COVERAGE_THRESHOLD
  console.log(
    `  ${coreOk ? green('✓') : red('✗')} Rust (core)  ${ruCov.coreLines.toFixed(1).padStart(5)}%  ${bar(ruCov.coreLines)}  ${dim('(content/organizer/tx/keychain)')}`,
  )
  console.log(
    `  ${dim('○')} Rust (all)   ${ruCov.lines.toFixed(1).padStart(5)}%  ${bar(ruCov.lines)}  ${dim('(includes lib/gemini/main)')}`,
  )
} else {
  console.log(`  ${yellow('?')} Rust       ${dim('no tarpaulin-report.json found')}`)
}

if (feCov && feCov.perFile.length) {
  console.log('')
  console.log('  Frontend per-module (lowest first):')
  feCov.perFile
    .sort((a, b) => a.lines - b.lines)
    .slice(0, 15)
    .forEach((f) => {
      const pct = `${f.lines.toFixed(1)}%`.padStart(7)
      const tag = f.lines >= COVERAGE_THRESHOLD ? green(pct) : red(pct)
      console.log(`    ${tag}  ${f.file}`)
    })
}

if (ruCov && ruCov.perFile.length) {
  console.log('')
  console.log('  Rust core modules (lowest first):')
  ruCov.perFile
    .filter((f) => f.isCore)
    .sort((a, b) => a.lines - b.lines)
    .slice(0, 15)
    .forEach((f) => {
      const pct = `${f.lines.toFixed(1)}%`.padStart(7)
      const tag = f.lines >= COVERAGE_THRESHOLD ? green(pct) : red(pct)
      console.log(`    ${tag}  ${f.file}`)
    })
}

console.log('')

// ── Gate ──────────────────────────────────────────────────────────────────────

const testsFailed = !allTestsExitOk || totalFailed > 0
const coverageFailed =
  (feCov && feCov.lines < COVERAGE_THRESHOLD) ||
  (ruCov && ruCov.coreLines < COVERAGE_THRESHOLD)

if (testsFailed) {
  console.error(red(`✗  ${totalFailed} test(s) failed across suites.`))
}
if (feCov && feCov.lines < COVERAGE_THRESHOLD) {
  console.error(
    red(
      `✗  Frontend line coverage ${feCov.lines.toFixed(1)}% < ${COVERAGE_THRESHOLD}% threshold`,
    ),
  )
}
if (ruCov && ruCov.coreLines < COVERAGE_THRESHOLD) {
  console.error(
    red(
      `✗  Rust core module coverage ${ruCov.coreLines.toFixed(1)}% < ${COVERAGE_THRESHOLD}% threshold`,
    ),
  )
}

if (testsFailed || coverageFailed) {
  process.exit(1)
}

console.log(green('✓  All checks passed.'))
process.exit(0)
