//! Integration tests for the paper-organise pipeline.
//!
//! Each test exercises `process_paper_core` end-to-end against a real
//! temporary filesystem.  No Tauri runtime is needed; the Gemini call is
//! replaced with an in-process override so no network or OS-keychain access
//! occurs.
//!
//! # Test matrix
//! | Test | Gemini override | Zotero | Expected outcome |
//! |------|-----------------|--------|------------------|
//! | `test_full_transaction_zotero_offline` | `Ok("large-language-models")` | offline | file in category/, pending queue has 1 entry |
//! | `test_full_transaction_rollback_on_gemini_failure` | `Err(...)` | offline | file back in unclassified/, error in log |
//! | `test_duplicate_detection` | `Ok(...)` | offline | `Err("duplicate")`, file stays in unclassified/ |

use app_lib::organizer::{process_paper_core, PdfMoveSpec};
use std::path::PathBuf;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Sample markdown paper content — clean YAML frontmatter + body.
fn sample_paper_content(doi: &str) -> String {
    format!(
        r#"---
title: Attention Is All You Need
abstract: We propose a new simple network architecture. The Transformer is based solely on attention mechanisms. Experiments on two machine translation tasks show it is superior in quality.
doi: {doi}
authors: Vaswani et al.
year: 2017
---

# Introduction

The dominant sequence transduction models are based on complex recurrent networks.
"#
    )
}

/// Build the content-root directory tree inside `root`:
///
/// ```
/// root/
/// ├── content/
/// │   ├── papers/
/// │   │   ├── unclassified/
/// │   │   └── .staging/
/// │   └── meta/
/// └── logs/
/// ```
///
/// Returns the absolute path to `root/content/`.
fn setup_dirs(root: &PathBuf) -> PathBuf {
    let content = root.join("content");
    std::fs::create_dir_all(content.join("papers").join("unclassified")).unwrap();
    std::fs::create_dir_all(content.join("papers").join(".staging")).unwrap();
    std::fs::create_dir_all(content.join("meta")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    content
}

// ── Test 1 ────────────────────────────────────────────────────────────────────

/// Full pipeline with Zotero offline — paper reaches category folder and the
/// Zotero update is deferred to the pending-sync queue.
///
/// Assertions:
/// * `process_paper_core` returns `Ok` with `zotero_pending = true`
/// * `result.category` is valid lower-case kebab-case
/// * paper file exists in `content/papers/large-language-models/`
/// * `unclassified/` is empty — the file is no longer there
/// * `.staging/` does not contain the file
/// * `pending-zotero-sync.json` has exactly 1 queued entry
/// * daily log file exists
/// * file content contains `tags:` and `summary:` fields
#[tokio::test]
async fn test_full_transaction_zotero_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let content_root = setup_dirs(&root);

    let paper_name = "attention.md";
    let doi = "10.1234/attention.2017";
    let paper_path = content_root
        .join("papers")
        .join("unclassified")
        .join(paper_name);

    std::fs::write(&paper_path, sample_paper_content(doi)).unwrap();

    // Run the pipeline with Gemini overridden (no network / keychain needed).
    let result = process_paper_core(
        paper_path.to_string_lossy().into_owned(),
        content_root.to_string_lossy().into_owned(),
        PdfMoveSpec::None,
        |_, _, _| {},
        Some(Ok("large-language-models".to_string())),
    )
    .await;

    let result = result.expect("process_paper_core should succeed (Zotero offline path)");

    // ── Category ──────────────────────────────────────────────────────────────
    assert_eq!(result.category, "large-language-models");
    assert!(
        result.zotero_pending,
        "Zotero was offline — result must be zotero_pending"
    );
    assert!(
        !result.zotero_synced,
        "Zotero was offline — result must NOT be zotero_synced"
    );

    // Category must be valid lower-case kebab-case.
    let cat = &result.category;
    assert!(
        !cat.is_empty()
            && cat.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false)
            && !cat.ends_with('-')
            && cat.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Category {cat:?} is not valid lower-case kebab-case"
    );

    // ── File location ──────────────────────────────────────────────────────────
    let expected_file = content_root
        .join("papers")
        .join("large-language-models")
        .join(paper_name);
    assert!(
        expected_file.exists(),
        "Paper must be at {:?}",
        expected_file
    );

    // `unclassified/` must be empty.
    let unclassified = content_root.join("papers").join("unclassified");
    let unclassified_entries: Vec<_> = std::fs::read_dir(&unclassified)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        unclassified_entries.is_empty(),
        "unclassified/ should be empty, found: {:?}",
        unclassified_entries
            .iter()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    // `.staging/` must not contain the paper.
    let staging_copy = content_root
        .join("papers")
        .join(".staging")
        .join(paper_name);
    assert!(
        !staging_copy.exists(),
        ".staging/ must not contain {paper_name} after successful pipeline"
    );

    // ── Pending-sync queue has exactly 1 entry ────────────────────────────────
    let queue_path = content_root.join("meta").join("pending-zotero-sync.json");
    assert!(queue_path.exists(), "pending-zotero-sync.json must exist");

    let queue_text = std::fs::read_to_string(&queue_path).unwrap();
    let queue: serde_json::Value = serde_json::from_str(&queue_text).unwrap();
    let items = queue["items"].as_array().expect("queue must have an 'items' array");
    assert_eq!(items.len(), 1, "Exactly 1 item should be pending, got {}", items.len());

    // ── Log file exists ────────────────────────────────────────────────────────
    let log_dir = root.join("logs");
    let log_files: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !log_files.is_empty(),
        "logs/ directory should contain at least one log file"
    );

    // ── Frontmatter was updated ────────────────────────────────────────────────
    let file_content = std::fs::read_to_string(&expected_file).unwrap();
    assert!(
        file_content.contains("tags:"),
        "Frontmatter must contain 'tags:' after pipeline\nContent:\n{file_content}"
    );
    assert!(
        file_content.contains("large-language-models"),
        "Frontmatter tags must include the category\nContent:\n{file_content}"
    );
    assert!(
        file_content.contains("summary:"),
        "Frontmatter must contain 'summary:' after pipeline\nContent:\n{file_content}"
    );
    // Summary should contain content from the abstract (at least one sentence).
    assert!(
        file_content.contains("We propose"),
        "Summary should contain text from the abstract\nContent:\n{file_content}"
    );
}

// ── Test 2 ────────────────────────────────────────────────────────────────────

/// Gemini classification fails — rollback must return the paper to
/// `unclassified/`, leave `.staging/` empty, create no category folder, and
/// write a failure entry to the log.
#[tokio::test]
async fn test_full_transaction_rollback_on_gemini_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let content_root = setup_dirs(&root);

    let paper_name = "to-be-rolled-back.md";
    let doi = "10.1234/rollback.2024";
    let paper_path = content_root
        .join("papers")
        .join("unclassified")
        .join(paper_name);

    std::fs::write(&paper_path, sample_paper_content(doi)).unwrap();

    // Inject a Gemini failure.
    let result = process_paper_core(
        paper_path.to_string_lossy().into_owned(),
        content_root.to_string_lossy().into_owned(),
        PdfMoveSpec::None,
        |_, _, _| {},
        Some(Err("simulated Gemini API failure".to_string())),
    )
    .await;

    // ── Pipeline must return Err ───────────────────────────────────────────────
    let err = result.expect_err("process_paper_core must fail when Gemini fails");
    assert!(
        err.contains("GeminiClassified"),
        "Error must mention the failed step; got: {err:?}"
    );

    // ── Paper must be back in unclassified/ ──────────────────────────────────
    let restored_path = content_root
        .join("papers")
        .join("unclassified")
        .join(paper_name);
    assert!(
        restored_path.exists(),
        "Paper must be restored to unclassified/ after rollback; path: {:?}",
        restored_path
    );

    // ── .staging/ must be empty ───────────────────────────────────────────────
    let staging_copy = content_root
        .join("papers")
        .join(".staging")
        .join(paper_name);
    assert!(
        !staging_copy.exists(),
        ".staging/ must not contain the paper after rollback"
    );

    // ── No category folder should have been created ───────────────────────────
    let papers_dir = content_root.join("papers");
    let unexpected_dirs: Vec<_> = std::fs::read_dir(&papers_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            n != "unclassified" && n != ".staging"
        })
        .collect();
    assert!(
        unexpected_dirs.is_empty(),
        "No category directory should exist after failed pipeline; found: {:?}",
        unexpected_dirs
            .iter()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    // ── Log file must exist and contain a failure entry ───────────────────────
    let log_dir = root.join("logs");
    let log_files: Vec<_> = std::fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !log_files.is_empty(),
        "logs/ directory should contain at least one file after a failure"
    );

    // Read the first log file and verify it has a failure entry.
    let log_path = log_files[0].path();
    let log_text = std::fs::read_to_string(&log_path).unwrap();
    let log_entries: Vec<serde_json::Value> = serde_json::from_str(&log_text)
        .expect("Log file should contain a valid JSON array");
    assert!(
        !log_entries.is_empty(),
        "Log file should have at least one entry"
    );
    // At least one entry should reference the failing step.
    let has_failure = log_entries.iter().any(|e| {
        e["step"].as_str() == Some("GeminiClassified")
    });
    assert!(
        has_failure,
        "Log must contain a GeminiClassified failure entry; entries: {log_text}"
    );

    // ── Pending queue must NOT have been created ───────────────────────────────
    let queue_path = content_root.join("meta").join("pending-zotero-sync.json");
    if queue_path.exists() {
        let q_text = std::fs::read_to_string(&queue_path).unwrap();
        let q: serde_json::Value = serde_json::from_str(&q_text).unwrap_or(serde_json::json!({"items": []}));
        let items = q["items"].as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(
            items, 0,
            "Pending queue must be empty after a failed pipeline (items: {items})"
        );
    }
}

// ── Test 3 ────────────────────────────────────────────────────────────────────

/// Duplicate DOI detected — the paper must stay in `unclassified/` and
/// `process_paper_core` must return `Err` containing "duplicate".
#[tokio::test]
async fn test_duplicate_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let content_root = setup_dirs(&root);

    let doi = "10.9999/already-processed";
    let paper_name = "duplicate-paper.md";
    let paper_path = content_root
        .join("papers")
        .join("unclassified")
        .join(paper_name);

    // Pre-populate .processed-dois.json with this DOI.
    let dois_path = content_root.join("meta").join(".processed-dois.json");
    let existing = serde_json::json!({
        "dois": {
            doi: {
                "file": "/content/papers/some-category/duplicate-paper.md",
                "category": "some-category",
                "processed_at": "2026-01-01T00:00:00Z"
            }
        }
    });
    std::fs::write(
        &dois_path,
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    // Write the paper file with the same DOI.
    std::fs::write(&paper_path, sample_paper_content(doi)).unwrap();

    // Run the pipeline — must be rejected before step 1.
    let result = process_paper_core(
        paper_path.to_string_lossy().into_owned(),
        content_root.to_string_lossy().into_owned(),
        PdfMoveSpec::None,
        |_, _, _| {},
        Some(Ok("some-category".to_string())),
    )
    .await;

    // ── Pipeline must return Err containing "duplicate" ───────────────────────
    let err = result.expect_err("process_paper_core must fail for duplicate DOI");
    assert!(
        err.to_lowercase().contains("duplicate"),
        "Error message must mention 'duplicate'; got: {err:?}"
    );

    // ── File must still be in unclassified/ (no step 1 was executed) ──────────
    assert!(
        paper_path.exists(),
        "Duplicate paper must remain in unclassified/; path: {:?}",
        paper_path
    );

    // ── .staging/ must be empty ───────────────────────────────────────────────
    let staging_copy = content_root
        .join("papers")
        .join(".staging")
        .join(paper_name);
    assert!(
        !staging_copy.exists(),
        ".staging/ must be empty — duplicate was rejected before step 1"
    );

    // ── No category folder should exist ──────────────────────────────────────
    let cat_folder = content_root.join("papers").join("some-category");
    assert!(
        !cat_folder.exists(),
        "Category folder must not be created for a duplicate paper"
    );
}

// ── Test 4 ────────────────────────────────────────────────────────────────────

/// Paper without a DOI skips Zotero steps 4–5 but still lands in the category folder.
#[tokio::test]
async fn test_pipeline_without_doi_skips_zotero() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let content_root = setup_dirs(&root);

    let paper_name = "no-doi-paper.md";
    let paper_path = content_root
        .join("papers")
        .join("unclassified")
        .join(paper_name);

    std::fs::write(
        &paper_path,
        r#"---
title: Paper Without DOI
abstract: Short abstract for classification.
---
Body without DOI field.
"#,
    )
    .unwrap();

    let result = process_paper_core(
        paper_path.to_string_lossy().into_owned(),
        content_root.to_string_lossy().into_owned(),
        PdfMoveSpec::None,
        |_, _, _| {},
        Some(Ok("computer-vision".to_string())),
    )
    .await
    .expect("pipeline should succeed without DOI");

    assert_eq!(result.category, "computer-vision");
    // Zotero is offline in CI/WSL — steps 4–5 are deferred even without a DOI.
    assert!(result.zotero_pending);
    assert!(!result.zotero_synced);

    let final_file = content_root
        .join("papers")
        .join("computer-vision")
        .join(paper_name);
    assert!(final_file.exists());
}
