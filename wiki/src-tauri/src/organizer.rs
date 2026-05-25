//! Paper organiser — orchestrates the 5-step transaction pipeline.
//!
//! # Tauri commands exposed
//! | Command         | Returns                     | Description                     |
//! |-----------------|-----------------------------|---------------------------------|
//! | `process_paper` | `Result<ProcessResult, String>` | Run the full organise pipeline |
//!
//! ## Pipeline overview
//! ```text
//! 1. MovedToStaging          unclassified/ → .staging/
//! 2. GeminiClassified        classify via Gemini API
//! 3. MovedToTarget           .staging/     → {category}/
//!   ┌─ Zotero online ──────────────────────────────────────────────────────────
//! 4. ZoteroCollectionChanged update Zotero item collection
//! 5. ZotMovConfirmed         wait for ZotMoov to move PDF (if path given)
//!   └─ Zotero offline → enqueue steps 4-5 for later replay
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

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Progress event payload — `Serialize + Clone` required by Tauri 2 `emit`.
#[derive(Serialize, Clone)]
struct TxProgress {
    step: String,
    status: String,
    detail: Option<String>,
}

/// Emit one `"tx-progress"` event.  Failures are silently discarded.
fn emit_progress(
    window: &tauri::WebviewWindow,
    step: &str,
    status: &str,
    detail: Option<&str>,
) {
    let _ = window.emit(
        "tx-progress",
        TxProgress {
            step: step.into(),
            status: status.into(),
            detail: detail.map(Into::into),
        },
    );
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

// ── Tauri command ─────────────────────────────────────────────────────────────

/// Organise a single paper through the full 5-step pipeline.
///
/// # Parameters
/// * `paper_path`        — absolute path to the markdown file in `unclassified/`
/// * `content_root`      — absolute path to `content/`
/// * `expected_pdf_path` — absolute path ZotMoov should write the PDF to (step 5);
///                         pass `null` from JS to skip step 5
/// * `window`            — injected by Tauri for event emission
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
    expected_pdf_path: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<ProcessResult, String> {
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

    // Check Zotero once upfront (single ping, no retry).
    let zotero_online = crate::zotero::ping(&config.http_client).await;

    let paper_str = paper_path_buf.to_string_lossy().into_owned();
    let mut tx = PaperTransaction::new(paper_path_buf.clone());

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 1  Move to .staging/
    // ──────────────────────────────────────────────────────────────────────────
    let staging_path = config.staging_dir().join(&file_name);
    emit_progress(&window, "MovedToStaging", "started", None);

    if let Err(e) = move_file(&paper_path_buf, &staging_path) {
        emit_progress(&window, "MovedToStaging", "failed", Some(&e.to_string()));
        // Nothing completed yet; no rollback needed.
        log_error(&config, &TxError::new("MovedToStaging", &e.to_string(), &paper_str));
        return Err(format!("[MovedToStaging] {e}"));
    }

    tx.record_step(TxStep::MovedToStaging { staging_path: staging_path.clone() });
    emit_progress(&window, "MovedToStaging", "done", None);

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 2  Gemini classification
    // ──────────────────────────────────────────────────────────────────────────
    emit_progress(&window, "GeminiClassified", "started", None);

    let category = match crate::gemini::classify_paper(
        fm.title.clone(),
        fm.abstract_text.clone(),
        content.clone(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            emit_progress(&window, "GeminiClassified", "failed", Some(&e));
            let _ = tx.rollback(&content_root_buf).await;
            log_error(&config, &TxError::new("GeminiClassified", &e, &paper_str));
            return Err(format!("[GeminiClassified] {e}"));
        }
    };

    tx.record_step(TxStep::GeminiClassified { category: category.clone() });
    emit_progress(&window, "GeminiClassified", "done", Some(&category));

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 3  Move to {category}/
    // ──────────────────────────────────────────────────────────────────────────
    let target_path = config.category_dir(&category).join(&file_name);
    emit_progress(&window, "MovedToTarget", "started", None);

    if let Err(e) = move_file(&staging_path, &target_path) {
        emit_progress(&window, "MovedToTarget", "failed", Some(&e.to_string()));
        let _ = tx.rollback(&content_root_buf).await;
        log_error(&config, &TxError::new("MovedToTarget", &e.to_string(), &paper_str));
        return Err(format!("[MovedToTarget] {e}"));
    }

    tx.record_step(TxStep::MovedToTarget { target_path: target_path.clone() });
    emit_progress(&window, "MovedToTarget", "done", None);

    // ──────────────────────────────────────────────────────────────────────────
    // OFFLINE BRANCH  Zotero unreachable → enqueue steps 4-5 and return
    // ──────────────────────────────────────────────────────────────────────────
    if !zotero_online {
        emit_progress(&window, "ZoteroCollectionChanged", "skipped",
            Some("Zotero offline — queued for later sync"));
        emit_progress(&window, "ZotMovConfirmed", "skipped",
            Some("Zotero offline"));

        // Build a placeholder key from the DOI (if available).
        // pending_sync::sync_all will fail on "doi:…" keys until DOI resolution
        // is added — but the item is durably in the queue for the next retry.
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

        return Ok(ProcessResult {
            category,
            final_path: target_path.to_string_lossy().into_owned(),
            zotero_synced: false,
            zotero_pending: true,
        });
    }

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 4  Zotero collection update  (only if DOI present)
    // ──────────────────────────────────────────────────────────────────────────
    let Some(doi) = fm.doi else {
        // No DOI in frontmatter — skip Zotero steps gracefully.
        emit_progress(&window, "ZoteroCollectionChanged", "skipped",
            Some("no DOI in frontmatter"));
        emit_progress(&window, "ZotMovConfirmed", "skipped",
            Some("no DOI in frontmatter"));
        return Ok(ProcessResult {
            category,
            final_path: target_path.to_string_lossy().into_owned(),
            zotero_synced: false,
            zotero_pending: false,
        });
    };

    emit_progress(&window, "ZoteroCollectionChanged", "started", None);

    // Resolve DOI → Zotero item.
    let item = match crate::zotero::get_item_by_doi(doi.clone()).await {
        Ok(i) => i,
        Err(e) => {
            emit_progress(&window, "ZoteroCollectionChanged", "failed", Some(&e));
            let _ = tx.rollback(&content_root_buf).await;
            log_error(&config, &TxError::new("ZoteroCollectionChanged", &e, &paper_str));
            return Err(format!("[ZoteroCollectionChanged] {e}"));
        }
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
        emit_progress(&window, "ZoteroCollectionChanged", "failed", Some(&e));
        let _ = tx.rollback(&content_root_buf).await;
        log_error(&config, &TxError::new("ZoteroCollectionChanged", &e, &paper_str));
        return Err(format!("[ZoteroCollectionChanged] {e}"));
    }

    tx.record_step(TxStep::ZoteroCollectionChanged {
        item_key: item.key.clone(),
        previous_collection,
        new_collection: category.clone(),
    });
    emit_progress(&window, "ZoteroCollectionChanged", "done", None);

    // ──────────────────────────────────────────────────────────────────────────
    // STEP 5  ZotMoov PDF confirmation  (only if caller provided expected path)
    // ──────────────────────────────────────────────────────────────────────────
    let _zotmoov_confirmed = match expected_pdf_path {
        None => {
            emit_progress(&window, "ZotMovConfirmed", "skipped",
                Some("no expected_pdf_path provided"));
            false
        }
        Some(pdf_path) => {
            emit_progress(&window, "ZotMovConfirmed", "started", None);
            match crate::zotero::wait_for_zotmoov(pdf_path.clone(), 10).await {
                Ok(()) => {
                    tx.record_step(TxStep::ZotMovConfirmed {
                        pdf_path: PathBuf::from(&pdf_path),
                    });
                    emit_progress(&window, "ZotMovConfirmed", "done", None);
                    true
                }
                Err(e) => {
                    emit_progress(&window, "ZotMovConfirmed", "failed", Some(&e));
                    let _ = tx.rollback(&content_root_buf).await;
                    log_error(&config, &TxError::new("ZotMovConfirmed", &e, &paper_str));
                    return Err(format!("[ZotMovConfirmed] {e}"));
                }
            }
        }
    };

    Ok(ProcessResult {
        category,
        final_path: target_path.to_string_lossy().into_owned(),
        zotero_synced: true,
        zotero_pending: false,
    })
}
