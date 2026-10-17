//! Vault scanning and Obsidian-style link resolution.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use walkdir::WalkDir;

use crate::datefmt::DateFormat;

#[derive(Debug, Clone)]
pub struct Note {
    /// Vault-relative path with `/` separators, e.g. `folder/Note.md`.
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub stem: String,
    pub folder: String,
    pub created_ms: i64,
    pub modified_ms: i64,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub file_name: String,
    pub stem: String,
    /// Lowercase extension without the dot; may be empty.
    pub ext: String,
    pub created_ms: i64,
    pub modified_ms: i64,
}

#[derive(Debug, Clone)]
pub struct DailyConfig {
    pub format: DateFormat,
    pub folder: String,
}

/// An entry of `.obsidian/bookmarks.json`, with groups flattened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bookmark {
    /// A note, or a heading or block in it (bookmarked as the whole note).
    File { path: String },
    /// Folders, searches, URLs and graph views have no Logseq favorite.
    Other { kind: String, title: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Note(usize),
    Attachment(usize),
}

#[derive(Debug, Default)]
pub struct Vault {
    pub root: PathBuf,
    pub notes: Vec<Note>,
    pub attachments: Vec<Attachment>,
    pub daily: Option<DailyConfig>,
    /// Folder of the core Templates plugin (`.obsidian/templates.json`).
    pub templates_folder: Option<String>,
    pub bookmarks: Vec<Bookmark>,
    /// Entries that could not be read and were skipped.
    pub warnings: Vec<String>,
    notes_by_stem: HashMap<String, Vec<usize>>,
    notes_by_path: HashMap<String, usize>,
    atts_by_name: HashMap<String, Vec<usize>>,
    atts_by_path: HashMap<String, usize>,
}

impl Vault {
    /// Scans `root` for notes (`.md`) and attachments (everything else),
    /// skipping hidden entries and any vault-relative path matching `exclude`.
    pub fn scan(root: &Path, exclude: &[String]) -> Result<Vault> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot open vault {}", root.display()))?;
        if !root.is_dir() {
            bail!("vault {} is not a folder", root.display());
        }
        let mut vault = Vault {
            root: root.clone(),
            ..Default::default()
        };
        let exclude: Vec<String> = exclude
            .iter()
            .map(|e| normalize_folder(e).to_lowercase())
            .collect();
        let walker = WalkDir::new(&root)
            .follow_links(true)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
        for entry in walker {
            // Dangling symlinks, loops and unreadable folders are common in
            // synced vaults; skip them instead of failing the conversion.
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    vault
                        .warnings
                        .push(format!("skipped unreadable entry: {e}"));
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let abs_path = entry.path().to_path_buf();
            let rel = abs_path
                .strip_prefix(&root)
                .expect("walkdir yields paths under root");
            let rel_path = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let rel_lower = rel_path.to_lowercase();
            if exclude
                .iter()
                .any(|e| rel_lower == *e || rel_lower.starts_with(&format!("{e}/")))
            {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            let folder = match rel_path.rfind('/') {
                Some(i) => rel_path[..i].to_string(),
                None => String::new(),
            };
            let (created_ms, modified_ms) = file_times(&abs_path);
            let (stem, ext) = split_ext(&file_name);
            if ext.eq_ignore_ascii_case("md") {
                vault.notes.push(Note {
                    rel_path,
                    abs_path,
                    stem: stem.to_string(),
                    folder,
                    created_ms,
                    modified_ms,
                });
            } else {
                vault.attachments.push(Attachment {
                    rel_path,
                    abs_path,
                    stem: stem.to_string(),
                    ext: ext.to_lowercase(),
                    file_name,
                    created_ms,
                    modified_ms,
                });
            }
        }
        vault.build_indexes();
        vault.load_obsidian_config();
        Ok(vault)
    }

    /// Reads the templates folder and bookmarks; missing or invalid files
    /// are ignored like Obsidian does.
    fn load_obsidian_config(&mut self) {
        let read = |name: &str| -> serde_json::Value {
            fs::read_to_string(self.root.join(".obsidian").join(name))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null)
        };
        self.templates_folder = read("templates.json")["folder"]
            .as_str()
            .map(|f| f.trim_matches('/').to_string())
            .filter(|f| !f.is_empty());
        let mut bookmarks = Vec::new();
        collect_bookmarks(&read("bookmarks.json")["items"], &mut bookmarks);
        self.bookmarks = bookmarks;
    }

    /// Whether the note is in the templates folder.
    pub fn is_template(&self, note: &Note) -> bool {
        self.templates_folder.as_ref().is_some_and(|f| {
            let folder = note.folder.to_lowercase();
            let f = f.to_lowercase();
            folder == f || folder.starts_with(&format!("{f}/"))
        })
    }

    fn build_indexes(&mut self) {
        for (i, note) in self.notes.iter().enumerate() {
            self.notes_by_stem
                .entry(note.stem.to_lowercase())
                .or_default()
                .push(i);
            self.notes_by_path
                .insert(strip_md(&note.rel_path).to_lowercase(), i);
        }
        for (i, att) in self.attachments.iter().enumerate() {
            self.atts_by_name
                .entry(att.file_name.to_lowercase())
                .or_default()
                .push(i);
            self.atts_by_path.insert(att.rel_path.to_lowercase(), i);
        }
    }

    /// Reads `.obsidian/daily-notes.json` (falling back to `YYYY-MM-DD`).
    pub fn load_daily_config(
        &mut self,
        format_override: Option<&str>,
        folder_override: Option<&str>,
    ) -> Result<()> {
        let config_path = self.root.join(".obsidian").join("daily-notes.json");
        let config: serde_json::Value = fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        let format = format_override
            .map(str::to_string)
            .or_else(|| config["format"].as_str().map(str::to_string))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let folder = folder_override
            .map(str::to_string)
            .or_else(|| config["folder"].as_str().map(str::to_string))
            .map(|f| normalize_folder(&f))
            .unwrap_or_default();
        let format = DateFormat::parse(&format)?;
        self.daily = Some(DailyConfig { format, folder });
        Ok(())
    }

    /// Returns the journal date for a note, if it is a daily note.
    pub fn note_journal_date(&self, note: &Note) -> Option<NaiveDate> {
        let daily = self.daily.as_ref()?;
        let path = strip_md(&note.rel_path);
        // Like Obsidian, only notes in the daily notes folder are daily notes.
        let rel = if daily.folder.is_empty() {
            path
        } else {
            let prefix = path.get(..daily.folder.len() + 1)?;
            if !prefix.eq_ignore_ascii_case(&format!("{}/", daily.folder)) {
                return None;
            }
            &path[prefix.len()..]
        };
        if daily.format.contains_path_separator() {
            daily.format.parse_date(rel)
        } else {
            daily.format.parse_date(&note.stem)
        }
    }

    /// Parses a bare link target as a daily note date.
    pub fn parse_journal_link(&self, target: &str) -> Option<NaiveDate> {
        let daily = self.daily.as_ref()?;
        let name = target.rsplit('/').next().unwrap_or(target);
        daily
            .format
            .parse_date(target)
            .or_else(|| daily.format.parse_date(name))
    }

    /// Resolves a link target (without `#heading` / `|alias`) the way
    /// Obsidian does: exact or relative path, then path suffix, then the
    /// file name closest to the linking note.
    pub fn resolve(&self, target: &str, from_note: Option<usize>) -> Option<Target> {
        let target = target.trim().replace('\\', "/");
        if target.is_empty() {
            return None;
        }
        let from_folder = from_note
            .map(|i| self.notes[i].folder.as_str())
            .unwrap_or("");
        if target.starts_with("./") || target.starts_with("../") {
            let joined = normalize_path(&format!("{from_folder}/{target}"))?;
            return self.resolve_exact(&joined);
        }
        let target = target.trim_start_matches('/');
        if from_note.is_some()
            && let Some(t) = normalize_path(&format!("{from_folder}/{target}"))
                .and_then(|p| self.resolve_exact(&p))
        {
            return Some(t);
        }
        if let Some(t) = self.resolve_exact(target) {
            return Some(t);
        }
        let lower = target.to_lowercase();
        let lower_no_md = strip_md_ci(&lower);
        if lower.contains('/') {
            let suffix = format!("/{lower_no_md}");
            let note = self
                .notes_by_path
                .iter()
                .filter(|(p, _)| p.ends_with(&suffix))
                .map(|(_, &i)| i)
                .min_by_key(|&i| (self.notes[i].rel_path.len(), i));
            if let Some(i) = note {
                return Some(Target::Note(i));
            }
            let suffix = format!("/{lower}");
            return self
                .atts_by_path
                .iter()
                .filter(|(p, _)| p.ends_with(&suffix))
                .map(|(_, &i)| i)
                .min_by_key(|&i| (self.attachments[i].rel_path.len(), i))
                .map(Target::Attachment);
        }
        if let Some(candidates) = self.notes_by_stem.get(lower_no_md) {
            return Some(Target::Note(
                self.closest(candidates, from_folder, |i| &self.notes[i].rel_path),
            ));
        }
        if let Some(candidates) = self.atts_by_name.get(&lower) {
            return Some(Target::Attachment(self.closest(
                candidates,
                from_folder,
                |i| &self.attachments[i].rel_path,
            )));
        }
        None
    }

    fn resolve_exact(&self, path: &str) -> Option<Target> {
        let lower = path.to_lowercase();
        if let Some(&i) = self.notes_by_path.get(strip_md_ci(&lower)) {
            return Some(Target::Note(i));
        }
        self.atts_by_path
            .get(&lower)
            .map(|&i| Target::Attachment(i))
    }

    fn closest<'a>(
        &'a self,
        candidates: &[usize],
        from_folder: &str,
        path_of: impl Fn(usize) -> &'a str,
    ) -> usize {
        let folder_of = |p: &'a str| p.rfind('/').map(|i| &p[..i]).unwrap_or("");
        *candidates
            .iter()
            .min_by_key(|&&i| {
                let p = path_of(i);
                (
                    folder_of(p) != from_folder,
                    p.matches('/').count(),
                    p.len(),
                    i,
                )
            })
            .expect("candidate lists are never empty")
    }
}

fn collect_bookmarks(items: &serde_json::Value, out: &mut Vec<Bookmark>) {
    for item in items.as_array().into_iter().flatten() {
        let kind = item["type"].as_str().unwrap_or("");
        match kind {
            "group" => collect_bookmarks(&item["items"], out),
            "file" | "heading" | "block" => {
                if let Some(path) = item["path"].as_str() {
                    out.push(Bookmark::File {
                        path: path.to_string(),
                    });
                }
            }
            _ => {
                let title = ["title", "path", "query", "url"]
                    .iter()
                    .find_map(|k| item[*k].as_str())
                    .unwrap_or("")
                    .to_string();
                out.push(Bookmark::Other {
                    kind: kind.to_string(),
                    title,
                });
            }
        }
    }
}

fn file_times(path: &Path) -> (i64, i64) {
    let to_ms = |t: SystemTime| {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    };
    match fs::metadata(path) {
        Ok(meta) => {
            let modified = meta.modified().map(to_ms).unwrap_or(0);
            let created = meta.created().map(to_ms).unwrap_or(modified);
            (created.min(modified).max(0), modified)
        }
        Err(_) => (0, 0),
    }
}

/// Splits `name.ext` into (`name`, `ext`); dotfiles and extension-less names
/// return an empty extension.
pub fn split_ext(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i + 1..]),
        _ => (file_name, ""),
    }
}

fn strip_md(path: &str) -> &str {
    path.len()
        .checked_sub(3)
        .filter(|&i| {
            path.get(i..)
                .is_some_and(|ext| ext.eq_ignore_ascii_case(".md"))
        })
        .map_or(path, |i| &path[..i])
}

/// Normalizes a vault-relative folder given by the user or the Obsidian
/// config: `./Daily/` and `Daily` name the same folder.
fn normalize_folder(folder: &str) -> String {
    let mut f = folder.trim().trim_matches('/');
    while let Some(rest) = f.strip_prefix("./") {
        f = rest.trim_start_matches('/');
    }
    if f == "." {
        String::new()
    } else {
        f.to_string()
    }
}

fn strip_md_ci(lower: &str) -> &str {
    lower.strip_suffix(".md").unwrap_or(lower)
}

/// Resolves `.` and `..` segments; returns None when escaping the vault.
fn normalize_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault_with(files: &[&str]) -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "x").unwrap();
        }
        let v = Vault::scan(dir.path(), &[]).unwrap();
        (dir, v)
    }

    fn note_path(v: &Vault, t: Option<Target>) -> Option<&str> {
        match t? {
            Target::Note(i) => Some(&v.notes[i].rel_path),
            Target::Attachment(i) => Some(&v.attachments[i].rel_path),
        }
    }

    #[test]
    fn resolves_like_obsidian() {
        let (_d, v) = vault_with(&[
            "Index.md",
            "a/Index.md",
            "a/b/Deep.md",
            "a/img.png",
            "img.png",
            ".obsidian/app.json",
        ]);
        assert_eq!(v.notes.len(), 3);
        assert_eq!(v.attachments.len(), 2);
        let from_a = v.notes.iter().position(|n| n.rel_path == "a/Index.md");
        assert_eq!(note_path(&v, v.resolve("index", None)), Some("Index.md"));
        assert_eq!(
            note_path(&v, v.resolve("Index", from_a)),
            Some("a/Index.md")
        );
        assert_eq!(
            note_path(&v, v.resolve("b/Deep", None)),
            Some("a/b/Deep.md")
        );
        assert_eq!(
            note_path(&v, v.resolve("Deep.md", None)),
            Some("a/b/Deep.md")
        );
        assert_eq!(
            note_path(&v, v.resolve("img.png", from_a)),
            Some("a/img.png")
        );
        assert_eq!(
            note_path(&v, v.resolve("../img.png", from_a)),
            Some("img.png")
        );
        assert_eq!(v.resolve("missing", None), None);
    }

    #[test]
    fn daily_notes_detection() {
        let (_d, mut v) = vault_with(&["Daily/2024-01-05.md", "Other.md"]);
        v.load_daily_config(None, None).unwrap();
        assert_eq!(
            v.note_journal_date(&v.notes[0]),
            NaiveDate::from_ymd_opt(2024, 1, 5)
        );
        assert_eq!(v.note_journal_date(&v.notes[1]), None);
    }

    #[test]
    fn daily_notes_only_in_daily_folder() {
        let (_d, mut v) = vault_with(&["Daily/2024-01-05.md", "Meetings/2024-01-05.md"]);
        v.load_daily_config(None, Some("./daily/")).unwrap();
        assert_eq!(v.notes[0].rel_path, "Daily/2024-01-05.md");
        assert_eq!(
            v.note_journal_date(&v.notes[0]),
            NaiveDate::from_ymd_opt(2024, 1, 5)
        );
        assert_eq!(v.note_journal_date(&v.notes[1]), None);
    }

    #[test]
    fn exclude_accepts_dot_slash() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Archive")).unwrap();
        fs::write(dir.path().join("Archive/Old.md"), "x").unwrap();
        fs::write(dir.path().join("Keep.md"), "x").unwrap();
        let v = Vault::scan(dir.path(), &["./Archive/".to_string()]).unwrap();
        assert_eq!(v.notes.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_skipped() {
        let (d, _) = vault_with(&["Note.md"]);
        std::os::unix::fs::symlink("/nonexistent", d.path().join("broken.png")).unwrap();
        let v = Vault::scan(d.path(), &[]).unwrap();
        assert_eq!(v.notes.len(), 1);
        assert_eq!(v.warnings.len(), 1);
    }

    #[test]
    fn mixed_case_extension_resolves_by_path() {
        let (_d, v) = vault_with(&["F/Note.Md", "G/Note.md"]);
        assert_eq!(note_path(&v, v.resolve("F/Note", None)), Some("F/Note.Md"));
        assert_eq!(
            note_path(&v, v.resolve("F/Note.Md", None)),
            Some("F/Note.Md")
        );
    }

    #[test]
    fn file_is_not_a_vault() {
        let (d, _) = vault_with(&["Note.md"]);
        assert!(Vault::scan(&d.path().join("Note.md"), &[]).is_err());
    }
}
