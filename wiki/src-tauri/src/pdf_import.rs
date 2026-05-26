//! PDF → markdown importer.
//!
//! There are two entry points into this module:
//!
//! 1. **Zotero-driven (preferred)** — the importer asks Zotero for every item
//!    in the `unclassified` collection, downloads each PDF attachment through
//!    the local API, runs it through Gemini, and writes the resulting
//!    markdown to `content/papers/unclassified/`.  No filesystem path needs
//!    to be configured because Zotero is the single source of truth.
//!
//!    ```text
//!     Zotero "unclassified" collection
//!            │  GET /items/<key>/file
//!            ▼
//!     PDF bytes ──► gemini::extract_pdf_to_markdown
//!            │
//!            ▼
//!     content/papers/unclassified/<slug>.md   (with zotero_key in frontmatter)
//!            │
//!            ▼  organizer::process_paper_core
//!     content/papers/<category>/<slug>.md
//!    ```
//!
//! 2. **Legacy filesystem-driven** — kept so users without Zotero can still
//!    feed a folder of PDFs into the wiki.  See `list_unprocessed_pdfs` /
//!    `import_pdf` below.
//!
//! # Tauri commands exposed
//! | Command                          | Returns                          | Description                                          |
//! |----------------------------------|----------------------------------|------------------------------------------------------|
//! | `list_unprocessed_pdfs`          | `Vec<PdfEntry>`                  | Filesystem scan, skip already-imported               |
//! | `import_pdf`                     | `Result<ImportResult, String>`   | One filesystem PDF → markdown                        |
//! | `import_pdf_and_organize`        | `Result<ProcessResult, String>`  | Filesystem PDF → markdown → organiser                |
//! | `list_zotero_unclassified`       | `Vec<ZoteroPdfImportEntry>`      | Items in `Unclassified` collection minus duplicates  |
//! | `list_zotero_all`                | `Vec<ZoteroPdfImportEntry>`      | Every top-level item in the library minus duplicates |
//! | `import_zotero_item_and_organize`| `Result<ProcessResult, String>`  | Zotero item → markdown → organiser                   |

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tauri::Emitter;
use walkdir::WalkDir;

/// Default name of the Zotero collection scanned by `list_zotero_unclassified`.
///
/// Matches the case the user gave the collection in Zotero itself
/// (capital-U "Unclassified") — the local API enforces exact string equality
/// on `data.name`, so an all-lowercase fallback would fail to find it.  The
/// filesystem folder under `content/papers/unclassified/` stays lowercase
/// per the CLAUDE.md kebab-case rule.
pub const DEFAULT_UNCLASSIFIED_COLLECTION: &str = "Unclassified";

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
/// When `pdf_root` is provided, the organiser also waits for ZotMoov to move
/// the physical PDF into `<pdf_root>/<category>/<original_filename>`.  Pass
/// the same root that was used to discover the PDFs in the first place.
///
/// Emits the same `tx-progress` events as the existing organiser so the
/// frontend progress UI works without changes.
#[tauri::command]
pub async fn import_pdf_and_organize(
    pdf_path: String,
    content_root: String,
    pdf_root: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<crate::organizer::ProcessResult, String> {
    // Preserve the original PDF filename so we can compute the ZotMoov
    // destination after classification.
    let pdf_filename = PathBuf::from(&pdf_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string);

    let imported = import_pdf(pdf_path, content_root.clone()).await?;

    crate::organizer::process_paper(
        imported.markdown_path,
        content_root,
        pdf_root,
        pdf_filename,
        window,
    )
    .await
}

// ── Zotero-driven path ────────────────────────────────────────────────────────

/// A Zotero item awaiting import.  Returned by `list_zotero_unclassified`.
///
/// `slug` is what the importer will use for the markdown filename; the
/// frontend can compare it against existing wiki entries to deduplicate the
/// UI list before invoking `import_zotero_item_and_organize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroPdfImportEntry {
    pub item_key: String,
    pub attachment_key: String,
    pub title: String,
    pub slug: String,
    /// The Zotero collection this item belongs to (lower-case kebab-case),
    /// or `None` if the item is in the Unclassified collection or has no
    /// collection.  When `Some`, the importer uses this as the classification
    /// override (skipping Gemini) to preserve the existing Zotero folder
    /// structure during a full-library import.
    pub collection_name: Option<String>,
}

/// Inject `zotero_key: <key>` into the YAML frontmatter so the organiser can
/// look the item up directly in step 4.  If Gemini's output has no
/// frontmatter the function returns the original string unchanged — the
/// organiser tolerates that case but it is unlikely in practice.
fn inject_zotero_key(markdown: &str, item_key: &str) -> String {
    if !markdown.starts_with("---\n") {
        return markdown.to_string();
    }
    let after_open = &markdown[4..];
    let Some(close_pos) = after_open.find("\n---") else {
        return markdown.to_string();
    };
    let fm = &after_open[..close_pos];
    let rest = &after_open[close_pos..]; // starts with "\n---"

    // Strip any pre-existing zotero_key line to keep injection idempotent.
    let cleaned: Vec<&str> = fm
        .lines()
        .filter(|l| !l.trim_start().starts_with("zotero_key:"))
        .collect();
    let cleaned_fm = cleaned.join("\n");

    format!("---\n{cleaned_fm}\nzotero_key: {item_key}{rest}")
}

/// List every item in the named Zotero collection that has not yet been
/// imported into the wiki.
///
/// `collection` defaults to [`DEFAULT_UNCLASSIFIED_COLLECTION`] when empty.
/// Already-imported items (by slug) are filtered out so calling this
/// repeatedly is safe.
#[tauri::command]
pub async fn list_zotero_unclassified(
    collection: Option<String>,
    content_root: String,
) -> Result<Vec<ZoteroPdfImportEntry>, String> {
    let collection_name = collection
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| DEFAULT_UNCLASSIFIED_COLLECTION.to_string());

    let raw = crate::zotero::list_collection_pdf_items(collection_name).await?;
    Ok(dedup_zotero_entries(raw, &content_root))
}

/// List every top-level item across the entire Zotero library that has not
/// yet been imported into the wiki.
///
/// Distinct from [`list_zotero_unclassified`] in that no collection filter is
/// applied — this is the "import everything I had before LLM-Wiki" button on
/// the dashboard.  Already-imported items (by slug) are filtered out so it is
/// safe to invoke repeatedly.
#[tauri::command]
pub async fn list_zotero_all(
    content_root: String,
) -> Result<Vec<ZoteroPdfImportEntry>, String> {
    let raw = crate::zotero::list_all_pdf_items().await?;
    Ok(dedup_zotero_entries(raw, &content_root))
}

/// Convert raw Zotero entries into import candidates, filtering out any whose
/// slug already exists under `content_root/papers/`.  Sorted by title for the
/// UI.  Shared by `list_zotero_unclassified` and `list_zotero_all`.
fn dedup_zotero_entries(
    raw: Vec<crate::zotero::ZoteroPdfEntry>,
    content_root: &str,
) -> Vec<ZoteroPdfImportEntry> {
    let existing = collect_existing_markdown_stems(
        &PathBuf::from(content_root).join("papers"),
    );

    let mut out: Vec<ZoteroPdfImportEntry> = raw
        .into_iter()
        .filter_map(|entry| {
            // Prefer the PDF filename so wikilinks match the source paper,
            // fall back to the Zotero item title.
            let stem_source = entry
                .filename
                .as_deref()
                .map(|f| {
                    Path::new(f)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(f)
                        .to_string()
                })
                .unwrap_or_else(|| entry.title.clone());

            let slug = slugify(&stem_source);
            if slug.is_empty() || existing.contains(&slug) {
                return None;
            }

            // Filter out the Unclassified collection name — items from it
            // should be classified by Gemini, not placed in an "unclassified"
            // category folder.
            let collection_name = entry.collection_name.and_then(|name| {
                if name.to_lowercase() == "unclassified" {
                    None
                } else {
                    Some(name)
                }
            });

            Some(ZoteroPdfImportEntry {
                item_key: entry.item_key,
                attachment_key: entry.attachment_key,
                title: entry.title,
                slug,
                collection_name,
            })
        })
        .collect();

    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// List the category folder names currently under `content/papers/`.
///
/// Excludes hidden directories (`.staging`) and the `unclassified` folder.
/// Returns an empty `Vec` if `papers/` does not exist.
fn list_existing_categories(content_root: &str) -> Vec<String> {
    let papers_dir = PathBuf::from(content_root).join("papers");
    if !papers_dir.is_dir() {
        return vec![];
    }
    let mut cats = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&papers_dir) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir && !name.starts_with('.') && name != "unclassified" {
                cats.push(name);
            }
        }
    }
    cats.sort();
    cats
}

/// Extract `title` and `abstract` values from a markdown document's YAML
/// frontmatter (the `---` block at the top).
///
/// Returns empty strings for any field that cannot be found.
fn extract_title_and_abstract(markdown: &str) -> (String, String) {
    let mut title = String::new();
    let mut abstract_text = String::new();

    if !markdown.starts_with("---") {
        return (title, abstract_text);
    }

    let after_open = markdown.trim_start_matches("---\n");
    let end = after_open.find("\n---").unwrap_or(after_open.len());
    let yaml = &after_open[..end];

    for line in yaml.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("title:") {
            title = v.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(v) = t.strip_prefix("abstract:") {
            abstract_text = v.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }

    (title, abstract_text)
}

/// Import a single Zotero item: download its PDF attachment, convert to
/// markdown with Gemini, write to `content/papers/unclassified/<slug>.md` with
/// `zotero_key` injected into the frontmatter, then run the organiser
/// pipeline.
///
/// Because the importer already knows the Zotero item key, step 4 of the
/// pipeline (collection update) uses it directly — no DOI guesswork.  The
/// organiser is run with [`PdfMoveSpec::None`] because ZotMoov receives the
/// authoritative signal via Zotero's collection field; LLM-Wiki does not need
/// to verify the physical move.
///
/// # Category resolution
/// * `override_category = Some(cat)` — used by the "import existing library"
///   flow where the paper's Zotero collection is already known.  The Gemini
///   classification call is skipped entirely and `cat` is used directly.
/// * `override_category = None` — used by the "import unclassified" flow.
///   Existing wiki categories are listed and passed to Gemini as hints so the
///   model prefers them over inventing new names.
#[tauri::command]
pub async fn import_zotero_item_and_organize(
    item_key: String,
    attachment_key: String,
    content_root: String,
    override_category: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<crate::organizer::ProcessResult, String> {
    // ── Download the PDF bytes through the local API ──────────────────────
    let pdf_bytes = crate::zotero::download_attachment(attachment_key.clone()).await?;

    // ── Gemini PDF → markdown ─────────────────────────────────────────────
    let markdown_raw = crate::gemini::extract_pdf_to_markdown(pdf_bytes).await?;
    let markdown = inject_zotero_key(&markdown_raw, &item_key);

    // ── Fetch metadata once so we can build a stable slug ─────────────────
    let item = crate::zotero::get_item_by_key(item_key.clone())
        .await
        .map_err(|e| format!("Zotero item lookup failed: {e}"))?;

    let title = item.data.title.clone().unwrap_or_else(|| item_key.clone());
    let slug = slugify(&title);
    let slug = if slug.is_empty() {
        slugify(&item_key)
    } else {
        slug
    };

    // ── Determine category before writing to disk ─────────────────────────
    //
    // We pre-determine the category here (one Gemini call total) and pass it
    // to `process_paper_core` as a `gemini_override` so the core pipeline
    // does not issue a second classification call.
    let gemini_override: Option<Result<String, String>> = if let Some(cat) = override_category {
        // Library import: skip Gemini, use the known Zotero collection name.
        Some(Ok(cat))
    } else {
        // Unclassified import: classify with hints from existing categories
        // so the model prefers established names over inventing new ones.
        let existing = list_existing_categories(&content_root);
        let (extracted_title, abstract_text) = extract_title_and_abstract(&markdown);
        let classify_title = if extracted_title.is_empty() { title.clone() } else { extracted_title };
        let body_excerpt: String = markdown.chars().take(2_000).collect();

        let category_result = if existing.is_empty() {
            crate::gemini::classify_paper(classify_title, abstract_text, body_excerpt).await
        } else {
            crate::gemini::classify_paper_with_existing_categories(
                classify_title,
                abstract_text,
                body_excerpt,
                existing,
            )
            .await
        };

        Some(category_result)
    };

    // ── Write to unclassified/<slug>.md ───────────────────────────────────
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

    // ── Hand off to the organiser pipeline ────────────────────────────────
    // pdf_root / pdf_filename are intentionally None — ZotMoov is driven by
    // the Zotero collection change (step 4) and we trust it to move the file.
    let window_clone = window.clone();
    let emit_fn = move |step: &str, status: &str, detail: Option<&str>| {
        let _ = window_clone.emit(
            "tx-progress",
            serde_json::json!({
                "step": step,
                "status": status,
                "detail": detail,
            }),
        );
    };

    crate::organizer::process_paper_core(
        md_path.to_string_lossy().into_owned(),
        content_root,
        crate::organizer::PdfMoveSpec::None,
        emit_fn,
        gemini_override,
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

    #[test]
    fn inject_zotero_key_adds_field_to_frontmatter() {
        let md = "---\ntitle: T\nabstract: A.\n---\n\nbody";
        let out = inject_zotero_key(md, "ABCD1234");
        assert!(out.contains("zotero_key: ABCD1234"));
        assert!(out.contains("title: T"));
        assert!(out.ends_with("body"));
    }

    #[test]
    fn inject_zotero_key_is_idempotent() {
        let md = "---\ntitle: T\nzotero_key: STALE\nabstract: A.\n---\n\nbody";
        let out = inject_zotero_key(md, "FRESH");
        let count = out.matches("zotero_key:").count();
        assert_eq!(count, 1, "expected exactly one zotero_key entry: {out}");
        assert!(out.contains("zotero_key: FRESH"));
        assert!(!out.contains("STALE"));
    }

    #[test]
    fn inject_zotero_key_no_frontmatter_returns_unchanged() {
        let md = "no frontmatter here";
        let out = inject_zotero_key(md, "ABC");
        assert_eq!(out, md);
    }
}
