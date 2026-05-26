//! PDF → markdown importer.
//!
//! There are two entry points into this module:
//!
//! 1. **Zotero-driven (preferred)** — the importer asks Zotero for every
//!    top-level item (or every item in the `Unclassified` collection) and
//!    builds a markdown stub from Zotero's own metadata.  When Zotero is
//!    missing the abstract, [`crate::abstract_lookup`] fetches one from
//!    Crossref / Semantic Scholar / OpenAlex.  **The PDF is never sent to
//!    Gemini** — we only need title + abstract for classification, and
//!    everything else already lives in Zotero.
//!
//!    ```text
//!     Zotero item (key + DOI + metadata)
//!            │
//!            ├──► Zotero.abstractNote ──┐
//!            │                          ▼
//!            └──► Crossref / S2 / OpenAlex  (only if Zotero abstract empty)
//!                                       │
//!                                       ▼
//!     content/papers/unclassified/<slug>.md   (frontmatter-only stub)
//!            │
//!            ▼  organizer::process_paper_core
//!     content/papers/<category>/<slug>.md
//!    ```
//!
//! 2. **Legacy filesystem-driven** — kept so users without Zotero can still
//!    feed a folder of PDFs into the wiki.  See `list_unprocessed_pdfs` /
//!    `import_pdf` below.  This path *does* call
//!    [`crate::gemini::extract_pdf_to_markdown`] because no Zotero metadata
//!    is available.
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

            // Sanitize the nested collection path for filesystem use while
            // preserving the user's preferred capitalisation and underscores
            // ("Computer Vision/01_Generative_Models/Autoencoders" stays
            // verbatim, only forbidden chars are stripped).  Items in the
            // top-level "Unclassified" Zotero collection get `None` so they
            // run through Gemini classification instead of being placed in
            // a literal "Unclassified" folder.
            let collection_name = entry
                .collection_name
                .and_then(|path| sanitize_collection_path(&path));

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

/// Sanitise a `/`-separated Zotero collection path for use as a wiki folder
/// path.  Returns `None` when the path collapses to nothing or to a single
/// `Unclassified` segment (those items belong in the Gemini-classification
/// flow, not in a literal `unclassified/` folder).
pub(crate) fn sanitize_collection_path(raw: &str) -> Option<String> {
    let segments: Vec<String> = raw
        .split('/')
        .map(sanitize_path_segment)
        .filter(|s| !s.is_empty())
        .collect();

    if segments.is_empty() {
        return None;
    }

    // Single "Unclassified" segment (case-insensitive) → return None so the
    // importer treats the item as new-and-unclassified.  Nested paths that
    // *contain* "Unclassified" deeper in the hierarchy are kept as-is.
    if segments.len() == 1 && segments[0].eq_ignore_ascii_case("unclassified") {
        return None;
    }

    Some(segments.join("/"))
}

/// Strip filesystem-unsafe characters from one path segment without losing
/// the original capitalisation, spaces, or underscores that the user
/// presumably chose on purpose in Zotero.
///
/// Removes the Windows-reserved set `< > : " | ? *`, NULs, and any embedded
/// path separators.  Collapses runs of whitespace and trims leading/trailing
/// dots and spaces (Windows would otherwise refuse to create the directory).
pub(crate) fn sanitize_path_segment(seg: &str) -> String {
    let cleaned: String = seg
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/' | '\0'))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string()
}

/// All metadata required to build a frontmatter-only markdown stub.
struct PaperMeta<'a> {
    title: &'a str,
    abstract_text: &'a str,
    doi: Option<&'a str>,
    year: Option<&'a str>,
    authors: Option<&'a str>,
    zotero_key: &'a str,
}

/// Render a YAML-frontmatter stub.  The body is intentionally empty — the
/// organiser uses `title + abstract` for classification and the wiki shows
/// the abstract as the paper page itself.
fn build_metadata_markdown(m: PaperMeta<'_>) -> String {
    let title = escape_yaml_plain(m.title);
    let abstract_text = escape_yaml_plain(m.abstract_text);
    let authors = m.authors.map(escape_yaml_plain).unwrap_or_default();
    let doi = m.doi.unwrap_or("").trim();
    let year = m.year.unwrap_or("").trim();

    format!(
        "---\n\
         title: \"{title}\"\n\
         authors: \"{authors}\"\n\
         abstract: \"{abstract_text}\"\n\
         doi: \"{doi}\"\n\
         year: {year}\n\
         zotero_key: {zk}\n\
         ---\n\n\
         {body}\n",
        zk = m.zotero_key,
        body = if abstract_text.is_empty() {
            String::new()
        } else {
            format!("## Abstract\n\n{abstract_text}")
        },
    )
}

/// Make a string safe inside a YAML double-quoted scalar that
/// `organizer::parse_frontmatter` reads as a single line.
///
/// We use a plain (single-line) representation: newlines become spaces,
/// embedded double quotes are mapped to U+201C/U+201D curly quotes so the
/// naive `trim_matches('"')` in the existing parser does not get confused.
pub(crate) fn escape_yaml_plain(s: &str) -> String {
    let collapsed = s.replace('\r', " ").replace('\n', " ");
    let trimmed: String = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");
    trimmed.replace('"', "\u{201D}")
}

/// Build a single-line `"First Last, First Last"` author list.
///
/// Skips editors/translators (the wiki only cares about authors) and falls
/// back to the institutional `name` field when no first/last name is set.
pub(crate) fn format_authors(creators: &[crate::zotero::ZoteroCreator]) -> Option<String> {
    let names: Vec<String> = creators
        .iter()
        .filter(|c| {
            c.creator_type
                .as_deref()
                .map(|t| t.eq_ignore_ascii_case("author"))
                .unwrap_or(true)
        })
        .filter_map(|c| match (c.first_name.as_deref(), c.last_name.as_deref(), c.name.as_deref()) {
            (Some(f), Some(l), _) if !f.is_empty() && !l.is_empty() => {
                Some(format!("{f} {l}"))
            }
            (None, Some(l), _) if !l.is_empty() => Some(l.to_string()),
            (Some(f), None, _) if !f.is_empty() => Some(f.to_string()),
            (_, _, Some(n)) if !n.trim().is_empty() => Some(n.trim().to_string()),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

/// Extract a 4-digit year from any Zotero date string.
///
/// Handles `"2017"`, `"2017-06-12"`, `"2017-06"`, `"June 2017"` and similar.
/// Returns `None` if no plausible year (1800-2099) appears.
pub(crate) fn extract_year(date: Option<&str>) -> Option<String> {
    let d = date?.trim();
    let bytes = d.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    for i in 0..=bytes.len() - 4 {
        let slice = &bytes[i..i + 4];
        if !slice.iter().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let n: u32 = std::str::from_utf8(slice).ok()?.parse().ok()?;
        if (1800..=2099).contains(&n) {
            return Some(n.to_string());
        }
    }
    None
}

/// Import a single Zotero item into the wiki using **metadata only**.
///
/// The original PDF is never downloaded or sent to Gemini.  All required
/// fields (title, authors, DOI, year, abstract) come straight from Zotero;
/// when Zotero's `abstractNote` is empty, [`crate::abstract_lookup`] fetches
/// the abstract from public scholarly APIs (Crossref → Semantic Scholar →
/// OpenAlex) using the DOI, or by title as a last resort.
///
/// `attachment_key` is no longer used for the import itself, but is kept in
/// the signature for backward compatibility with the JS frontend (the
/// listing endpoints still return it, and a future "open original PDF in
/// Zotero" button can consume it without another round-trip).
///
/// Because the importer already knows the Zotero item key, step 4 of the
/// pipeline (collection update) uses it directly — no DOI guesswork.  The
/// organiser is run with [`crate::organizer::PdfMoveSpec::None`] because
/// ZotMoov receives the authoritative signal via Zotero's collection field;
/// LLM-Wiki does not need to verify the physical move.
///
/// # Category resolution
/// * `override_category = Some(cat)` — used by the "import existing library"
///   flow where the paper's Zotero collection is already known.  The Gemini
///   classification call is skipped entirely and `cat` is used directly.
/// * `override_category = None` — used by the "import unclassified" flow.
///   `process_paper_core` calls Gemini for classification using only
///   `title + abstract` (no PDF, no body).  This is **one** Gemini call per
///   item instead of two, drastically reducing 429 / 400 errors.
#[tauri::command]
pub async fn import_zotero_item_and_organize(
    item_key: String,
    attachment_key: String,
    content_root: String,
    override_category: Option<String>,
    window: tauri::WebviewWindow,
) -> Result<crate::organizer::ProcessResult, String> {
    // `attachment_key` is currently unused — the metadata-only flow does not
    // download the PDF.  We keep the parameter so the JS bindings continue
    // to compile and a future "open PDF" UI can pass it through.
    let _ = attachment_key;

    // ── Fetch Zotero metadata ─────────────────────────────────────────────
    let item = crate::zotero::get_item_by_key(item_key.clone())
        .await
        .map_err(|e| format!("Zotero item lookup failed: {e}"))?;

    let title = item
        .data
        .title
        .clone()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| item_key.clone());

    let doi = item
        .data
        .doi
        .clone()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty());

    let authors = format_authors(&item.data.creators);
    let year = extract_year(item.data.date.as_deref());

    // ── Resolve abstract: Zotero > Crossref/S2/OpenAlex > title search ────
    let zotero_abstract = item
        .data
        .abstract_note
        .clone()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());

    let abstract_text = match zotero_abstract {
        Some(a) => a,
        None => {
            let from_doi = match doi.as_deref() {
                Some(d) => crate::abstract_lookup::fetch_abstract_by_doi(d).await,
                None => None,
            };
            match from_doi {
                Some(a) => a,
                None => crate::abstract_lookup::fetch_abstract_by_title(&title)
                    .await
                    .unwrap_or_default(),
            }
        }
    };

    // ── Build the markdown stub ───────────────────────────────────────────
    let slug = slugify(&title);
    let slug = if slug.is_empty() {
        slugify(&item_key)
    } else {
        slug
    };

    let markdown = build_metadata_markdown(PaperMeta {
        title: &title,
        abstract_text: &abstract_text,
        doi: doi.as_deref(),
        year: year.as_deref(),
        authors: authors.as_deref(),
        zotero_key: &item_key,
    });

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
    let gemini_override: Option<Result<String, String>> = override_category.map(Ok);

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
    fn build_metadata_markdown_includes_required_fields() {
        let md = build_metadata_markdown(PaperMeta {
            title: "Attention Is All You Need",
            abstract_text: "We propose a new architecture.",
            doi: Some("10.1234/test"),
            year: Some("2017"),
            authors: Some("Ashish Vaswani, Noam Shazeer"),
            zotero_key: "ABCD1234",
        });
        assert!(md.starts_with("---\n"));
        assert!(md.contains("title: \"Attention Is All You Need\""));
        assert!(md.contains("authors: \"Ashish Vaswani, Noam Shazeer\""));
        assert!(md.contains("abstract: \"We propose a new architecture.\""));
        assert!(md.contains("doi: \"10.1234/test\""));
        assert!(md.contains("year: 2017"));
        assert!(md.contains("zotero_key: ABCD1234"));
        assert!(md.contains("## Abstract"));
    }

    #[test]
    fn build_metadata_markdown_handles_missing_abstract() {
        let md = build_metadata_markdown(PaperMeta {
            title: "T",
            abstract_text: "",
            doi: None,
            year: None,
            authors: None,
            zotero_key: "K",
        });
        assert!(md.contains("abstract: \"\""));
        assert!(md.contains("year: "));
        assert!(!md.contains("## Abstract"));
    }

    #[test]
    fn escape_yaml_plain_strips_newlines_and_quotes() {
        let raw = "Line\nwith\r\nbreaks and \"quotes\"";
        let out = escape_yaml_plain(raw);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert!(!out.contains('"'));
        assert!(out.contains("Line with breaks"));
    }

    #[test]
    fn format_authors_combines_first_and_last_names() {
        use crate::zotero::ZoteroCreator;
        let creators = vec![
            ZoteroCreator {
                creator_type: Some("author".into()),
                first_name: Some("Ashish".into()),
                last_name: Some("Vaswani".into()),
                name: None,
            },
            ZoteroCreator {
                creator_type: Some("editor".into()),
                first_name: Some("Anon".into()),
                last_name: Some("Editor".into()),
                name: None,
            },
            ZoteroCreator {
                creator_type: Some("author".into()),
                first_name: None,
                last_name: None,
                name: Some("OpenAI".into()),
            },
        ];
        let out = format_authors(&creators).unwrap();
        assert_eq!(out, "Ashish Vaswani, OpenAI");
    }

    #[test]
    fn format_authors_returns_none_when_empty() {
        assert!(format_authors(&[]).is_none());
    }

    #[test]
    fn sanitize_path_segment_preserves_capitals_and_spaces() {
        assert_eq!(sanitize_path_segment("Computer Vision"), "Computer Vision");
        assert_eq!(sanitize_path_segment("01_Generative_Models"), "01_Generative_Models");
        assert_eq!(sanitize_path_segment("Autoencoders"), "Autoencoders");
    }

    #[test]
    fn sanitize_path_segment_strips_forbidden_chars() {
        assert_eq!(sanitize_path_segment("a<b>c"), "abc");
        assert_eq!(sanitize_path_segment("foo/bar"), "foobar");
        assert_eq!(sanitize_path_segment("trailing dots..."), "trailing dots");
        assert_eq!(sanitize_path_segment("  spaced  out  "), "spaced out");
    }

    #[test]
    fn sanitize_collection_path_preserves_nested_zotero_layout() {
        let path = "Computer Vision/01_Generative_Models/Autoencoders";
        let out = sanitize_collection_path(path).unwrap();
        assert_eq!(out, "Computer Vision/01_Generative_Models/Autoencoders");
    }

    #[test]
    fn sanitize_collection_path_drops_top_unclassified() {
        assert!(sanitize_collection_path("Unclassified").is_none());
        assert!(sanitize_collection_path("unclassified").is_none());
        assert!(sanitize_collection_path("").is_none());
    }

    #[test]
    fn sanitize_collection_path_keeps_unclassified_when_nested() {
        let path = "Computer Vision/Unclassified";
        let out = sanitize_collection_path(path).unwrap();
        assert_eq!(out, "Computer Vision/Unclassified");
    }

    #[test]
    fn extract_year_handles_zotero_date_shapes() {
        assert_eq!(extract_year(Some("2017")).as_deref(), Some("2017"));
        assert_eq!(extract_year(Some("2017-06-12")).as_deref(), Some("2017"));
        assert_eq!(extract_year(Some("June 2017")).as_deref(), Some("2017"));
        assert_eq!(extract_year(Some("Spring 1999")).as_deref(), Some("1999"));
        assert_eq!(extract_year(Some("n.d.")), None);
        assert_eq!(extract_year(None), None);
        assert_eq!(extract_year(Some("99")), None);
    }
}
