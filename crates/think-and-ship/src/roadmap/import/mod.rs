//! Roadmap importers — turn an existing roadmap (in whatever format a project
//! already keeps) into native [`ImportedChunk`]s that map onto
//! [`crate::roadmap::engine::RoadmapEngine::add_chunk`].
//!
//! Three layers:
//! - [`markdown`] — a *universal* markdown parser: headings of any level,
//!   checklists, plain lists, status sections, checkbox/strikethrough/emoji
//!   status, and arbitrary level-of-detail.
//! - [`yaml`] — a phases/tasks YAML schema (magistr's canonical format and
//!   anything shaped like it).
//! - this module — source **discovery** (find every roadmap file in a project),
//!   format **dispatch** (by extension), and **dedup** across sources.
//!
//! The shared status vocabulary ([`status_from_word`]) is deliberately broad so
//! the parsers agree on what "done" / "in progress" / "backlog" look like
//! across wildly different conventions.

pub mod markdown;
pub mod yaml;

use std::path::{Path, PathBuf};

use crate::roadmap::domain::ChunkStatus;
pub use crate::roadmap::domain::NoteSection;

/// A chunk parsed out of some roadmap source, ready to feed to `add_chunk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedChunk {
    pub id: String,
    pub title: String,
    /// The short node label, when the source carried one. Empty means the
    /// source had none and the engine seeds it from the id — which is every
    /// roadmap written before this field existed, and every format that has no
    /// place to put it.
    pub name: String,
    pub status: ChunkStatus,
    /// Document/source order — lower sorts earlier (maps to engine priority).
    pub priority: u32,
    pub description: String,
    /// Hand-written prose body carried verbatim from the source.
    pub notes: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
}

impl ImportedChunk {
    /// Construct with empty description/notes/acceptance/deps (leaf items).
    pub fn new(id: String, title: String, status: ChunkStatus, priority: u32) -> Self {
        Self {
            id,
            title,
            name: String::new(),
            status,
            priority,
            description: String::new(),
            notes: String::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
        }
    }

    /// The node label this chunk should land with: what the source carried when
    /// it is usable, and a name seeded from the id otherwise.
    ///
    /// One rule in one place, because both import paths (`seed_from_import` and
    /// `merge_from_import`) build a `Chunk` by hand and would otherwise be free
    /// to disagree about what a missing name means.
    #[must_use]
    pub fn resolved_name(&self) -> String {
        if crate::roadmap::name::fits(&self.name) {
            self.name.trim().to_string()
        } else {
            crate::roadmap::name::derive(&self.id)
        }
    }
}

/// A whole roadmap parsed from a source: its chunks plus the surrounding
/// hand-written narrative (intro `preamble` + doc-level `notes` sections) that a
/// lossless `export` needs to reproduce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedRoadmap {
    pub preamble: String,
    pub notes: Vec<NoteSection>,
    pub chunks: Vec<ImportedChunk>,
}

/// Map a free-text status / section word to a [`ChunkStatus`]. Tolerant of the
/// many conventions roadmaps use. Returns `None` when the word carries no
/// recognizable status signal (so callers can fall back to context).
pub fn status_from_word(raw: &str) -> Option<ChunkStatus> {
    let w = raw.trim().to_ascii_lowercase();
    // Strip surrounding markdown/punctuation/emoji noise ("**done**", "✅ done",
    // "done:", "(wip)"), then any whitespace the strip exposed.
    let w = w
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != ' ' && c != '-')
        .trim();
    let has = |needles: &[&str]| needles.iter().any(|n| w == *n || w.starts_with(n));
    if has(&[
        "done",
        "complete",
        "completed",
        "shipped",
        "finished",
        "released",
        "closed",
    ]) {
        Some(ChunkStatus::Done)
    } else if has(&[
        "in progress",
        "in-progress",
        "inprogress",
        "in flight",
        "in-flight",
        "inflight",
        "in review",
        "reviewing",
        "wip",
        "active",
        "doing",
        "current",
        "ongoing",
        "started",
    ]) {
        Some(ChunkStatus::InProgress)
    } else if has(&["blocked", "waiting", "on hold", "on-hold", "stalled"]) {
        Some(ChunkStatus::Blocked)
    } else if has(&[
        "backlog",
        "icebox",
        "later",
        "someday",
        "ideas",
        "idea",
        "discovered",
        "future",
        "maybe",
        "wishlist",
    ]) {
        Some(ChunkStatus::Backlog)
    } else if has(&[
        "obsolete",
        "obsoleted",
        "abandoned",
        "cancelled",
        "canceled",
        "wontfix",
        "deprecated",
        "dropped",
        "rejected",
    ]) {
        Some(ChunkStatus::Obsoleted)
    } else if has(&[
        "pending", "todo", "to do", "to-do", "planned", "next", "up next", "next up", "upcoming",
        "ready", "open", "queued", "now",
    ]) {
        Some(ChunkStatus::Pending)
    } else {
        None
    }
}

/// Parse one roadmap file into a full [`ImportedRoadmap`] (chunks + narrative),
/// dispatching by extension. `.yml`/`.yaml` → [`yaml`]; everything else → [`markdown`].
pub fn parse_file_full(path: &Path) -> std::io::Result<ImportedRoadmap> {
    let text = std::fs::read_to_string(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    Ok(match ext.as_str() {
        "yml" | "yaml" => yaml::parse_roadmap(&text),
        _ => markdown::parse_roadmap(&text),
    })
}

/// Chunks-only convenience over [`parse_file_full`].
pub fn parse_file(path: &Path) -> std::io::Result<Vec<ImportedChunk>> {
    Ok(parse_file_full(path)?.chunks)
}

/// Roadmap source files this importer recognizes, in **authority order** — when
/// the same chunk id appears in more than one source, the earlier (more
/// canonical) source wins. YAML stores (a project's structured source of truth)
/// rank above generated markdown views.
pub fn discover_sources(root: &Path) -> Vec<PathBuf> {
    // Fixed candidates, highest authority first.
    const CANDIDATES: &[&str] = &[
        ".magistr/roadmap.yaml",
        ".magistr/roadmap.yml",
        ".magistr-roadmap.yml",
        ".magistr-roadmap.yaml",
        "roadmap.yaml",
        "roadmap.yml",
        ".roadmap.yml",
        ".roadmap.yaml",
        "ROADMAP.md",
        "Roadmap.md",
        "roadmap.md",
        "docs/ROADMAP.md",
        "docs/roadmap.md",
        ".roadmap/active.md",
        "ROADMAP.markdown",
    ];
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for rel in CANDIDATES {
        let p = root.join(rel);
        if p.is_file()
            && let Ok(canon) = p.canonicalize()
            && seen.insert(canon)
        {
            found.push(p);
        }
    }
    // Any other `*.md` under a `.roadmap/` directory.
    if let Ok(rd) = std::fs::read_dir(root.join(".roadmap")) {
        let mut extra: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().and_then(|e| e.to_str()) == Some("md")
                    && p.canonicalize().is_ok_and(|c| seen.insert(c))
            })
            .collect();
        extra.sort();
        found.extend(extra);
    }
    found
}

/// Parse every discovered source under `root` and merge into one
/// [`ImportedRoadmap`]: chunks **deduped by id** (first/most-authoritative
/// source wins, priorities renumbered), the first non-empty `preamble`, and all
/// `notes` sections (deduped by heading).
pub fn import_project_full(root: &Path) -> ImportedRoadmap {
    let mut merged: ImportedRoadmap = ImportedRoadmap::default();
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_notes = std::collections::HashSet::new();
    for src in discover_sources(root) {
        let Ok(parsed) = parse_file_full(&src) else {
            continue;
        };
        if merged.preamble.is_empty() && !parsed.preamble.is_empty() {
            merged.preamble = parsed.preamble;
        }
        for n in parsed.notes {
            if seen_notes.insert(n.heading.to_ascii_lowercase()) {
                merged.notes.push(n);
            }
        }
        for c in parsed.chunks {
            if seen_ids.insert(c.id.clone()) {
                merged.chunks.push(c);
            }
        }
    }
    for (i, c) in merged.chunks.iter_mut().enumerate() {
        c.priority = i as u32;
    }
    merged
}

/// Chunks-only convenience over [`import_project_full`].
pub fn import_project(root: &Path) -> Vec<ImportedChunk> {
    import_project_full(root).chunks
}

// ---- shared id/title helpers used by both parsers ----

/// Max display length (in chars) of a chunk title before the overflow is split
/// off into the chunk's `description`. Real roadmaps routinely write a whole
/// paragraph as a bullet; without this cap the import turns each into a
/// paragraph-length title (measured on a real roadmap: 172 of 294 chunks had paragraph
/// titles, which is what made `roadmap_status` exceed the MCP output limit).
pub(crate) const MAX_TITLE_LEN: usize = 90;

/// Don't cut a title shorter than this many bytes when hunting for a clause
/// boundary — a too-eager break produces a stub like "Phase".
const MIN_TITLE_BYTES: usize = 24;

/// Split a possibly-long item text into a concise headline `title` and the
/// `overflow` that becomes the chunk's `description`. Short text passes through
/// unchanged (overflow empty). Long text is cut at the last clause boundary
/// (`. ` `; ` ` — ` `: ` `, `) within budget, else at the last word boundary
/// with an ellipsis. The two halves are complementary, so `export` reproduces
/// the original as `**title** — description` with no loss of content.
pub(crate) fn headline(text: &str) -> (String, String) {
    let text = text.trim();
    if text.chars().count() <= MAX_TITLE_LEN {
        return (text.to_string(), String::new());
    }
    let budget = text
        .char_indices()
        .nth(MAX_TITLE_LEN)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let window = &text[..budget];
    for marker in [". ", "; ", " — ", ": ", ", "] {
        if let Some(pos) = window.rfind(marker)
            && pos >= MIN_TITLE_BYTES
        {
            let title = text[..pos].trim().to_string();
            let rest = text[pos + marker.len()..].trim().to_string();
            return (title, rest);
        }
    }
    // No clause boundary — cut at the last word break and mark the elision.
    let pos = window
        .rfind(' ')
        .filter(|p| *p >= MIN_TITLE_BYTES)
        .unwrap_or(budget);
    let title = format!("{}…", text[..pos].trim());
    let rest = text[pos..].trim().to_string();
    (title, rest)
}

/// Slug-ify a title into a stable id. Phase-like titles (`phase 26e — …`,
/// `Milestone 3: …` — kind matching is case-insensitive) keep their short
/// token (`phase-26e`); anything else is a kebab-cased prefix of the title.
pub(crate) fn derive_id(title: &str) -> String {
    let t = title.trim();
    const KINDS: &[&str] = &[
        "phase",
        "milestone",
        "sprint",
        "epic",
        "stage",
        "step",
        "story",
        "task",
        "release",
        "version",
    ];
    let lower = t.to_ascii_lowercase();
    for kind in KINDS {
        if let Some(rest) = lower.strip_prefix(kind)
            && rest.starts_with([' ', '-', ':'])
        {
            let token: String = rest
                .trim_start_matches([' ', '-', ':'])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
                .collect();
            let token = token.trim_end_matches('.');
            if !token.is_empty() {
                return format!("{kind}-{token}");
            }
        }
    }
    slugify(t)
}

/// Lowercase, hyphen-separated slug of up to 7 words; punctuation dropped.
pub(crate) fn slugify(s: &str) -> String {
    let words: Vec<String> = s
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(7)
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() {
        "item".to_string()
    } else {
        words.join("-")
    }
}

/// Ensure `id` is unique within `seen`, appending `-2`, `-3`, … on collision.
/// Inserts the chosen id into `seen` and returns it.
pub(crate) fn uniquify(id: String, seen: &mut std::collections::HashSet<String>) -> String {
    if seen.insert(id.clone()) {
        return id;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{id}-{n}");
        if seen.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_vocabulary_is_broad() {
        for w in ["Done", "✅ complete", "Shipped", "RELEASED"] {
            assert_eq!(status_from_word(w), Some(ChunkStatus::Done), "{w}");
        }
        for w in ["In Progress", "WIP", "doing", "active"] {
            assert_eq!(status_from_word(w), Some(ChunkStatus::InProgress), "{w}");
        }
        for w in ["Backlog", "Icebox", "Someday / Maybe", "Ideas"] {
            assert_eq!(status_from_word(w), Some(ChunkStatus::Backlog), "{w}");
        }
        for w in ["TODO", "Pending (priority order)", "Planned", "Up next"] {
            assert_eq!(status_from_word(w), Some(ChunkStatus::Pending), "{w}");
        }
        assert_eq!(status_from_word("Architecture"), None);
        assert_eq!(status_from_word("Overview"), None);
    }

    #[test]
    fn derive_id_keeps_phase_tokens_else_slugs() {
        assert_eq!(derive_id("phase 26e — Ship the importer"), "phase-26e");
        assert_eq!(derive_id("Milestone 3: Ship it"), "milestone-3");
        // "v1.2" is not one of the phase-like kinds → falls through to a slug.
        assert_eq!(derive_id("v1.2 — Hardening"), "v1-2-hardening");
    }

    #[test]
    fn slugify_truncates_and_cleans() {
        // Capped at 7 words (drops "now please").
        assert_eq!(
            slugify("Add a Dark Mode toggle to settings now please"),
            "add-a-dark-mode-toggle-to-settings"
        );
        assert_eq!(slugify("   "), "item");
    }

    #[test]
    fn headline_passes_short_titles_through() {
        let (t, d) = headline("Stage 197 — Two-artifact release pipeline");
        assert_eq!(t, "Stage 197 — Two-artifact release pipeline");
        assert!(d.is_empty());
    }

    #[test]
    fn headline_splits_long_text_at_a_clause_boundary() {
        let text = "Imported charts always feel one tier too hard. Follow-up to milestone 191 \
            where we tuned the difficulty curve but never recalibrated the import path.";
        let (title, desc) = headline(text);
        assert!(title.chars().count() <= MAX_TITLE_LEN, "title: {title:?}");
        assert_eq!(title, "Imported charts always feel one tier too hard");
        assert!(desc.starts_with("Follow-up to milestone 191"));
        // Complementary: no content vanished.
        assert!(desc.contains("recalibrated the import path"));
    }

    #[test]
    fn headline_hard_truncates_with_ellipsis_when_no_boundary() {
        let text = "a ".repeat(80); // 160 chars, no clause markers
        let (title, desc) = headline(&text);
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= MAX_TITLE_LEN + 1);
        assert!(!desc.is_empty());
    }

    #[test]
    fn uniquify_disambiguates() {
        let mut seen = std::collections::HashSet::new();
        assert_eq!(uniquify("x".into(), &mut seen), "x");
        assert_eq!(uniquify("x".into(), &mut seen), "x-2");
        assert_eq!(uniquify("x".into(), &mut seen), "x-3");
    }
}
