//! Paper organiser — orchestrates the 5-step transaction pipeline.
//!
//! # Tauri commands exposed
//! | Command         | Returns                     | Description                     |
//! |-----------------|-----------------------------|---------------------------------|
//! | `process_paper` | `Result<ProcessResult, String>` | Run the full organise pipeline |
//!
//! ## Pipeline overview
//! ```text
//! 1. DuplicateDetection      reject if DOI already in .processed-dois.json
//! 2. MovedToStaging          unclassified/ → .staging/
//! 3. GeminiClassified        classify via Gemini API (or override in tests)
//! 4. MovedToTarget           .staging/     → {category}/
//!   ── FrontmatterUpdate     inject tags + summary into the markdown file
//!   ┌─ Zotero online ──────────────────────────────────────────────────────────
//! 5. ZoteroCollectionChanged update Zotero item collection
//! 6. ZotMovConfirmed         wait for ZotMoov to move PDF (if path given)
//!   └─ Zotero offline → enqueue steps 5-6 for later replay
//! ```
//! Any failure triggers `PaperTransaction::rollback()` and logs to
//! `logs/organize-YYYY-MM-DD.json`.  The file **always** returns to
//! `unclassified/`.
//!
//! ## Events emitted (event: `"tx-progress"`)
//! ```json
//! { "step": "MovedToStaging", "status": "started|done|skipped|failed",
//!   "detail": "<optional string>" }
//! ```

use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::Emitter;

use crate::transaction::{PaperTransaction, TxError, TxStep};

// ── Config (path helpers + shared HTTP client) ─────────────────────────────────

/// Paths and shared resources for one organise run.
pub struct OrganizerConfig {
    /// Absolute path to `content/` (parent of `papers/`, `meta/`).
    pub content_root: PathBuf,
    /// Shared HTTP client (re-used across pipeline steps).
    pub http_client: Client,
}

impl OrganizerConfig {
    fn new(content_root: PathBuf) -> Self {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("organiser: failed to build HTTP client");
        Self { content_root, http_client }
    }

    /// `content/papers/unclassified/`
    pub fn unclassified_dir(&self) -> PathBuf {
        self.content_root.join("papers").join("unclassified")
    }

    /// `content/papers/.staging/`
    pub fn staging_dir(&self) -> PathBuf {
        self.content_root.join("papers").join(".staging")
    }

    /// `content/papers/{category}/`
    pub fn category_dir(&self, category: &str) -> PathBuf {
        self.content_root.join("papers").join(category)
    }

    /// `logs/organize-YYYY-MM-DD.json`  (sibling of `content/`)
    pub fn log_path(&self) -> PathBuf {
        let date = Utc::now().format("%Y-%m-%d");
        self.content_root
            .parent()
            .unwrap_or(&self.content_root)
            .join("logs")
            .join(format!("organize-{date}.json"))
    }

    /// `content/meta/pending-zotero-sync.json`
    pub fn queue_path(&self) -> PathBuf {
        self.content_root.join("meta").join("pending-zotero-sync.json")
    }

    /// `content/meta/.processed-dois.json`
    pub fn dois_path(&self) -> PathBuf {
        self.content_root.join("meta").join(".processed-dois.json")
    }
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Returned to JS when the pipeline completes (success path).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProcessResult {
    /// Lower-case kebab-case category assigned by Gemini.
    pub category: String,
    /// Absolute path where the markdown file now lives.
    pub final_path: String,
    /// `true` if Zotero collection was updated in this run.
    pub zotero_synced: bool,
    /// `true` if the Zotero update was deferred to the pending-sync queue.
    pub zotero_pending: bool,
}

/// How the pipeline should handle the physical PDF after classification.
///
/// In all branches that wait for the PDF, the wait uses
/// [`crate::zotero::wait_for_zotmoov`], which polls for the file on disk.
#[derive(Debug, Clone)]
pub enum PdfMoveSpec {
    /// Skip the PDF-confirmation step entirely.
    None,
    /// Wait for `path` (already fully resolved) to appear.
    StaticPath { path: PathBuf, timeout_secs: u64 },
    /// Wait for `root/<category>/filename` to appear, where `<category>` is
    /// the value produced by Gemini classification at runtime.  Used by the
    /// PDF importer when ZotMoov is configured with `subfolder = {collection}`.
    PerCategory { root: PathBuf, filename: String, timeout_secs: u64 },
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Progress event payload — `Serialize + Clone` required by Tauri 2 `emit`.
#[derive(Serialize, Clone)]
struct TxProgress {
    step: String,
    status: String,
    detail: Option<String>,
}

/// Simple YAML-frontmatter fields extracted from Zotero-exported markdown.
struct Frontmatter {
    title: String,
    abstract_text: String,
    doi: Option<String>,
}

/// Extract `title`, `abstract`, and `DOI` from a `---`-delimited YAML block.
///
/// Falls back to empty strings if the file has no frontmatter or a field is
/// absent — Gemini can still classify from the body alone.
fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut title = String::new();
    let mut abstract_text = String::new();
    let mut doi: Option<String> = None;

    if !content.starts_with("---") {
        return Frontmatter { title, abstract_text, doi };
    }

    // Find the closing `---` (skip the opening one).
    let after_open = content.trim_start_matches("---\n");
    let end = after_open.find("\n---").unwrap_or(after_open.len());
    let yaml = &after_open[..end];

    for line in yaml.lines() {
        // Simple key: value parsing — handles quoted and unquoted values.
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("title:") {
            title = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = trimmed.strip_prefix("abstract:") {
            abstract_text = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = trimmed.strip_prefix("DOI:").or_else(|| trimmed.strip_prefix("doi:")) {
            let raw = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !raw.is_empty() {
                doi = Some(raw);
            }
        }
    }

    Frontmatter { title, abstract_text, doi }
}

/// Move a file synchronously, creating the destination directory if needed.
pub fn move_file(src: &Path, dst: &Path) -> Result<(), anyhow::Error> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(src, dst)?;
    Ok(())
}

/// Append a `TxError` entry to the daily JSON log.  Best-effort.
pub fn log_error(config: &OrganizerConfig, err: &TxError) {
    let log_path = config.log_path();

    let mut entries: Vec<serde_json::Value> = if log_path.exists() {
        std::fs::read_to_string(&log_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if let Ok(v) = serde_json::to_value(err) {
        entries.push(v);
    }

    if let Ok(text) = serde_json::to_string_pretty(&entries) {
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&log_path, text);
    }
}

/// Append a success entry to the daily JSON log.  Best-effort.
fn log_success(config: &OrganizerConfig, paper_path: &str, category: &str) {
    let entry = serde_json::json!({
        "result": "success",
        "paper_path": paper_path,
        "category": category,
        "timestamp": Utc::now().to_rfc3339(),
    });

    let log_path = config.log_path();

    let mut entries: Vec<serde_json::Value> = if log_path.exists() {
        std::fs::read_to_string(&log_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    entries.push(entry);

    if let Ok(text) = serde_json::to_string_pretty(&entries) {
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&log_path, text);
    }
}

// ── Duplicate DOI detection ────────────────────────────────────────────────────

/// Return `true` if `doi` is already recorded in `.processed-dois.json`.
fn is_duplicate_doi(dois_path: &Path, doi: &str) -> bool {
    if !dois_path.exists() {
        return false;
    }
    let text = std::fs::read_to_string(dois_path).unwrap_or_default();
    let map: serde_json::Value =
        serde_json::from_str(&text).unwrap_or(serde_json::json!({"dois": {}}));
    map["dois"].get(doi).is_some()
}

/// Persist a processed DOI to `.processed-dois.json` so duplicates are detected
/// on future runs.  Creates the file and parent directories if needed.
fn record_doi_processed(
    dois_path: &Path,
    doi: &str,
    file: &str,
    category: &str,
) -> Result<(), String> {
    let mut map: serde_json::Value = if dois_path.exists() {
        std::fs::read_to_string(dois_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(serde_json::json!({"dois": {}}))
    } else {
        serde_json::json!({"dois": {}})
    };

    if let Some(dois) = map["dois"].as_object_mut() {
        dois.insert(
            doi.to_string(),
            serde_json::json!({
                "file": file,
                "category": category,
                "processed_at": Utc::now().to_rfc3339(),
            }),
        );
    }

    if let Some(parent) = dois_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
    std::fs::write(dois_path, text).map_err(|e| e.to_string())?;
    Ok(())
}

// ── Frontmatter update ─────────────────────────────────────────────────────────

/// Take the first ≤ 3 sentences from `abstract_text` (split on `". "`).
fn extract_summary(abstract_text: &str) -> String {
    let text = abstract_text.trim();
    let mut sentences: Vec<&str> = Vec::new();
    let mut remaining = text;
    for _ in 0..3 {
        if let Some(pos) = remaining.find(". ") {
            sentences.push(&remaining[..pos + 1]); // include the period
            remaining = &remaining[pos + 2..]; // skip ". "
        } else {
            if !remaining.is_empty() {
                sentences.push(remaining);
            }
            break;
        }
    }
    sentences.join(" ")
}

/// Inject / replace `tags` and `summary` fields in the file's YAML frontmatter.
///
/// Existing `tags:` and `summary:` blocks are stripped before the new values
/// are appended, making repeated calls idempotent.
///
/// Returns `Ok(())` on success; propagates I/O errors as `Err(String)`.
fn update_file_frontmatter(
    file_path: &Path,
    category: &str,
    abstract_text: &str,
) -> Result<(), String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Cannot read file for frontmatter update: {e}"))?;

    let summary = extract_summary(abstract_text);

    let updated = if content.starts_with("---\n") {
        // Locate the closing `---` delimiter.
        if let Some(close_pos) = content[4..].find("\n---") {
            let fm_content = &content[4..4 + close_pos]; // YAML between delimiters
            let rest = &content[4 + close_pos..]; // starts with "\n---"

            // Strip any existing tags / summary entries.
            let mut in_tags_block = false;
            let mut new_fm_lines: Vec<&str> = Vec::new();
            for line in fm_content.lines() {
                let t = line.trim();
                if t.starts_with("tags:") {
                    in_tags_block = true;
                    continue;
                }
                if t.starts_with("summary:") {
                    in_tags_block = false;
                    continue;
                }
                // Skip list items that belong to the tags block.
                if in_tags_block {
                    if line.starts_with("  ") || line.starts_with('\t') || t.starts_with("- ") {
                        continue;
                    }
                    in_tags_block = false;
                }
                new_fm_lines.push(line);
            }

            let new_fm = new_fm_lines.join("\n");
            format!(
                "---\n{new_fm}\ntags:\n  - {category}\nsummary: {summary}{rest}"
            )
        } else {
            content
        }
    } else {
        content
    };

    std::fs::write(file_path, updated)
        .map_err(|e| format!("Cannot write frontmatter update: {e}"))
}

// ── Core pipeline (testable without a Tauri runtime) ─────────────────────────

/// Organise a single paper through the full pipeline.
///
/// This function contains all the business logic and is called both by the
/// production Tauri command and the integration-test suite.
///
/// # Parameters
/// * `paper_path`        — absolute path to the markdown file in `unclassified/`
/// * `content_root`      — absolute path to `content/`
/// * `expected_pdf_path` — absolute path ZotMoov should write the PDF to (step 5);
///                         `None` skips step 5
/// * `emit_fn`           — called for every pipeline step event with
///                         `(step_name, status, optional_detail)`; the Tauri
///                         command passes a closure that calls `window.emit`;
///                         integration tests pass a no-op.
/// * `gemini_override`   — if `Some`, replaces the live Gemini API call in
///                         step 2; pass `None` in production
///
/// # Errors
/// Returns a human-readable error string on failure.
/// The paper file is guaranteed to be back in `unclassified/` on any error.
pub async fn process_paper_core<F>(
    paper_path: String,
    content_root: String,
    pdf_move: PdfMoveSpec,
    emit_fn: F,
    gemini_override: Option<Result<String, String>>,
) -> Result<ProcessResult, String>
where
    F: Fn(&str, &str, Option<&str>) + Send,
{
    let content_root_buf = PathBuf::from(&content_root);
    let paper_path_buf = PathBuf::from(&paper_path);

    let config = OrganizerConfig::new(content_root_buf.clone());

    // ── Read file & parse metadata ────────────────────────────────────────────
    let content = tokio::fs::read_to_string(&paper_path_buf)
        .await
        .map_err(|e| format!("Cannot read paper file: {e}"))?;

    let fm = parse_frontmatter(&content);
    let file_name = paper_path_buf
        .file_name()
        .unwrap_or_default()
        .to_os_string();

    // ── Duplicate DOI check ───────────────────────────────────────────────────
    if let Some(ref doi) = fm.doi {
        if is_duplicate_doi(&config.dois_path(), doi) {
            let msg = format!("duplicate DOI: {doi}");
            log_error(
                &config,
                &TxError::new("DuplicateDetection", &msg, &paper_path),
            );
            return Err(format!("[DuplicateDetection] {msg}"));
        }
    }

    // Check Zotero once upfront (single ping, no retry).
    let zotero_online = crate::zotero::ping(&config.http_client).await;

    let paper_str = paper_path_buf.to_string_lossy().into_owned();
    let mut tx = PaperTransaction::new(paper_path_buf.clone());

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 1  Move to .staging/
    // ──────────────────────────────────────────────────────────────────────────
    let staging_path = config.staging_dir().join(&file_name);
    emit_fn("MovedToStaging", "started", None);

    if let Err(e) = move_file(&paper_path_buf, &staging_path) {
        emit_fn("MovedToStaging", "failed", Some(&e.to_string()));
        // Nothing completed yet; no rollback needed.
        log_error(&config, &TxError::new("MovedToStaging", &e.to_string(), &paper_str));
        return Err(format!("[MovedToStaging] {e}"));
    }

    tx.record_step(TxStep::MovedToStaging { staging_path: staging_path.clone() });
    emit_fn("MovedToStaging", "done", None);

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 2  Gemini classification
    // ──────────────────────────────────────────────────────────────────────────
    emit_fn("GeminiClassified", "started", None);

    let category = match gemini_override {
        Some(override_result) => match override_result {
            Ok(cat) => cat,
            Err(e) => {
                emit_fn("GeminiClassified", "failed", Some(&e));
                let _ = tx.rollback(&content_root_buf).await;
                log_error(&config, &TxError::new("GeminiClassified", &e, &paper_str));
                return Err(format!("[GeminiClassified] {e}"));
            }
        },
        None => match crate::gemini::classify_paper(
            fm.title.clone(),
            fm.abstract_text.clone(),
            content.clone(),
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                emit_fn("GeminiClassified", "failed", Some(&e));
                let _ = tx.rollback(&content_root_buf).await;
                log_error(&config, &TxError::new("GeminiClassified", &e, &paper_str));
                return Err(format!("[GeminiClassified] {e}"));
            }
        },
    };

    tx.record_step(TxStep::GeminiClassified { category: category.clone() });
    emit_fn("GeminiClassified", "done", Some(&category));

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 3  Move to {category}/
    // ──────────────────────────────────────────────────────────────────────────
    let target_path = config.category_dir(&category).join(&file_name);
    emit_fn("MovedToTarget", "started", None);

    if let Err(e) = move_file(&staging_path, &target_path) {
        emit_fn("MovedToTarget", "failed", Some(&e.to_string()));
        let _ = tx.rollback(&content_root_buf).await;
        log_error(&config, &TxError::new("MovedToTarget", &e.to_string(), &paper_str));
        return Err(format!("[MovedToTarget] {e}"));
    }

    tx.record_step(TxStep::MovedToTarget { target_path: target_path.clone() });
    emit_fn("MovedToTarget", "done", None);

    // ── Update frontmatter with category tags + abstract summary (best-effort)
    if let Err(e) = update_file_frontmatter(&target_path, &category, &fm.abstract_text) {
        eprintln!("Warning: frontmatter update skipped: {e}");
    }

    // ──────────────────────────────────────────────────────────────────────────
    // OFFLINE BRANCH  Zotero unreachable → enqueue steps 4-5 and return
    // ──────────────────────────────────────────────────────────────────────────
    if !zotero_online {
        emit_fn(
            "ZoteroCollectionChanged",
            "skipped",
            Some("Zotero offline — queued for later sync"),
        );
        emit_fn("ZotMovConfirmed", "skipped", Some("Zotero offline"));

        // Build a placeholder key from the DOI (if available).
        let placeholder_key = fm
            .doi
            .as_deref()
            .map(|d| format!("doi:{d}"))
            .unwrap_or_else(|| format!("file:{}", file_name.to_string_lossy()));

        let _ = crate::pending_sync::enqueue(
            config.queue_path().to_string_lossy().into_owned(),
            paper_str.clone(),
            placeholder_key,
            category.clone(),
        );

        // Record DOI and log success for this completed-to-step-3 run.
        if let Some(ref doi) = fm.doi {
            let _ = record_doi_processed(&config.dois_path(), doi, &paper_str, &category);
        }
        log_success(&config, &paper_str, &category);

        return Ok(ProcessResult {
            category,
            final_path: target_path.to_string_lossy().into_owned(),
            zotero_synced: false,
            zotero_pending: true,
        });
    }

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 4  Zotero collection update
    //
    // Lookup precedence:  DOI (most reliable) → title (fallback when Gemini
    // failed to extract a DOI).  If neither resolves to a Zotero item the
    // step is skipped and ZotMoov confirmation is skipped along with it.
    // ──────────────────────────────────────────────────────────────────────────
    emit_fn("ZoteroCollectionChanged", "started", None);

    let lookup_result: Option<(crate::zotero::ZoteroItem, String)> = match &fm.doi {
        Some(d) => match crate::zotero::get_item_by_doi(d.clone()).await {
            Ok(i) => Some((i, format!("DOI {d}"))),
            Err(_) if !fm.title.is_empty() => crate::zotero::get_item_by_title(fm.title.clone())
                .await
                .ok()
                .map(|i| (i, format!("title fallback (DOI {d} not found)"))),
            Err(_) => None,
        },
        None if !fm.title.is_empty() => crate::zotero::get_item_by_title(fm.title.clone())
            .await
            .ok()
            .map(|i| (i, "title (no DOI in frontmatter)".to_string())),
        None => None,
    };

    let Some((item, lookup_via)) = lookup_result else {
        emit_fn(
            "ZoteroCollectionChanged",
            "skipped",
            Some("no Zotero item matched DOI or title"),
        );
        emit_fn("ZotMovConfirmed", "skipped", Some("no Zotero item resolved"));
        log_success(&config, &paper_str, &category);
        return Ok(ProcessResult {
            category,
            final_path: target_path.to_string_lossy().into_owned(),
            zotero_synced: false,
            zotero_pending: false,
        });
    };

    // Record previous collection for rollback.
    let previous_collection = crate::zotero::get_current_collection(item.key.clone())
        .await
        .unwrap_or_default();

    if let Err(e) = crate::zotero::update_collection(
        item.key.clone(),
        category.clone(),
    )
    .await
    {
        emit_fn("ZoteroCollectionChanged", "failed", Some(&e));
        let _ = tx.rollback(&content_root_buf).await;
        log_error(&config, &TxError::new("ZoteroCollectionChanged", &e, &paper_str));
        return Err(format!("[ZoteroCollectionChanged] {e}"));
    }

    tx.record_step(TxStep::ZoteroCollectionChanged {
        item_key: item.key.clone(),
        previous_collection,
        new_collection: category.clone(),
    });
    emit_fn(
        "ZoteroCollectionChanged",
        "done",
        Some(&format!("via {lookup_via}")),
    );

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 5  ZotMoov PDF confirmation
    //
    // After Zotero's collection field changes, the ZotMoov plugin (when
    // configured with `subfolder = {collection}`) physically moves the PDF
    // attachment to `<dest>/<category>/<filename>.pdf`.  We poll for the file
    // to appear there before declaring success — that way the user sees the
    // PDF land in the right folder as part of one atomic-feeling action.
    // ──────────────────────────────────────────────────────────────────────────
    let expected_pdf_path: Option<PathBuf> = match &pdf_move {
        PdfMoveSpec::None => None,
        PdfMoveSpec::StaticPath { path, .. } => Some(path.clone()),
        PdfMoveSpec::PerCategory { root, filename, .. } => {
            Some(root.join(&category).join(filename))
        }
    };

    let _zotmoov_confirmed = match expected_pdf_path {
        None => {
            emit_fn(
                "ZotMovConfirmed",
                "skipped",
                Some("no PDF move spec configured"),
            );
            false
        }
        Some(pdf_path) => {
            emit_fn(
                "ZotMovConfirmed",
                "started",
                Some(&pdf_path.to_string_lossy()),
            );
            let timeout = match &pdf_move {
                PdfMoveSpec::None => 30,
                PdfMoveSpec::StaticPath { timeout_secs, .. } => *timeout_secs,
                PdfMoveSpec::PerCategory { timeout_secs, .. } => *timeout_secs,
            };
            let path_str = pdf_path.to_string_lossy().into_owned();
            match crate::zotero::wait_for_zotmoov(path_str.clone(), timeout).await {
                Ok(()) => {
                    tx.record_step(TxStep::ZotMovConfirmed {
                        pdf_path: pdf_path.clone(),
                    });
                    emit_fn("ZotMovConfirmed", "done", None);
                    true
                }
                Err(e) => {
                    emit_fn("ZotMovConfirmed", "failed", Some(&e));
                    let _ = tx.rollback(&content_root_buf).await;
                    log_error(&config, &TxError::new("ZotMovConfirmed", &e, &paper_str));
                    return Err(format!("[ZotMovConfirmed] {e}"));
                }
            }
        }
    };

    // Record DOI + log success for fully completed pipeline.
    if let Some(ref doi) = fm.doi {
        let _ = record_doi_processed(&config.dois_path(), doi, &paper_str, &category);
    }
    log_success(&config, &paper_str, &category);

    Ok(ProcessResult {
        category,
        final_path: target_path.to_string_lossy().into_owned(),
        zotero_synced: true,
        zotero_pending: false,
    })
}

// ── Tauri command ─────────────────────────────────────────────────────────────

/// Organise a single paper through the full 5-step pipeline.
///
/// # Parameters
/// * `paper_path`     — absolute path to the markdown file in `unclassified/`
/// * `content_root`   — absolute path to `content/`
/// * `pdf_root`       — ZotMoov destination root (where category subfolders live).
///                      When provided together with `pdf_filename`, the
///                      organiser waits for `<pdf_root>/<category>/<filename>`
///                      to appear after Zotero updates the collection.
/// * `pdf_filename`   — original PDF filename ZotMoov should land in the
///                      destination folder.  Ignored unless `pdf_root` is set.
/// * `window`         — injected by Tauri for event emission
///
/// # Events
/// Emits `"tx-progress"` per step with `{step, status, detail}`.
///
/// # Errors
/// Returns a human-readable error string on failure.
/// The paper file is guaranteed to be back in `unclassified/` on any error.
#[tauri::command]
pub async fn process_paper(
    paper_path: String,
    content_root: String,
    pdf_root: Option<String>,
    pdf_filename: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<ProcessResult, String> {
    let pdf_move = match (pdf_root, pdf_filename) {
        (Some(root), Some(filename)) => PdfMoveSpec::PerCategory {
            root: PathBuf::from(root),
            filename,
            timeout_secs: 30,
        },
        _ => PdfMoveSpec::None,
    };

    process_paper_core(
        paper_path,
        content_root,
        pdf_move,
        move |step, status, detail| {
            let _ = window.emit(
                "tx-progress",
                TxProgress {
                    step: step.into(),
                    status: status.into(),
                    detail: detail.map(Into::into),
                },
            );
        },
        None,
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;
    use std::fs;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> OrganizerConfig {
        let content = dir.path().join("content");
        fs::create_dir_all(content.join("papers").join("unclassified")).unwrap();
        fs::create_dir_all(content.join("meta")).unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        OrganizerConfig {
            content_root: content,
            http_client: Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap(),
        }
    }

    #[test]
    fn parse_frontmatter_extracts_fields() {
        let md = sample_paper_content("10.1234/test");
        let fm = parse_frontmatter(&md);
        assert_eq!(fm.title, "Attention Is All You Need");
        assert!(fm.abstract_text.contains("architecture"));
        assert_eq!(fm.doi.as_deref(), Some("10.1234/test"));
    }

    #[test]
    fn parse_frontmatter_empty_without_delimiters() {
        let fm = parse_frontmatter("plain body only");
        assert!(fm.title.is_empty());
        assert!(fm.doi.is_none());
    }

    #[test]
    fn extract_summary_takes_up_to_three_sentences() {
        let abs = "First sentence. Second sentence. Third sentence. Fourth ignored.";
        let s = extract_summary(abs);
        assert_eq!(s, "First sentence. Second sentence. Third sentence.");
    }

    #[test]
    fn extract_summary_handles_single_sentence_without_dot_space() {
        assert_eq!(extract_summary("Only one sentence"), "Only one sentence");
    }

    #[test]
    fn move_file_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.md");
        let dst = dir.path().join("nested").join("dst.md");
        fs::write(&src, "body").unwrap();
        move_file(&src, &dst).unwrap();
        assert!(dst.exists());
        assert!(!src.exists());
    }

    #[test]
    fn duplicate_doi_detection_roundtrip() {
        let dir = TempDir::new().unwrap();
        let dois = dir.path().join(".processed-dois.json");
        assert!(!is_duplicate_doi(&dois, "10.1/abc"));
        record_doi_processed(&dois, "10.1/abc", "paper.md", "nlp").unwrap();
        assert!(is_duplicate_doi(&dois, "10.1/abc"));
    }

    #[test]
    fn log_success_appends_to_existing_log_file() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        log_success(&cfg, "first.md", "nlp");
        log_success(&cfg, "second.md", "cv");
        let text = fs::read_to_string(cfg.log_path()).unwrap();
        let entries: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn log_error_and_success_write_json() {
        let dir = TempDir::new().unwrap();
        let cfg = test_config(&dir);
        let err = TxError {
            step: "GeminiClassified".into(),
            message: "boom".into(),
            paper_path: "paper.md".into(),
            timestamp: Utc::now().to_rfc3339(),
        };
        log_error(&cfg, &err);
        log_success(&cfg, "paper.md", "nlp");
        let log_path = cfg.log_path();
        assert!(log_path.exists());
        let text = fs::read_to_string(log_path).unwrap();
        assert!(text.contains("boom"));
        assert!(text.contains("success"));
    }

    #[test]
    fn update_file_frontmatter_replaces_existing_tags_and_summary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("paper.md");
        fs::write(
            &path,
            "---\ntitle: Test\ntags:\n  - stale\nsummary: stale summary\nabstract: One. Two.\n---\n",
        )
        .unwrap();
        update_file_frontmatter(&path, "new-cat", "One. Two.").unwrap();
        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains("new-cat"));
        assert!(!updated.contains("stale summary"));
    }

    #[test]
    fn update_file_frontmatter_injects_tags_and_summary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("paper.md");
        fs::write(
            &path,
            "---\ntitle: Test\nabstract: One. Two. Three.\n---\n\nBody",
        )
        .unwrap();
        update_file_frontmatter(&path, "my-category", "One. Two. Three.").unwrap();
        let updated = fs::read_to_string(&path).unwrap();
        assert!(updated.contains("tags:"));
        assert!(updated.contains("my-category"));
        assert!(updated.contains("summary:"));
    }

    fn sample_paper_content(doi: &str) -> String {
        format!(
            "---\ntitle: Attention Is All You Need\nabstract: We propose a new architecture.\ndoi: {doi}\n---\n\nBody"
        )
    }
}
