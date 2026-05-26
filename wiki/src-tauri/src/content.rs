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
use walkdir::WalkDir;

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

/// One node in the nested category tree returned by `list_category_tree`.
///
/// A node is either a **branch** (has children, may also contain papers directly)
/// or a **leaf** (`children` is empty).  Leaf nodes are the units you click to
/// view papers; branch nodes expand/collapse.
#[derive(Debug, Serialize, Clone)]
pub struct CategoryNode {
    /// Just this folder's name, e.g. `"deep-learning"`.
    pub name: String,
    /// Path relative to `content/papers/` using `/` separators,
    /// e.g. `"machine-learning/deep-learning"`.
    pub path: String,
    /// Papers directly in this folder (excluding subdirectories).
    pub paper_count: u32,
    /// Papers in this folder **plus** all descendants — shown on branch nodes.
    pub total_paper_count: u32,
    pub latest_paper_date: Option<String>,
    /// Immediate child categories, sorted by name.
    pub children: Vec<CategoryNode>,
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

// ── Category tree helpers ──────────────────────────────────────────────────────

/// Recursively build the category tree rooted at `dir`.
///
/// `papers_dir` is the absolute path of `content/papers/` and is used to
/// compute `CategoryNode::path` (relative, forward-slash separated).
fn build_category_tree(papers_dir: &Path, dir: &Path) -> Vec<CategoryNode> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };

    let mut nodes = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_path = entry.path();

        if !entry_path.is_dir() || !is_user_category(&name) {
            continue;
        }

        // Relative path from papers_dir (forward-slash, e.g. "ml/deep-learning")
        let rel_path = entry_path
            .strip_prefix(papers_dir)
            .map(|r| r.to_string_lossy().replace('\\', "/").to_owned())
            .unwrap_or_else(|_| name.clone());

        // Count papers directly in this folder (not in sub-folders)
        let mut paper_count = 0u32;
        let mut latest: Option<SystemTime> = None;

        if let Ok(sub_entries) = fs::read_dir(&entry_path) {
            for sub in sub_entries.flatten() {
                let fname = sub.file_name().to_string_lossy().to_string();
                if !fname.ends_with(".md") || fname.starts_with('.') {
                    continue;
                }
                paper_count += 1;
                if let Ok(t) = sub.metadata().and_then(|m| m.modified()) {
                    if latest.map(|l| t > l).unwrap_or(true) {
                        latest = Some(t);
                    }
                }
            }
        }

        // Recurse into children first so we can aggregate their totals
        let children = build_category_tree(papers_dir, &entry_path);

        let child_total: u32 = children.iter().map(|c| c.total_paper_count).sum();
        let total_paper_count = paper_count + child_total;

        let self_latest = latest.map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.to_rfc3339()
        });
        let child_latest: Option<String> = children
            .iter()
            .filter_map(|c| c.latest_paper_date.clone())
            .max();
        let latest_paper_date = match (self_latest, child_latest) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };

        nodes.push(CategoryNode {
            name,
            path: rel_path,
            paper_count,
            total_paper_count,
            latest_paper_date,
            children,
        });
    }

    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes
}

/// Return the full category tree rooted at `content/papers/`.
///
/// Each node carries a `path` relative to `papers/` using forward-slash
/// separators, which can be passed directly to `list_papers_in_category`.
/// Sorted alphabetically at every level.  Excludes `unclassified/` and
/// `.staging/`.
#[tauri::command]
pub fn list_category_tree(content_root: String) -> Result<Vec<CategoryNode>, String> {
    let papers_dir = PathBuf::from(&content_root).join("papers");
    if !papers_dir.is_dir() {
        return Err(format!(
            "papers directory not found: {}",
            papers_dir.display()
        ));
    }
    Ok(build_category_tree(&papers_dir, &papers_dir))
}

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
/// Scans nested subdirectories recursively so papers in sub-categories are
/// included.  Used by the dashboard "Recent additions" feed.
#[tauri::command]
pub fn list_recent_papers(
    content_root: String,
    limit: usize,
) -> Result<Vec<PaperFrontmatter>, String> {
    let papers_dir = PathBuf::from(&content_root).join("papers");
    let mut all: Vec<PaperFrontmatter> = Vec::new();

    for entry in WalkDir::new(&papers_dir)
        .min_depth(2) // skip papers_dir itself and top-level dirs; files are min_depth=2
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let n = e.file_name().to_string_lossy();
            n.ends_with(".md") && !n.starts_with('.')
        })
    {
        let path = entry.path();
        let parent = match path.parent() {
            Some(p) => p,
            None => continue,
        };
        let rel = match parent.strip_prefix(&papers_dir) {
            Ok(r) => r.to_string_lossy().replace('\\', "/").to_owned(),
            Err(_) => continue,
        };
        // Skip system folders (unclassified, .staging, …)
        let top = rel.split('/').next().unwrap_or(&rel);
        if !is_user_category(top) || rel.is_empty() {
            continue;
        }
        if let Ok(paper) = read_markdown_paper(path, &rel) {
            all.push(paper);
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

/// Resolve a slug back to its containing category path.
///
/// The single-paper route is `/papers/[slug]`, but the markdown file lives at
/// `papers/<category-path>/<slug>.md`.  Searches recursively so papers in
/// nested sub-categories are found.  Returns a path relative to
/// `content/papers/` using forward-slash separators.
#[tauri::command]
pub fn find_paper_category(
    content_root: String,
    slug: String,
) -> Result<String, String> {
    let papers_dir = PathBuf::from(&content_root).join("papers");
    let target = format!("{slug}.md");

    for entry in WalkDir::new(&papers_dir)
        .min_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.file_name().to_string_lossy() == target.as_str())
    {
        let path = entry.path();
        let parent = match path.parent() {
            Some(p) => p,
            None => continue,
        };
        let rel = match parent.strip_prefix(&papers_dir) {
            Ok(r) => r.to_string_lossy().replace('\\', "/").to_owned(),
            Err(_) => continue,
        };
        let top = rel.split('/').next().unwrap_or(&rel);
        if !is_user_category(top) || rel.is_empty() {
            continue;
        }
        return Ok(rel);
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Fixture with two classified categories for cross-folder scans.
    fn fixture_multi_category() -> (TempDir, String) {
        let (dir, root) = fixture_root();
        let cv = PathBuf::from(&root)
            .join("papers")
            .join("computer-vision");
        fs::create_dir_all(&cv).unwrap();
        fs::write(
            cv.join("resnet.md"),
            "---\ntitle: ResNet\nyear: 2015\n---\n\nCNN body.",
        )
        .unwrap();
        (dir, root)
    }

    /// Build a minimal `content/` tree and return the content-root path.
    fn fixture_root() -> (TempDir, String) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().join("content");
        let papers = root.join("papers");
        let cat = papers.join("large-language-models");
        let uncl = papers.join("unclassified");
        fs::create_dir_all(&cat).unwrap();
        fs::create_dir_all(&uncl).unwrap();
        fs::create_dir_all(root.join("meta")).unwrap();

        fs::write(
            cat.join("attention.md"),
            "---\ntitle: Attention Is All You Need\nyear: 2017\n---\n\nBody text.",
        )
        .unwrap();
        fs::write(
            cat.join("bert.md"),
            "No frontmatter here — plain body only.",
        )
        .unwrap();
        fs::write(uncl.join("draft.md"), "---\ntitle: Draft\n---\n").unwrap();
        fs::write(
            root.join("meta").join("backlinks.json"),
            r#"{"attention":["bert"]}"#,
        )
        .unwrap();

        (dir, root.to_string_lossy().to_string())
    }

    #[test]
    fn extract_frontmatter_block_parses_yaml() {
        let md = "---\ntitle: Foo\nyear: 2024\n---\n\nBody";
        assert_eq!(extract_frontmatter_block(md), "title: Foo\nyear: 2024");
    }

    #[test]
    fn extract_frontmatter_block_empty_when_missing() {
        assert_eq!(extract_frontmatter_block("no frontmatter"), "");
        assert_eq!(extract_frontmatter_block("---\nunclosed"), "");
    }

    #[test]
    fn mtime_iso_returns_rfc3339_for_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("paper.md");
        fs::write(&path, "body").unwrap();
        let iso = mtime_iso(&path).expect("mtime");
        assert!(iso.contains('T'));
    }

    #[test]
    fn is_user_category_excludes_system_folders() {
        assert!(is_user_category("large-language-models"));
        assert!(!is_user_category("unclassified"));
        assert!(!is_user_category(".staging"));
        assert!(!is_user_category(".hidden"));
    }

    #[test]
    fn list_categories_returns_sorted_user_categories() {
        let (_dir, root) = fixture_root();
        let cats = list_categories(root).unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "large-language-models");
        assert_eq!(cats[0].paper_count, 2);
        assert!(cats[0].latest_paper_date.is_some());
    }

    #[test]
    fn list_papers_in_category_returns_frontmatter() {
        let (_dir, root) = fixture_root();
        let papers = list_papers_in_category(root, "large-language-models".into()).unwrap();
        assert_eq!(papers.len(), 2);
        let attention = papers.iter().find(|p| p.slug == "attention").unwrap();
        assert!(attention.frontmatter.contains("title: Attention"));
        assert!(!attention.created_at.is_empty());
    }

    #[test]
    fn list_recent_papers_respects_limit() {
        let (_dir, root) = fixture_root();
        let recent = list_recent_papers(root, 1).unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn list_recent_papers_scans_every_category() {
        let (_dir, root) = fixture_multi_category();
        let recent = list_recent_papers(root, 10).unwrap();
        assert_eq!(recent.len(), 3);
        let slugs: Vec<_> = recent.iter().map(|p| p.slug.as_str()).collect();
        assert!(slugs.contains(&"attention"));
        assert!(slugs.contains(&"resnet"));
    }

    #[test]
    fn read_paper_file_returns_full_content() {
        let (_dir, root) = fixture_root();
        let paper = read_paper_file(root, "large-language-models".into(), "attention".into())
            .unwrap();
        assert_eq!(paper.slug, "attention");
        assert!(paper.content.contains("Body text."));
    }

    #[test]
    fn find_paper_category_locates_slug() {
        let (_dir, root) = fixture_root();
        let cat = find_paper_category(root, "attention".into()).unwrap();
        assert_eq!(cat, "large-language-models");
    }

    #[test]
    fn find_paper_category_finds_second_category() {
        let (_dir, root) = fixture_multi_category();
        let cat = find_paper_category(root, "resnet".into()).unwrap();
        assert_eq!(cat, "computer-vision");
    }

    #[test]
    fn list_papers_in_category_errors_for_missing_dir() {
        let (_dir, root) = fixture_root();
        assert!(list_papers_in_category(root, "no-such-category".into()).is_err());
    }

    #[test]
    fn find_paper_category_errors_when_missing() {
        let (_dir, root) = fixture_root();
        assert!(find_paper_category(root, "nonexistent".into()).is_err());
    }

    #[test]
    fn list_unclassified_lists_pending_files() {
        let (_dir, root) = fixture_root();
        let pending = list_unclassified(root).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "draft.md");
    }

    #[test]
    fn read_backlinks_returns_json() {
        let (_dir, root) = fixture_root();
        let json = read_backlinks(root).unwrap();
        assert!(json.contains("attention"));
    }

    #[test]
    fn list_categories_errors_when_papers_dir_missing() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("content");
        fs::create_dir_all(&root).unwrap();
        assert!(list_categories(root.to_string_lossy().to_string()).is_err());
    }

    #[test]
    fn read_backlinks_missing_file_returns_empty_object() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("content");
        fs::create_dir_all(root.join("meta")).unwrap();
        let json = read_backlinks(root.to_string_lossy().to_string()).unwrap();
        assert_eq!(json, "{}");
    }

    // ── CategoryNode tree tests ────────────────────────────────────────────────

    /// Flat structure: one top-level category → tree should have one root node,
    /// no children, and the same paper count as `list_categories`.
    #[test]
    fn list_category_tree_flat_structure() {
        let (_dir, root) = fixture_root();
        let tree = list_category_tree(root).unwrap();
        assert_eq!(tree.len(), 1);
        let node = &tree[0];
        assert_eq!(node.name, "large-language-models");
        assert_eq!(node.path, "large-language-models");
        assert_eq!(node.paper_count, 2);
        assert_eq!(node.total_paper_count, 2);
        assert!(node.children.is_empty());
        assert!(node.latest_paper_date.is_some());
    }

    /// Nested structure: parent folder + child sub-folder.
    /// Aggregate paper counts must bubble up correctly.
    #[test]
    fn list_category_tree_nested_structure() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("content");
        let papers = root.join("papers");

        // parent folder with 1 direct paper
        let ml = papers.join("ml");
        fs::create_dir_all(&ml).unwrap();
        fs::write(ml.join("survey.md"), "---\ntitle: Survey\n---\n").unwrap();

        // child sub-folder with 2 papers
        let llm = ml.join("llm");
        fs::create_dir_all(&llm).unwrap();
        fs::write(llm.join("gpt.md"), "---\ntitle: GPT\n---\n").unwrap();
        fs::write(llm.join("bert.md"), "---\ntitle: BERT\n---\n").unwrap();

        // unrelated root category (should be a sibling root node)
        let cv = papers.join("computer-vision");
        fs::create_dir_all(&cv).unwrap();
        fs::write(cv.join("resnet.md"), "---\ntitle: ResNet\n---\n").unwrap();

        let root_str = root.to_string_lossy().to_string();
        let tree = list_category_tree(root_str).unwrap();

        // Two root nodes: "computer-vision" and "ml" (sorted)
        assert_eq!(tree.len(), 2);
        let cv_node = tree.iter().find(|n| n.name == "computer-vision").unwrap();
        let ml_node = tree.iter().find(|n| n.name == "ml").unwrap();

        assert_eq!(cv_node.paper_count, 1);
        assert_eq!(cv_node.total_paper_count, 1);
        assert!(cv_node.children.is_empty());

        // "ml" has 1 direct paper + 2 in "llm" child
        assert_eq!(ml_node.paper_count, 1);
        assert_eq!(ml_node.total_paper_count, 3);
        assert_eq!(ml_node.children.len(), 1);

        let llm_node = &ml_node.children[0];
        assert_eq!(llm_node.name, "llm");
        assert_eq!(llm_node.path, "ml/llm");
        assert_eq!(llm_node.paper_count, 2);
        assert_eq!(llm_node.total_paper_count, 2);
        assert!(llm_node.children.is_empty());
    }

    /// `find_paper_category` must return the relative nested path, not just
    /// the top-level folder name.
    #[test]
    fn find_paper_category_nested_path() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("content");
        let papers = root.join("papers");
        let nested = papers.join("ml").join("llm");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("gpt.md"), "---\ntitle: GPT\n---\n").unwrap();

        let root_str = root.to_string_lossy().to_string();
        let cat = find_paper_category(root_str, "gpt".into()).unwrap();
        assert_eq!(cat, "ml/llm");
    }

    /// `list_recent_papers` must find papers in nested sub-directories.
    #[test]
    fn list_recent_papers_recurses_nested_folders() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("content");
        let papers = root.join("papers");

        let nested = papers.join("ml").join("llm");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("gpt.md"), "---\ntitle: GPT\n---\n").unwrap();

        let top_level = papers.join("computer-vision");
        fs::create_dir_all(&top_level).unwrap();
        fs::write(top_level.join("resnet.md"), "---\ntitle: ResNet\n---\n").unwrap();

        let root_str = root.to_string_lossy().to_string();
        let recent = list_recent_papers(root_str, 10).unwrap();
        assert_eq!(recent.len(), 2);
        let slugs: Vec<_> = recent.iter().map(|p| p.slug.as_str()).collect();
        assert!(slugs.contains(&"gpt"));
        assert!(slugs.contains(&"resnet"));
    }
}
