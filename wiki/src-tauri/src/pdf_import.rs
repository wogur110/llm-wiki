//! PDF → markdown importer.
//!
//! Bridges a Zotero `storage/` folder full of PDFs and the existing
//! `content/papers/unclassified/` workflow:
//!
//! ```text
//!  Zotero/storage/<KEY>/paper.pdf
//!         │
//!         ▼   read bytes + gemini::extract_pdf_to_markdown()
//!  content/papers/unclassified/<slug>.md
//!         │
//!         ▼   organizer::process_paper_core()  (existing pipeline)
//!  content/papers/<category>/<slug>.md
//! ```
//!
//! # Tauri commands exposed
//! | Command                  | Returns                       | Description                                          |
//! |--------------------------|-------------------------------|------------------------------------------------------|
//! | `list_unprocessed_pdfs`  | `Vec<PdfEntry>`               | Walk the Zotero PDF folder, skip already-imported    |
//! | `import_pdf`             | `Result<ImportResult, String>`| Convert one PDF → markdown, drop in `unclassified/`  |
//! | `import_pdf_and_organize`| `Result<ProcessResult, String>`| `import_pdf` + run organiser pipeline in one call   |

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// ── Public types ──────────────────────────────────────────────────────────────

/// A PDF discovered under the Zotero storage folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfEntry {
    /// Absolute path to the PDF file.
    pub path: String,
    /// PDF filename without extension — used as the markdown filename.
    pub stem: String,
    /// File size in bytes (cheap reassurance for the UI).
    pub size_bytes: u64,
}

/// Returned to JS by `import_pdf`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    /// Absolute path to the newly-created markdown file in `unclassified/`.
    pub markdown_path: String,
    /// Slug used for the filename (kebab-case, no extension).
    pub slug: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert an arbitrary file stem to a safe lower-case kebab-case slug.
///
/// Mirrors the rules used by the frontend `slugifyWikilink` so the same paper
/// produces the same slug whether it enters the wiki via PDF import or via
/// a wikilink in another note.
pub(crate) fn slugify(stem: &str) -> String {
    let lower = stem.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_dash = true; // suppress leading dashes
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Gather every `.md` filename stem currently inside `content/papers/`.
///
/// Used to decide which PDFs in the Zotero folder have *not* yet been
/// imported.  Returns an empty set if `papers/` does not exist.
fn collect_existing_markdown_stems(papers_root: &Path) -> HashSet<String> {
    if !papers_root.exists() {
        return HashSet::new();
    }
    WalkDir::new(papers_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_lowercase)
        })
        .collect()
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Walk `pdf_root` recursively, returning every `.pdf` whose slug has not yet
/// been imported into `content_root/papers/`.
///
/// Hidden directories (starting with `.`) are skipped — this excludes Zotero's
/// own `.zotero-ft-cache/` and similar.
#[tauri::command]
pub fn list_unprocessed_pdfs(
    pdf_root: String,
    content_root: String,
) -> Result<Vec<PdfEntry>, String> {
    let pdf_root = PathBuf::from(pdf_root);
    if !pdf_root.is_dir() {
        return Err(format!(
            "PDF folder does not exist or is not a directory: {}",
            pdf_root.display()
        ));
    }

    let existing = collect_existing_markdown_stems(
        &PathBuf::from(&content_root).join("papers"),
    );

    let mut out: Vec<PdfEntry> = WalkDir::new(&pdf_root)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden directories (Zotero metadata caches).
            !e.file_name()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pdf"))
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            let slug = slugify(&stem);
            if existing.contains(&slug) {
                return None;
            }
            let size = entry.metadata().ok().map(|m| m.len()).unwrap_or(0);
            Some(PdfEntry {
                path: path.to_string_lossy().into_owned(),
                stem,
                size_bytes: size,
            })
        })
        .collect();

    out.sort_by(|a, b| a.stem.to_lowercase().cmp(&b.stem.to_lowercase()));
    Ok(out)
}

/// Convert a single PDF to a markdown file in `content/papers/unclassified/`.
///
/// Returns the path to the newly-written markdown file.  Does **not** run the
/// organiser pipeline — call `import_pdf_and_organize` for the full flow.
#[tauri::command]
pub async fn import_pdf(
    pdf_path: String,
    content_root: String,
) -> Result<ImportResult, String> {
    let pdf_path_buf = PathBuf::from(&pdf_path);
    if !pdf_path_buf.is_file() {
        return Err(format!("PDF file does not exist: {pdf_path}"));
    }

    let stem = pdf_path_buf
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Could not derive filename from PDF path".to_string())?
        .to_string();
    let slug = slugify(&stem);
    if slug.is_empty() {
        return Err(format!("PDF filename {stem:?} produced an empty slug"));
    }

    let pdf_bytes = tokio::fs::read(&pdf_path_buf)
        .await
        .map_err(|e| format!("Cannot read PDF: {e}"))?;

    let markdown = crate::gemini::extract_pdf_to_markdown(pdf_bytes).await?;

    let unclassified_dir = PathBuf::from(&content_root)
        .join("papers")
        .join("unclassified");
    tokio::fs::create_dir_all(&unclassified_dir)
        .await
        .map_err(|e| format!("Cannot create unclassified/ directory: {e}"))?;

    let md_path = unclassified_dir.join(format!("{slug}.md"));
    tokio::fs::write(&md_path, markdown)
        .await
        .map_err(|e| format!("Cannot write markdown file: {e}"))?;

    Ok(ImportResult {
        markdown_path: md_path.to_string_lossy().into_owned(),
        slug,
    })
}

/// Convenience wrapper: PDF → markdown → organiser pipeline.
///
/// Emits the same `tx-progress` events as the existing organiser so the
/// frontend progress UI works without changes.
#[tauri::command]
pub async fn import_pdf_and_organize(
    pdf_path: String,
    content_root: String,
    window: tauri::WebviewWindow,
) -> Result<crate::organizer::ProcessResult, String> {
    let imported = import_pdf(pdf_path, content_root.clone()).await?;
    crate::organizer::process_paper(
        imported.markdown_path,
        content_root,
        None,
        window,
    )
    .await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn slugify_handles_typical_paper_filenames() {
        assert_eq!(
            slugify("Attention Is All You Need"),
            "attention-is-all-you-need"
        );
        assert_eq!(
            slugify("Vaswani_et_al_2017_Attention"),
            "vaswani-et-al-2017-attention"
        );
        assert_eq!(slugify("  Trim  Edges  "), "trim-edges");
        assert_eq!(slugify("BERT: Pre-training"), "bert-pre-training");
    }

    #[test]
    fn slugify_collapses_runs_of_separators() {
        assert_eq!(slugify("a---b___c"), "a-b-c");
    }

    #[test]
    fn list_unprocessed_pdfs_filters_already_imported() {
        let dir = TempDir::new().unwrap();

        // Build a fake Zotero storage tree.
        let pdf_root = dir.path().join("storage");
        fs::create_dir_all(pdf_root.join("ABC123")).unwrap();
        fs::create_dir_all(pdf_root.join("DEF456")).unwrap();
        fs::write(pdf_root.join("ABC123").join("Attention.pdf"), b"%PDF-1.4").unwrap();
        fs::write(pdf_root.join("DEF456").join("LoRA.pdf"), b"%PDF-1.4").unwrap();
        // Hidden directory must be skipped.
        fs::create_dir_all(pdf_root.join(".zotero-ft-cache")).unwrap();
        fs::write(pdf_root.join(".zotero-ft-cache").join("ignore.pdf"), b"x").unwrap();

        // Pretend Attention has already been imported.
        let content_root = dir.path().join("content");
        fs::create_dir_all(content_root.join("papers").join("nlp")).unwrap();
        fs::write(
            content_root.join("papers").join("nlp").join("attention.md"),
            "---\n---\n",
        )
        .unwrap();

        let entries = list_unprocessed_pdfs(
            pdf_root.to_string_lossy().into_owned(),
            content_root.to_string_lossy().into_owned(),
        )
        .unwrap();

        assert_eq!(entries.len(), 1, "got {entries:#?}");
        assert_eq!(entries[0].stem, "LoRA");
    }

    #[test]
    fn list_unprocessed_pdfs_errors_for_missing_folder() {
        let result = list_unprocessed_pdfs(
            "/definitely/not/a/real/path/zzz".into(),
            "/tmp/whatever".into(),
        );
        assert!(result.is_err());
    }
}
