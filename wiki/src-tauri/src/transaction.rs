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
/// ```rust,no_run
/// # use app_lib::transaction::{PaperTransaction, TxStep};
/// # use std::path::PathBuf;
/// # async fn example() {
/// let mut tx = PaperTransaction::new(PathBuf::from("/content/papers/unclassified/paper.md"));
/// // … do step 1 …
/// tx.record_step(TxStep::MovedToStaging { staging_path: PathBuf::from("/content/papers/.staging/paper.md") });
/// // … step fails …
/// let outcome = tx.rollback(std::path::Path::new("/content")).await;
/// # }
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Step 1 rollback: file moved to staging must return to `unclassified/`.
    #[tokio::test]
    async fn test_rollback_move_to_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let unclassified = root.join("papers").join("unclassified");
        let staging = root.join("papers").join(".staging");
        tokio::fs::create_dir_all(&unclassified).await.unwrap();
        tokio::fs::create_dir_all(&staging).await.unwrap();

        // Create the paper in unclassified/.
        let name = "test-paper.md";
        let original = unclassified.join(name);
        tokio::fs::write(&original, b"# Test").await.unwrap();

        // Simulate step 1: move to staging.
        let staging_path = staging.join(name);
        tokio::fs::rename(&original, &staging_path).await.unwrap();
        assert!(!original.exists(), "paper should have moved out of unclassified");

        // Build a transaction that records step 1.
        let mut tx = PaperTransaction::new(original.clone());
        tx.record_step(TxStep::MovedToStaging {
            staging_path: staging_path.clone(),
        });

        // Rollback step 1.
        let outcome = tx.rollback(root).await;

        assert!(
            outcome.all_ok(),
            "Rollback should succeed: {:?}",
            outcome.steps
        );
        assert!(original.exists(), "Paper must be back in unclassified/");
        assert!(!staging_path.exists(), "Paper must be gone from staging");
    }

    /// Steps 1-3 rollback: file in target category must return to `unclassified/`.
    #[tokio::test]
    async fn test_rollback_move_to_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let unclassified = root.join("papers").join("unclassified");
        let staging = root.join("papers").join(".staging");
        let category_dir = root.join("papers").join("large-language-models");
        tokio::fs::create_dir_all(&unclassified).await.unwrap();
        tokio::fs::create_dir_all(&staging).await.unwrap();
        tokio::fs::create_dir_all(&category_dir).await.unwrap();

        let name = "attention-paper.md";
        let original = unclassified.join(name);
        let staging_path = staging.join(name);
        let target_path = category_dir.join(name);

        // Place the file directly in the target (simulating completed steps 1-3).
        tokio::fs::write(&target_path, b"# Attention is All You Need").await.unwrap();

        // Record steps 1-3 in the transaction.
        let mut tx = PaperTransaction::new(original.clone());
        tx.record_step(TxStep::MovedToStaging {
            staging_path: staging_path.clone(),
        });
        tx.record_step(TxStep::GeminiClassified {
            category: "large-language-models".into(),
        });
        tx.record_step(TxStep::MovedToTarget {
            target_path: target_path.clone(),
        });

        let outcome = tx.rollback(root).await;

        // Step 3 reversal (MovedToTarget) must have succeeded and moved the file.
        let moved_to_target_step = outcome
            .steps
            .iter()
            .find(|s| s.step == "MovedToTarget")
            .expect("MovedToTarget should appear in rollback steps");
        assert!(
            moved_to_target_step.ok,
            "MovedToTarget rollback failed: {:?}",
            moved_to_target_step.error
        );

        // The paper must be back in unclassified/.
        assert!(original.exists(), "Paper must be returned to unclassified/");
        assert!(!target_path.exists(), "Paper must be gone from category dir");
    }

    /// `rollback` must reverse steps in the **opposite** order they were recorded.
    #[tokio::test]
    async fn test_rollback_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // No real files needed — we only check the order of outcome entries.

        let mut tx = PaperTransaction::new(root.join("dummy.md"));
        tx.record_step(TxStep::MovedToStaging {
            staging_path: root.join("papers/.staging/dummy.md"),
        });
        tx.record_step(TxStep::GeminiClassified {
            category: "test-category".into(),
        });
        tx.record_step(TxStep::MovedToTarget {
            target_path: root.join("papers/test-category/dummy.md"),
        });

        let outcome = tx.rollback(root).await;

        // Three steps → three outcome entries.
        assert_eq!(outcome.steps.len(), 3, "Expected 3 rollback entries");

        // Order must be reversed: 3 → 2 → 1.
        assert_eq!(
            outcome.steps[0].step, "MovedToTarget",
            "First undone step should be the last recorded"
        );
        assert_eq!(outcome.steps[1].step, "GeminiClassified");
        assert_eq!(outcome.steps[2].step, "MovedToStaging");
    }

    /// Rollback on a transaction with no recorded steps must produce an empty,
    /// error-free outcome.
    #[tokio::test]
    async fn test_empty_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let tx = PaperTransaction::new(root.join("nonexistent.md"));
        let outcome = tx.rollback(root).await;

        assert!(
            outcome.steps.is_empty(),
            "Empty transaction should produce no rollback steps"
        );
        assert!(
            outcome.all_ok(),
            "Empty rollback is trivially all_ok"
        );
    }
}
