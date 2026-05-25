//! Content folder reader — exposes read-only access to `content/papers/` and
//! `content/meta/` so the Next.js frontend can render the wiki at runtime
//! without bundling everything at build time.
//!
//! All commands accept an explicit `content_root` argument supplied by the
//! frontend (read from `localStorage['content-root']`).  None of these
//! commands ever WRITE to disk — that responsibility belongs to the
//! organiser pipeline (`organizer.rs`).
//!
//! Frontmatter parsing is intentionally **not** done in Rust: each command
//! returns either the full markdown body or just the raw YAML frontmatter
//! block, and the JS layer parses it with `gray-matter`.  This keeps the
//! Rust dependency tree small and lets the frontend evolve its frontmatter
//! schema without round-tripping through here.
//!
//! # Tauri commands exposed
//! | Command                   | Returns                          | Description                                |
//! |---------------------------|----------------------------------|--------------------------------------------|
//! | `list_categories`         | `Vec<CategoryInfo>`              | All non-`unclassified` category folders    |
//! | `list_papers_in_category` | `Vec<PaperFrontmatter>`          | Frontmatter for every paper in `<cat>/`    |
//! | `list_recent_papers`      | `Vec<PaperFrontmatter>`          | Top-N papers across categories by mtime    |
//! | `read_paper_file`         | `PaperFile`                      | Full markdown text for one paper           |
//! | `find_paper_category`     | `Result<String, String>`         | Locate which category contains a slug      |
//! | `list_unclassified`       | `Vec<UnclassifiedPaper>`         | Files awaiting organisation                |
//! | `read_backlinks`          | `String`                         | Raw JSON contents of `meta/backlinks.json` |

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Folders directly under `content/papers/` that are NOT user-facing categories.
const EXCLUDED_TOP_LEVEL: &[&str] = &["unclassified", ".staging"];

// ── Public types ──────────────────────────────────────────────────────────────

/// One category card shown on the dashboard.
#[derive(Debug, Serialize)]
pub struct CategoryInfo {
    /// Lower-case kebab-case folder name (matches Zotero collection name).
    pub name: String,
    pub paper_count: u32,
    /// Latest paper mtime as ISO 8601 string, or `None` if the category is empty.
    pub latest_paper_date: Option<String>,
}

/// Lightweight paper descriptor used for list views.
/// Body is **not** included — only the raw YAML frontmatter block.
#[derive(Debug, Serialize, Clone)]
pub struct PaperFrontmatter {
    pub slug: String,
    pub category: String,
    /// Raw YAML block between the leading and trailing `---` delimiters
    /// (empty string if the file has no frontmatter).
    pub frontmatter: String,
    /// ISO 8601 mtime — used as a stand-in for "created at".
    pub created_at: String,
}

/// Full paper payload for the single-paper view.
#[derive(Debug, Serialize)]
pub struct PaperFile {
    pub slug: String,
    pub category: String,
    /// Entire markdown file including frontmatter — frontend parses with gray-matter.
    pub content: String,
    pub created_at: String,
}

/// A file sitting in `content/papers/unclassified/` waiting to be organised.
#[derive(Debug, Serialize)]
pub struct UnclassifiedPaper {
    /// Absolute filesystem path — pass back to `process_paper`.
    pub path: String,
    pub name: String,
    pub created_at: String,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn mtime_iso(p: &Path) -> Option<String> {
    let t: SystemTime = fs::metadata(p).ok()?.modified().ok()?;
    let dt: DateTime<Utc> = t.into();
    Some(dt.to_rfc3339())
}

/// Extract the raw text inside the leading `---` … `---` YAML block.
/// Returns an empty string if the file has no frontmatter.
fn extract_frontmatter_block(content: &str) -> String {
    let Some(rest) = content.strip_prefix("---") else {
        return String::new();
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    match rest.find("\n---") {
        Some(end) => rest[..end].to_string(),
        None => String::new(),
    }
}

fn is_user_category(name: &str) -> bool {
    !name.starts_with('.') && !EXCLUDED_TOP_LEVEL.contains(&name)
}

fn read_markdown_paper(
    path: &Path,
    category: &str,
) -> Result<PaperFrontmatter, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = name.trim_end_matches(".md").to_string();
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("read failed for {name}: {e}"))?;
    Ok(PaperFrontmatter {
        slug,
        category: category.to_string(),
        frontmatter: extract_frontmatter_block(&raw),
        created_at: mtime_iso(path).unwrap_or_default(),
    })
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// List every classified category folder under `content/papers/`.
///
/// Excludes `unclassified/` and `.staging/`.  Sorted by name (ascending).
/// `paper_count` and `latest_paper_date` are computed by scanning each folder.
#[tauri::command]
pub fn list_categories(content_root: String) -> Result<Vec<CategoryInfo>, String> {
    let papers_dir = PathBuf::from(&content_root).join("papers");
    let mut categories = Vec::new();

    let entries = fs::read_dir(&papers_dir).map_err(|e| {
        format!("read_dir failed for {}: {e}", papers_dir.display())
    })?;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_user_category(&name) || !entry.path().is_dir() {
            continue;
        }

        let mut paper_count = 0u32;
        let mut latest: Option<SystemTime> = None;

        if let Ok(papers) = fs::read_dir(entry.path()) {
            for p in papers.flatten() {
                let fname = p.file_name().to_string_lossy().to_string();
                if !fname.ends_with(".md") || fname.starts_with('.') {
                    continue;
                }
                paper_count += 1;
                if let Ok(t) = p.metadata().and_then(|m| m.modified()) {
                    if latest.map(|l| t > l).unwrap_or(true) {
                        latest = Some(t);
                    }
                }
            }
        }

        let latest_iso = latest.map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.to_rfc3339()
        });

        categories.push(CategoryInfo {
            name,
            paper_count,
            latest_paper_date: latest_iso,
        });
    }

    categories.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(categories)
}

/// List every paper in a single category folder.
///
/// Returns frontmatter blocks (not full bodies) so list views can render
/// without copying entire markdown files across the IPC bridge.
/// Sorted by newest `created_at` first.
#[tauri::command]
pub fn list_papers_in_category(
    content_root: String,
    category: String,
) -> Result<Vec<PaperFrontmatter>, String> {
    let dir = PathBuf::from(&content_root).join("papers").join(&category);
    let mut papers = Vec::new();

    let entries = fs::read_dir(&dir).map_err(|e| {
        format!("read_dir failed for {}: {e}", dir.display())
    })?;

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        match read_markdown_paper(&path, &category) {
            Ok(p) => papers.push(p),
            Err(_) => continue,
        }
    }

    papers.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(papers)
}

/// Top-N most recently modified papers across every category folder.
///
/// Used by the dashboard "Recent additions" feed.
#[tauri::command]
pub fn list_recent_papers(
    content_root: String,
    limit: usize,
) -> Result<Vec<PaperFrontmatter>, String> {
    let papers_dir = PathBuf::from(&content_root).join("papers");
    let mut all: Vec<PaperFrontmatter> = Vec::new();

    let entries = fs::read_dir(&papers_dir).map_err(|e| {
        format!("read_dir failed for {}: {e}", papers_dir.display())
    })?;

    for entry in entries.flatten() {
        let cat = entry.file_name().to_string_lossy().to_string();
        if !is_user_category(&cat) || !entry.path().is_dir() {
            continue;
        }
        if let Ok(mut papers) = list_papers_in_category(content_root.clone(), cat) {
            all.append(&mut papers);
        }
    }

    all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    all.truncate(limit);
    Ok(all)
}

/// Return the full markdown text (frontmatter + body) for one paper.
#[tauri::command]
pub fn read_paper_file(
    content_root: String,
    category: String,
    slug: String,
) -> Result<PaperFile, String> {
    let path = PathBuf::from(&content_root)
        .join("papers")
        .join(&category)
        .join(format!("{slug}.md"));

    let content = fs::read_to_string(&path)
        .map_err(|e| format!("read failed for {}: {e}", path.display()))?;
    let created_at = mtime_iso(&path).unwrap_or_default();

    Ok(PaperFile {
        slug,
        category,
        content,
        created_at,
    })
}

/// Resolve a slug back to its containing category folder.
///
/// The single-paper route is `/papers/[slug]`, but the markdown file lives at
/// `papers/<category>/<slug>.md` — this command scans the user-category
/// folders to find the match.
#[tauri::command]
pub fn find_paper_category(
    content_root: String,
    slug: String,
) -> Result<String, String> {
    let papers_dir = PathBuf::from(&content_root).join("papers");

    let entries = fs::read_dir(&papers_dir).map_err(|e| {
        format!("read_dir failed for {}: {e}", papers_dir.display())
    })?;

    for entry in entries.flatten() {
        let cat = entry.file_name().to_string_lossy().to_string();
        if !is_user_category(&cat) || !entry.path().is_dir() {
            continue;
        }
        let candidate = entry.path().join(format!("{slug}.md"));
        if candidate.exists() {
            return Ok(cat);
        }
    }
    Err(format!("Paper not found: {slug}"))
}

/// List every `.md` file currently in `content/papers/unclassified/`.
///
/// Used by the dashboard "Organize Now" button to enumerate work for the
/// `process_paper` pipeline.  Missing folder → empty `Vec`.
#[tauri::command]
pub fn list_unclassified(
    content_root: String,
) -> Result<Vec<UnclassifiedPaper>, String> {
    let dir = PathBuf::from(&content_root)
        .join("papers")
        .join("unclassified");
    let mut out = Vec::new();

    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        out.push(UnclassifiedPaper {
            path: path.to_string_lossy().to_string(),
            name,
            created_at: mtime_iso(&path).unwrap_or_default(),
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Return the raw JSON contents of `content/meta/backlinks.json`.
///
/// Returns the string `"{}"` if the file does not exist yet.
#[tauri::command]
pub fn read_backlinks(content_root: String) -> Result<String, String> {
    let path = PathBuf::from(&content_root)
        .join("meta")
        .join("backlinks.json");
    if !path.exists() {
        return Ok("{}".into());
    }
    fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))
}
