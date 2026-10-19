//! `docs/decisions/`: one file per decision, so two pull requests never conflict over one log
//! (#295, #303). Checks file names and headings, and that no entry lands in `docs/decisions.md`
//! again (a branch made before the split brought the whole old log back once, #312).

use std::path::Path;

/// Undated files in `docs/decisions/` that are documented redirects, not records.
const REDIRECTS: &[&str] = &["plugins-rfc.md"];

/// Checks every decision file under `root`.
pub fn check(root: &Path) -> Result<(), String> {
    let index = std::fs::read_to_string(root.join("docs/decisions.md")).map_err(|e| format!("docs/decisions.md: {e}"))?;
    let mut problems: Vec<String> = index_problems(&index).into_iter().map(|p| format!("docs/decisions.md: {p}")).collect();
    let dir = root.join("docs/decisions");
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut count = 0usize;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            problems.push(format!("{}: the file name isn't UTF-8", path.display()));
            continue;
        };
        // Hidden files (`.DS_Store` from Finder, editor swap files) aren't entries.
        if name.starts_with('.') || REDIRECTS.contains(&name.as_str()) {
            continue;
        }
        if !path.is_file() {
            problems.push(format!("docs/decisions/{name}: only decision files belong here, not folders"));
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("docs/decisions/{name}: {e}"))?;
        problems.extend(file_problems(&name, &text).into_iter().map(|p| format!("docs/decisions/{name}: {p}")));
        count = count.saturating_add(1);
    }
    if problems.is_empty() {
        println!("Decisions: {count} entries in docs/decisions/ ok");
        Ok(())
    } else {
        Err(format!("decision log problems (see docs/decisions.md):\n  {}", problems.join("\n  ")))
    }
}

/// The index explains the folder; entries never go back into it.
fn index_problems(index: &str) -> Vec<String> {
    index
        .lines()
        .filter(|line| line.starts_with("## ") && date_prefix(line.get(3..).unwrap_or("")).is_some())
        .map(|line| format!("`{line}` is an entry: move it to its own file in docs/decisions/"))
        .collect()
}

/// `YYYY-MM-DD` at the start of `text`, if it is one.
fn date_prefix(text: &str) -> Option<&str> {
    let date = text.get(..10)?;
    let shaped = date.char_indices().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    shaped.then_some(date)
}

fn file_problems(name: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(stem) = name.strip_suffix(".md") else {
        return vec!["decision files are Markdown (`.md`)".into()];
    };
    let Some(date) = date_prefix(stem) else {
        return vec!["the name must start with the date, `YYYY-MM-DD-`".into()];
    };
    let slug = stem.get(11..).unwrap_or("");
    if stem.get(10..11) != Some("-") || slug.is_empty() || !slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        problems.push("the name must be `YYYY-MM-DD-<slug>.md`, the slug lowercase letters, digits and hyphens".into());
    }
    // Records moved by #303 keep their original blank separators, so skip leading blank lines.
    let heading = text.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    if !heading.starts_with(&format!("## {date} · ")) {
        problems.push(format!("the first line must be `## {date} · <Title>`, with the file's date"));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_entries_pass() {
        assert!(file_problems("2026-10-17-x.md", "\n\n## 2026-10-17 · Moved record (#1)\n").is_empty(), "leading blank lines");
        assert!(file_problems("2026-10-18-history-81.md", "## 2026-10-18 · Generation history (#81)\n\n- **A.**\n").is_empty());
    }

    #[test]
    fn names_and_headings_are_checked() {
        assert_eq!(file_problems("notes.txt", "").len(), 1);
        assert_eq!(file_problems("history.md", "## 2026-10-18 · X").len(), 1);
        assert_eq!(file_problems("2026-10-18-History_81.md", "## 2026-10-18 · X").len(), 1, "slug case and underscore");
        assert_eq!(file_problems("2026-10-18.md", "## 2026-10-18 · X").len(), 1, "no slug");
        assert_eq!(file_problems("2026-10-18-x.md", "## 2026-10-17 · X").len(), 1, "the heading date must match");
        assert_eq!(file_problems("2026-10-18-x.md", "# 2026-10-18 · X").len(), 1, "a level-two heading, as the index asks");
        assert_eq!(file_problems("2026-1a-18-x.md", "").len(), 1);
        assert_eq!(file_problems("2026-10-18-ü.md", "## 2026-10-18 · X").len(), 1, "non-ASCII names are refused, not sliced");
    }

    #[test]
    fn entries_appended_to_the_index_are_refused() {
        assert!(index_problems("# Decisions\n\n## How to add one\n").is_empty());
        assert_eq!(index_problems("# Decisions\n\n## 2026-10-18 · New thing (#1)\n- x\n").len(), 1);
    }
}
