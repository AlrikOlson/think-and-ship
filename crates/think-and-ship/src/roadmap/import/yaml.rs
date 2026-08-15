//! YAML roadmap parser — a `phases:`/`tasks:` schema (magistr's canonical
//! format, and anything shaped like it). Tolerant of alternate key names and
//! both string and `{text, checked}` task shapes.

use std::collections::HashSet;

use serde::Deserialize;

use super::{ImportedChunk, ImportedRoadmap, NoteSection, derive_id, status_from_word, uniquify};
use crate::roadmap::domain::ChunkStatus;

#[derive(Deserialize, Default)]
struct YamlRoadmap {
    #[serde(default)]
    phases: Vec<YamlPhase>,
    #[serde(default)]
    chunks: Vec<YamlPhase>,
    #[serde(default)]
    items: Vec<YamlPhase>,
    // Document-level narrative (magistr stores these) preserved as notes.
    #[serde(default)]
    preamble: String,
    #[serde(default)]
    vision: String,
    #[serde(default)]
    epilogue: String,
    #[serde(default)]
    principles: Vec<String>,
}

#[derive(Deserialize, Default)]
struct YamlPhase {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    tagline: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    problem: String,
    #[serde(default)]
    solution: String,
    #[serde(default)]
    description: String,
    #[serde(default, alias = "dependsOn", alias = "depends_on")]
    deps: Vec<String>,
    #[serde(default)]
    tasks: Vec<YamlTask>,
    #[serde(default)]
    acceptance: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum YamlTask {
    Text(String),
    Obj {
        #[serde(default)]
        text: String,
        #[serde(default)]
        title: String,
    },
}

impl YamlTask {
    fn into_text(self) -> Option<String> {
        let s = match self {
            YamlTask::Text(s) => s,
            YamlTask::Obj { text, title } => {
                if !text.is_empty() {
                    text
                } else {
                    title
                }
            }
        };
        let s = s.trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

/// Chunks-only convenience over [`parse_roadmap`].
pub fn parse_yaml(text: &str) -> Vec<ImportedChunk> {
    parse_roadmap(text).chunks
}

/// Parse a YAML roadmap into chunks + narrative (preamble, and Vision /
/// Principles / Epilogue → note sections). Empty on a parse error.
pub fn parse_roadmap(text: &str) -> ImportedRoadmap {
    let root: YamlRoadmap = serde_yaml::from_str(text).unwrap_or_default();

    let mut notes = Vec::new();
    let mut candidates: Vec<(&str, String)> = vec![("Vision", root.vision.clone())];
    if !root.principles.is_empty() {
        candidates.push((
            "Principles",
            root.principles
                .iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    candidates.push(("Epilogue", root.epilogue.clone()));
    for (heading, body) in candidates {
        if !body.trim().is_empty() {
            notes.push(NoteSection {
                heading: heading.to_string(),
                body: body.trim().to_string(),
            });
        }
    }

    let mut phases = root.phases;
    if phases.is_empty() {
        phases = root.chunks;
    }
    if phases.is_empty() {
        phases = root.items;
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, p) in phases.into_iter().enumerate() {
        let title = first_nonempty([&p.name, &p.title, &p.id]);
        if title.is_empty() {
            continue;
        }
        let id_src = if !p.id.is_empty() {
            p.id.clone()
        } else {
            title.clone()
        };
        let id = uniquify(normalize_id(&id_src), &mut seen);
        let status = status_from_word(&p.status).unwrap_or(ChunkStatus::Pending);
        let description = join_nonempty([&p.tagline, &p.problem, &p.solution, &p.description]);
        let deps = p.deps.iter().map(|d| normalize_id(d)).collect();
        let mut acceptance = p.acceptance;
        for t in p.tasks {
            if let Some(s) = t.into_text() {
                acceptance.push(s);
            }
        }
        out.push(ImportedChunk {
            id,
            title,
            // Left for the engine to seed. This schema's `name` key is a TITLE
            // synonym (see `first_nonempty` above), so reading it as a node
            // label would install the sentence this field exists to escape.
            name: String::new(),
            status,
            priority: i as u32,
            description,
            notes: String::new(),
            acceptance,
            deps,
        });
    }
    ImportedRoadmap {
        preamble: root.preamble.trim().to_string(),
        notes,
        chunks: out,
    }
}

fn first_nonempty<const N: usize>(candidates: [&str; N]) -> String {
    candidates
        .into_iter()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

fn join_nonempty<const N: usize>(parts: [&str; N]) -> String {
    parts
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" — ")
}

/// Normalize an explicit id (`OV1`, `E0`, `Stage 3`) into a stable slug. Short
/// alnum tokens are just lowercased (`ov1`); anything with spaces is slugged.
fn normalize_id(raw: &str) -> String {
    let t = raw.trim();
    if t.contains(' ') {
        return derive_id(t);
    }
    let s: String = t
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "item".to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGISTR_LIKE: &str = "\
title: Magistr — Roadmap
preamble: |
  Some intro text.
phases:
- id: OV1
  name: Navigation Consolidation
  tagline: Kill tabs, reduce to 4 views
  status: done
  problem: Dual redundant navigation
  solution: Eliminate tab bar
  tasks:
  - text: Reduce ViewId type
    checked: true
  - text: Delete ViewTabBar.svelte
    checked: true
- id: E1
  name: Domain Model Extraction
  status: pending
  depends_on:
  - OV1
  tasks:
  - Define crate boundary
";

    fn by_id<'a>(c: &'a [ImportedChunk], id: &str) -> &'a ImportedChunk {
        c.iter().find(|x| x.id == id).expect("present")
    }

    #[test]
    fn parses_magistr_style_phases() {
        let c = parse_yaml(MAGISTR_LIKE);
        assert_eq!(c.len(), 2);
        let ov1 = by_id(&c, "ov1");
        assert_eq!(ov1.title, "Navigation Consolidation");
        assert_eq!(ov1.status, ChunkStatus::Done);
        assert!(ov1.description.contains("Kill tabs"));
        assert!(ov1.description.contains("Eliminate tab bar"));
        assert_eq!(
            ov1.acceptance,
            vec!["Reduce ViewId type", "Delete ViewTabBar.svelte"]
        );
    }

    #[test]
    fn maps_depends_on_to_normalized_deps() {
        let c = parse_yaml(MAGISTR_LIKE);
        let e1 = by_id(&c, "e1");
        assert_eq!(e1.status, ChunkStatus::Pending);
        assert_eq!(e1.deps, vec!["ov1"]); // OV1 → ov1, matching the chunk id
        assert_eq!(e1.acceptance, vec!["Define crate boundary"]); // bare-string task
    }

    #[test]
    fn empty_or_garbage_yaml_yields_nothing() {
        assert!(parse_yaml("").is_empty());
        assert!(parse_yaml("not: a roadmap\nrandom: 5").is_empty());
    }
}
