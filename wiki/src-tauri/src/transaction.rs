//! Transaction state machine for the 5-step paper-organise pipeline.
//!
//! Each step that completes successfully is appended to
//! `PaperTransaction::completed_steps`.  On any failure the caller invokes
//! `PaperTransaction::rollback(content_root)`, which reverses every completed
//! step in reverse order.  The paper file is **always** returned to
//! `content/papers/unclassified/` — filesystem moves are attempted
//! unconditionally; API steps (Zotero) are best-effort.
//!
//! ## Pipeline steps
//! | # | `TxStep` variant          | Rollback action                              |
//! |---|---------------------------|----------------------------------------------|
//! | 1 | `MovedToStaging`          | move `.staging/{name}` → `unclassified/`     |
//! | 2 | `GeminiClassified`        | nothing (read-only API call)                 |
//! | 3 | `MovedToTarget`           | move `{category}/{name}` → `unclassified/`   |
//! | 4 | `ZoteroCollectionChanged` | `update_collection(key, previous_collection)`|
//! | 5 | `ZotMovConfirmed`         | nothing (undone by step 4 reversal)          |

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Step enum — each variant carries its own rollback data ────────────────────

/// A successfully completed step in the organise pipeline.
///
/// Each variant is self-describing: it carries exactly the information needed
/// to undo itself, so `rollback` never needs to consult external state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TxStep {
    /// File moved: `unclassified/{name}` → `.staging/{name}`.
    ///
    /// Rollback: move `staging_path` back to `unclassified/`.
    MovedToStaging { staging_path: PathBuf },

    /// Gemini returned a category string.  Pure API read — nothing to undo.
    GeminiClassified { category: String },

    /// File moved: `.staging/{name}` → `{category}/{name}`.
    ///
    /// Rollback: move `target_path` back to `unclassified/`.
    MovedToTarget { target_path: PathBuf },

    /// Zotero item's collection was updated to the target category.
    ///
    /// Rollback: call `zotero::update_collection(item_key, previous_collection)`
    /// (best-effort async; failure is logged but does not abort rollback).
    ZoteroCollectionChanged {
        item_key: String,
        previous_collection: String,
        new_collection: String,
    },

    /// ZotMoov confirmed the PDF is in the correct folder.
    ///
    /// Rollback: nothing extra — the `ZoteroCollectionChanged` reversal above
    /// triggers ZotMoov to move the PDF back automatically.
    ZotMovConfirmed { pdf_path: PathBuf },
}

// ── Main transaction struct ───────────────────────────────────────────────────

/// Live state of a single paper-organise transaction.
///
/// # Usage pattern
/// ```rust
/// let mut tx = PaperTransaction::new(paper_path);
/// // … do step 1 …
/// tx.record_step(TxStep::MovedToStaging { staging_path });
/// // … step fails …
/// let outcome = tx.rollback(&content_root).await;
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperTransaction {
    /// The paper file's location **before** the pipeline started.
    pub original_path: PathBuf,
    /// Steps completed so far; appended by `record_step`, iterated in reverse
    /// by `rollback`.
    pub completed_steps: Vec<TxStep>,
    /// RFC 3339 timestamp of transaction creation (for log correlation).
    pub started_at: String,
}

impl PaperTransaction {
    /// Create a fresh transaction for the given paper file.
    pub fn new(original_path: PathBuf) -> Self {
        Self {
            original_path,
            completed_steps: Vec::new(),
            started_at: Utc::now().to_rfc3339(),
        }
    }

    /// Append a successfully completed step to the transaction log.
    pub fn record_step(&mut self, step: TxStep) {
        self.completed_steps.push(step);
    }

    /// Reverse every completed step in reverse order.
    ///
    /// **File guarantee**: `unclassified/` receipt is unconditional for
    /// filesystem steps.  If the rename fails (e.g. file missing, permissions),
    /// the failure is recorded in `RollbackOutcome` but rollback continues.
    ///
    /// **Zotero guarantee**: best-effort.  A Zotero API failure is recorded
    /// but does not abort the filesystem moves that follow.
    ///
    /// Returns a [`RollbackOutcome`] with per-step success / error details
    /// suitable for structured logging.
    pub async fn rollback(&self, content_root: &Path) -> RollbackOutcome {
        let unclassified = content_root.join("papers").join("unclassified");
        let mut steps: Vec<StepOutcome> = Vec::new();

        for tx_step in self.completed_steps.iter().rev() {
            let outcome = rollback_one(tx_step, &unclassified).await;
            steps.push(outcome);
        }

        RollbackOutcome { steps }
    }
}

// ── Rollback dispatcher (private) ─────────────────────────────────────────────

async fn rollback_one(step: &TxStep, unclassified: &Path) -> StepOutcome {
    match step {
        // ── Step 1 ────────────────────────────────────────────────────────────
        TxStep::MovedToStaging { staging_path } => {
            let dest = unclassified.join(filename_of(staging_path));
            let result = move_file(staging_path, &dest).await;
            StepOutcome::from_result("MovedToStaging", result)
        }

        // ── Step 2 — read-only, no undo ───────────────────────────────────────
        TxStep::GeminiClassified { .. } => StepOutcome {
            step: "GeminiClassified".into(),
            ok: true,
            error: None,
        },

        // ── Step 3 ────────────────────────────────────────────────────────────
        TxStep::MovedToTarget { target_path } => {
            let dest = unclassified.join(filename_of(target_path));
            let result = move_file(target_path, &dest).await;
            StepOutcome::from_result("MovedToTarget", result)
        }

        // ── Step 4 — async Zotero API call ────────────────────────────────────
        TxStep::ZoteroCollectionChanged {
            item_key,
            previous_collection,
            ..
        } => {
            let result = crate::zotero::update_collection(
                item_key.clone(),
                previous_collection.clone(),
            )
            .await;
            StepOutcome::from_result("ZoteroCollectionChanged", result)
        }

        // ── Step 5 — nothing to undo ──────────────────────────────────────────
        TxStep::ZotMovConfirmed { .. } => StepOutcome {
            step: "ZotMovConfirmed".into(),
            ok: true,
            error: None,
        },
    }
}

// ── Outcome types ─────────────────────────────────────────────────────────────

/// Aggregate result of a full rollback run.
#[derive(Debug, Serialize, Deserialize)]
pub struct RollbackOutcome {
    /// One entry per step that was reversed, in the order they were undone.
    pub steps: Vec<StepOutcome>,
}

impl RollbackOutcome {
    /// `true` if every step rolled back without error.
    pub fn all_ok(&self) -> bool {
        self.steps.iter().all(|s| s.ok)
    }
}

/// Result of reversing a single step.
#[derive(Debug, Serialize, Deserialize)]
pub struct StepOutcome {
    pub step: String,
    pub ok: bool,
    pub error: Option<String>,
}

impl StepOutcome {
    fn from_result(step: &str, r: Result<(), String>) -> Self {
        Self {
            step: step.into(),
            ok: r.is_ok(),
            error: r.err(),
        }
    }
}

// ── Structured error type (written to logs by the organiser) ──────────────────

/// One entry in `logs/organize-YYYY-MM-DD.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct TxError {
    pub step: String,
    pub message: String,
    pub paper_path: String,
    pub timestamp: String,
}

impl TxError {
    pub fn new(step: &str, message: &str, paper_path: &str) -> Self {
        Self {
            step: step.into(),
            message: message.into(),
            paper_path: paper_path.into(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

// ── Private filesystem helpers ────────────────────────────────────────────────

/// Move a file to `dst`, creating destination parent dirs if needed.
///
/// Uses `tokio::fs` to avoid blocking the executor on slow filesystems.
async fn move_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    tokio::fs::rename(src, dst)
        .await
        .map_err(|e| format!("rename {} → {}: {e}", src.display(), dst.display()))
}

/// Extract the final path component as an `OsStr`, falling back to `""`.
fn filename_of(p: &Path) -> &std::ffi::OsStr {
    p.file_name().unwrap_or_default()
}
