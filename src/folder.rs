//! The Markdown files of an opened folder and of the folders below it, as the sidebar
//! lists them. Pure logic with no GTK dependency; synchronous IO.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

/// The most files a listing holds. A folder with more, a home folder opened by mistake,
/// is listed in part rather than walked for minutes.
pub const MAX_FILES: usize = 5000;

/// Folders never walked into besides hidden ones: what package managers and build tools
/// fill with files nobody writes by hand.
const SKIPPED_FOLDERS: &[&str] = &["node_modules", "target"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    /// What a folder holds, folders first; `None` for a file.
    pub children: Option<Vec<Entry>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// Files past [`MAX_FILES`] were left out.
    pub truncated: bool,
}

/// The Markdown files in `root` and below it, and the folders that lead to them. Hidden
/// files and folders are left out, and so are folders reached through a symbolic link,
/// which could lead back up; a link to a file is listed. Folders that cannot be read are
/// passed over.
pub fn scan(root: &Path, max_files: usize) -> Listing {
    let mut listing = Listing::default();
    let mut count = 0;
    listing.entries = scan_folder(root, max_files, &mut count, &mut listing.truncated);
    listing
}

fn scan_folder(
    folder: &Path,
    max_files: usize,
    count: &mut usize,
    truncated: &mut bool,
) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf, bool)> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let file_type = entry.file_type().ok()?;
            let path = entry.path();
            if file_type.is_dir() {
                (!SKIPPED_FOLDERS.contains(&name.as_str())).then_some((name, path, true))
            } else if is_markdown(&path) && fs::metadata(&path).is_ok_and(|meta| meta.is_file()) {
                Some((name, path, false))
            } else {
                None
            }
        })
        .collect();
    found.sort_by(|a, b| order(&a.0, a.2, &b.0, b.2));

    let mut entries = Vec::new();
    for (name, path, is_folder) in found {
        if *truncated {
            break;
        }
        if is_folder {
            let children = scan_folder(&path, max_files, count, truncated);
            if !children.is_empty() {
                entries.push(Entry {
                    name,
                    path,
                    children: Some(children),
                });
            }
        } else if *count == max_files {
            *truncated = true;
        } else {
            *count += 1;
            entries.push(Entry {
                name,
                path,
                children: None,
            });
        }
    }
    entries
}

/// The files of `entries` whose names hold `query`, ignoring case, with the folders
/// that lead to them. A folder whose own name holds it keeps all it has.
pub fn filter(entries: &[Entry], query: &str) -> Vec<Entry> {
    let query = query.to_lowercase();
    filter_lowercase(entries, &query)
}

fn filter_lowercase(entries: &[Entry], query: &str) -> Vec<Entry> {
    entries
        .iter()
        .filter_map(|entry| {
            if entry.name.to_lowercase().contains(query) {
                return Some(entry.clone());
            }
            let children = filter_lowercase(entry.children.as_deref()?, query);
            (!children.is_empty()).then(|| Entry {
                children: Some(children),
                ..entry.clone()
            })
        })
        .collect()
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    })
}

/// Folders before files, each by name regardless of case.
fn order(a: &str, a_is_folder: bool, b: &str, b_is_folder: bool) -> Ordering {
    b_is_folder
        .cmp(&a_is_folder)
        .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
        .then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(tag: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "blink-folder-{}-{}-{}",
            tag,
            std::process::id(),
            suffix
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    /// The listing as indented names, to compare whole trees at once.
    fn outline(entries: &[Entry]) -> Vec<String> {
        fn walk(entries: &[Entry], depth: usize, lines: &mut Vec<String>) {
            for entry in entries {
                let slash = if entry.children.is_some() { "/" } else { "" };
                lines.push(format!("{}{}{slash}", "  ".repeat(depth), entry.name));
                if let Some(children) = &entry.children {
                    walk(children, depth + 1, lines);
                }
            }
        }
        let mut lines = Vec::new();
        walk(entries, 0, &mut lines);
        lines
    }

    #[test]
    fn lists_markdown_below_the_root_folders_first() {
        let root = unique_dir("tree");
        touch(&root, "README.md");
        touch(&root, "CONTRIBUTING.md");
        touch(&root, "Cargo.toml");
        touch(&root, "docs/usage.md");
        touch(&root, "docs/Install.MARKDOWN");
        touch(&root, "docs/api/index.md");
        touch(&root, "src/main.rs");

        let listing = scan(&root, MAX_FILES);
        assert!(!listing.truncated);
        assert_eq!(
            outline(&listing.entries),
            [
                "docs/",
                "  api/",
                "    index.md",
                "  Install.MARKDOWN",
                "  usage.md",
                "CONTRIBUTING.md",
                "README.md",
            ]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn skips_hidden_and_tool_folders() {
        let root = unique_dir("skipped");
        touch(&root, "notes.md");
        touch(&root, ".hidden.md");
        touch(&root, ".git/description.md");
        touch(&root, ".github/pull_request_template.md");
        touch(&root, "node_modules/left-pad/README.md");
        touch(&root, "target/doc/index.md");

        let listing = scan(&root, MAX_FILES);
        assert_eq!(outline(&listing.entries), ["notes.md"]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn follows_links_to_files_but_not_to_folders() {
        let root = unique_dir("links");
        touch(&root, "real/page.md");
        std::os::unix::fs::symlink(root.join("real/page.md"), root.join("linked.md")).unwrap();
        // A link back up would otherwise be walked without end.
        std::os::unix::fs::symlink(&root, root.join("real/loop")).unwrap();
        std::os::unix::fs::symlink(root.join("missing.md"), root.join("broken.md")).unwrap();

        let listing = scan(&root, MAX_FILES);
        assert_eq!(
            outline(&listing.entries),
            ["real/", "  page.md", "linked.md"]
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn leaves_out_folders_without_markdown() {
        let root = unique_dir("empty");
        touch(&root, "src/lib.rs");
        fs::create_dir_all(root.join("empty/deeper")).unwrap();

        assert_eq!(scan(&root, MAX_FILES), Listing::default());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stops_at_the_limit_and_says_so() {
        let root = unique_dir("limit");
        for name in ["a.md", "b.md", "c.md", "sub/d.md"] {
            touch(&root, name);
        }

        let listing = scan(&root, 3);
        assert!(listing.truncated);
        assert_eq!(
            outline(&listing.entries),
            ["sub/", "  d.md", "a.md", "b.md"]
        );

        let listing = scan(&root, 4);
        assert!(!listing.truncated);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn filter_keeps_matches_and_the_folders_to_them() {
        let root = unique_dir("filter");
        for name in [
            "README.md",
            "docs/usage.md",
            "docs/install.md",
            "docs/api/Usage-notes.md",
            "guides/intro.md",
            "notes/todo.md",
        ] {
            touch(&root, name);
        }
        let listing = scan(&root, MAX_FILES);

        assert_eq!(
            outline(&filter(&listing.entries, "USAGE")),
            ["docs/", "  api/", "    Usage-notes.md", "  usage.md"]
        );
        // A folder that matches by name keeps its files, matching or not.
        assert_eq!(
            outline(&filter(&listing.entries, "guide")),
            ["guides/", "  intro.md"]
        );
        assert!(filter(&listing.entries, "missing").is_empty());
        assert_eq!(filter(&listing.entries, ""), listing.entries);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_root_is_empty() {
        let root = unique_dir("missing").join("gone");
        assert_eq!(scan(&root, MAX_FILES), Listing::default());
    }
}
