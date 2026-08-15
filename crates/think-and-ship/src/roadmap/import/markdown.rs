//! Universal markdown roadmap parser.
//!
//! Handles the wildly different shapes real roadmaps take, adaptively:
//! - **Heading-structured** (`### Stage 1 — …` with checklist tasks under it):
//!   each work heading is a chunk; its nested list items become acceptance.
//! - **Section + checklist** (`## Pending` / `## Done` with `- [ ] item`s):
//!   the status headings set context; the checklist items are the chunks.
//! - **Pure checklist / TODO** (no headings): each top-level item is a chunk.
//!
//! Status is inferred from (in priority order) an explicit checkbox / emoji /
//! strikethrough on the item, then the enclosing status section, then defaults
//! to `Pending`. Structural headings (Overview, Architecture, Notes, …) and the
//! document title are ignored.

use std::collections::HashSet;

use super::{
    ImportedChunk, ImportedRoadmap, NoteSection, derive_id, headline, status_from_word, uniquify,
};
use crate::roadmap::domain::ChunkStatus;

/// Parse a markdown roadmap body into ordered native chunks (chunks only).
pub fn parse_markdown(input: &str) -> Vec<ImportedChunk> {
    parse_roadmap(input).chunks
}

/// Where the current body line is routed.
enum Sink {
    /// Intro prose before the first section → roadmap preamble.
    Preamble,
    /// A status section / work-heading region (lists → chunks/acceptance, prose
    /// → the current heading-chunk's notes).
    Body,
    /// An instructional section (Verify/Setup/…) — drop everything.
    Drop,
    /// A doc-level note section captured verbatim into `notes[idx].body`.
    Note(usize),
}

/// Parse a markdown roadmap into chunks **and** the surrounding hand-written
/// narrative (preamble + doc-level note sections + per-chunk prose), so an
/// `export` can reproduce it without loss.
pub fn parse_roadmap(input: &str) -> ImportedRoadmap {
    let mut chunks: Vec<ImportedChunk> = Vec::new();
    let mut notes: Vec<NoteSection> = Vec::new();
    let mut preamble = String::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut section: Option<ChunkStatus> = None;
    let mut priority: u32 = 0;
    let mut current: Option<usize> = None;
    let mut current_from_heading = false;
    let mut seen_first_heading = false;
    let mut in_code_fence = false;
    let mut sink = Sink::Preamble;

    for raw in input.lines() {
        let trimmed = raw.trim();

        // Fenced code: toggle; within a note, capture the fence verbatim.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            if let Sink::Note(idx) = sink {
                push_line(&mut notes[idx].body, raw);
            }
            continue;
        }
        if in_code_fence {
            if let Sink::Note(idx) = sink {
                push_line(&mut notes[idx].body, raw);
            }
            continue;
        }

        // ---- heading (ends any note / preamble / drop region) ----
        if let Some((level, htext)) = parse_heading(trimmed) {
            let cleaned = clean_inline(htext);

            if let Some(st) = section_heading_status(&cleaned) {
                section = Some(st);
                current = None;
                current_from_heading = false;
                seen_first_heading = true;
                sink = Sink::Body;
                continue;
            }
            // A priority band under a status section (`### critical`). Structure
            // the exporter writes, not work anybody planned — so it opens no
            // chunk and keeps the section's status, and the bullets under it
            // stay top-level chunks rather than becoming its acceptance list.
            if band_heading(&cleaned) {
                current = None;
                current_from_heading = false;
                seen_first_heading = true;
                sink = Sink::Body;
                continue;
            }
            // Document title (first top-level H1) → not a chunk; following prose
            // is the preamble.
            if !seen_first_heading && level == 1 {
                seen_first_heading = true;
                sink = Sink::Preamble;
                continue;
            }
            seen_first_heading = true;
            // Instructional section (Verify/Setup/Build/…) — drop it + its body.
            if instructional_section(&cleaned) {
                current = None;
                current_from_heading = false;
                sink = Sink::Drop;
                continue;
            }
            // Prose/note section (Research notes/Vision/Design/…) — keep verbatim.
            if note_section_heading(&cleaned) {
                current = None;
                current_from_heading = false;
                notes.push(NoteSection {
                    heading: cleaned,
                    body: String::new(),
                });
                sink = Sink::Note(notes.len() - 1);
                continue;
            }
            // A work heading → a chunk.
            let status = inline_marker_status(htext)
                .or(section)
                .unwrap_or(ChunkStatus::Pending);
            let id = uniquify(derive_id(&cleaned), &mut seen_ids);
            let (title, overflow) = headline(&cleaned);
            let mut chunk = ImportedChunk::new(id, title, status, priority);
            chunk.description = overflow;
            chunks.push(chunk);
            priority += 1;
            current = Some(chunks.len() - 1);
            current_from_heading = true;
            sink = Sink::Body;
            continue;
        }

        // ---- body line, by sink ----
        match sink {
            Sink::Note(idx) => {
                push_line(&mut notes[idx].body, raw);
                continue;
            }
            Sink::Drop => continue,
            Sink::Preamble => {
                // Prose before the first section accretes into the preamble; but
                // a list item with no preceding heading (a pure checklist) starts
                // the body — fall through to list handling.
                if parse_list_item(raw).is_none() {
                    if !trimmed.is_empty() {
                        push_line(&mut preamble, trimmed);
                    }
                    continue;
                }
                sink = Sink::Body;
            }
            Sink::Body => {}
        }
        if trimmed.is_empty() {
            continue;
        }

        // list item (checklist / bullet / numbered)
        if let Some(item) = parse_list_item(raw) {
            let nested = current_from_heading || (item.indent > 0 && current.is_some());
            if nested && let Some(idx) = current {
                let acc = clean_inline(&item.text);
                // `- name: …` is a field the export writes, not a criterion a
                // human wrote. Without this the round-trip turns the node's
                // label into an acceptance bullet and the chunk comes back
                // nameless.
                if let Some(n) = name_line(&acc) {
                    chunks[idx].name = n;
                    continue;
                }
                // Same rescue, opposite disposal: the exported blocker line is
                // recognised and dropped rather than reconstructed. See
                // [`is_blocker_line`] for why an import cannot honestly rebuild
                // a `BlockedBy` from it.
                if is_blocker_line(&acc) {
                    continue;
                }
                if !acc.is_empty() {
                    chunks[idx].acceptance.push(acc);
                }
                continue;
            }
            let full = clean_inline(&item.text);
            if full.is_empty() {
                continue;
            }
            let status = item
                .checkbox_status()
                .or_else(|| inline_marker_status(&item.text))
                .or(section)
                .unwrap_or(ChunkStatus::Pending);
            let id = uniquify(derive_id(&full), &mut seen_ids);
            let (title, overflow) = headline(&full);
            let mut chunk = ImportedChunk::new(id, title, status, priority);
            chunk.description = overflow;
            chunks.push(chunk);
            priority += 1;
            current = Some(chunks.len() - 1);
            current_from_heading = false;
            continue;
        }

        // standalone acceptance marker
        if let Some(acc) = acceptance_line(trimmed)
            && let Some(idx) = current
        {
            chunks[idx].acceptance.push(acc);
            continue;
        }

        // Prose under a work heading → that chunk's notes (verbatim narrative).
        if current_from_heading && let Some(idx) = current {
            push_line(&mut chunks[idx].notes, trimmed);
        }
    }

    preamble = preamble.trim().to_string();
    for n in &mut notes {
        n.body = n.body.trim_end().to_string();
    }
    for c in &mut chunks {
        c.notes = c.notes.trim_end().to_string();
    }
    ImportedRoadmap {
        preamble,
        notes,
        chunks,
    }
}

/// Append `line` to a verbatim buffer (newline-separated).
fn push_line(buf: &mut String, line: &str) {
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// An ATX heading: returns `(level, text)` for `#`..`######` lines.
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    if text.is_empty() {
        None
    } else {
        Some((level, text))
    }
}

/// A heading counts as a *status section* only when it's short and reads as a
/// status word (so `## Done` / `## Pending (priority order)` qualify, but
/// `## Add a done-state cache` does not).
fn section_heading_status(heading: &str) -> Option<ChunkStatus> {
    let core = heading
        .split('(')
        .next()
        .unwrap_or(heading)
        .trim()
        .trim_end_matches(':')
        .trim();
    if core.split_whitespace().count() > 4 {
        return None;
    }
    status_from_word(core)
}

/// Whether `heading` matches one of `words` (exact, or `word `-prefixed).
fn heading_in(heading: &str, words: &[&str]) -> bool {
    let h = heading.trim().to_ascii_lowercase();
    let h = h.trim_end_matches(':').trim();
    words
        .iter()
        .any(|w| h == *w || h.starts_with(&format!("{w} ")))
}

/// Instructional / boilerplate sections — dropped entirely (heading + body),
/// because they hold commands or meta, not roadmap work.
fn instructional_section(heading: &str) -> bool {
    const WORDS: &[&str] = &[
        "verify",
        "verification",
        "build",
        "setup",
        "set up",
        "install",
        "installation",
        "getting started",
        "get started",
        "quick start",
        "quickstart",
        "prerequisites",
        "requirements",
        "commands",
        "command",
        "workflow",
        "workflows",
        "running",
        "scripts",
        "environment",
        "tooling",
        "usage",
        "contributing",
        "license",
        "changelog",
        "faq",
        "glossary",
        "references",
        "reference",
        "links",
        "contents",
        "table of contents",
    ];
    heading_in(heading, WORDS)
}

/// Prose / narrative sections — not work items, but preserved verbatim as a
/// [`NoteSection`] so `export` can reproduce them (a bug hit twice in
/// production: export used to nuke these).
fn note_section_heading(heading: &str) -> bool {
    const WORDS: &[&str] = &[
        "overview",
        "architecture",
        "introduction",
        "intro",
        "notes",
        "note",
        "legend",
        "key",
        "about",
        "vision",
        "goals",
        "goal",
        "principles",
        "summary",
        "background",
        "design notes",
        "design",
        "research notes",
        "research",
        "what is",
        "why",
        "how it works",
        "status",
        "discovered",
        "discovered / backlog",
    ];
    heading_in(heading, WORDS)
}

/// Status from emoji / strikethrough / a trailing `(status)` parenthetical on a
/// raw item or heading text.
fn inline_marker_status(text: &str) -> Option<ChunkStatus> {
    if text.contains("~~") {
        return Some(ChunkStatus::Obsoleted);
    }
    for ch in text.chars() {
        match ch {
            '✅' | '✔' | '☑' | '🎉' => return Some(ChunkStatus::Done),
            '🚧' | '🏗' => return Some(ChunkStatus::InProgress),
            '❌' | '🚫' | '⛔' => return Some(ChunkStatus::Obsoleted),
            '🔜' | '⬜' | '◻' | '🔲' => return Some(ChunkStatus::Pending),
            _ => {}
        }
    }
    // Trailing/embedded `(done)`, `(wip)`, `(shipped)`, …
    if let Some(open) = text.rfind('(')
        && let Some(close) = text[open..].find(')')
    {
        let inner = &text[open + 1..open + close];
        if let Some(st) = status_from_word(inner) {
            return Some(st);
        }
    }
    None
}

/// `**Acceptance:** …` (or plain `Acceptance: …`) → the criterion text.
fn acceptance_line(line: &str) -> Option<String> {
    let l = line.trim_start_matches("- ").trim();
    let rest = l
        .strip_prefix("**Acceptance:**")
        .or_else(|| l.strip_prefix("Acceptance:"))?;
    let cleaned = clean_inline(rest.trim().trim_start_matches('✅').trim());
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Whether a heading is bare priority-band structure rather than a unit of
/// work. Matched against the band vocabulary itself, so a band renamed there
/// cannot quietly start importing as a chunk.
fn band_heading(heading: &str) -> bool {
    let h = heading.trim().to_ascii_lowercase();
    // One priority inside each band, asked of the same function the exporter
    // uses — never a hand-copied list of the five words.
    [0, 101, 201, 301, 401]
        .iter()
        .any(|p| crate::infra::coerce::priority_band(*p) == h)
}

/// `name: …` (or `**name:** …`) → the short node label the export wrote.
///
/// Only accepts a value inside the budget. A `name:` line that is really a
/// sentence is a human's acceptance criterion that happens to start with the
/// word, so it falls through to being read as one rather than being installed
/// as a label no node could wear.
fn name_line(line: &str) -> Option<String> {
    let l = line.trim();
    let rest = l
        .strip_prefix("**name:**")
        .or_else(|| l.strip_prefix("**Name:**"))
        .or_else(|| l.strip_prefix("name:"))
        .or_else(|| l.strip_prefix("Name:"))?;
    let cleaned = clean_inline(rest.trim());
    crate::roadmap::name::fits(&cleaned).then_some(cleaned)
}

/// Does this sub-bullet carry the blocker the export wrote (`blocked by: …`)?
///
/// Recognised so it can be DROPPED, which needs saying because dropping data is
/// normally the wrong answer. It is not reconstructed into a `BlockedBy`: that
/// type requires a `blocked_at`, the export does not write one, and an import
/// that invented a timestamp would make a plan claim to know how long something
/// has been stuck when it does not. Losing the blocker on a round-trip is what
/// already happened when the export omitted it entirely; the thing that must
/// not happen is the alternative, where the line survives as an acceptance
/// criterion no human ever wrote. Every sub-bullet the exporter emits that is
/// not rescued here suffers exactly that — `deps:` does today.
fn is_blocker_line(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    l.starts_with("blocked by:") || l.starts_with("**blocked by:**")
}

struct ListItem {
    indent: usize,
    checkbox: Option<char>,
    text: String,
}

impl ListItem {
    fn checkbox_status(&self) -> Option<ChunkStatus> {
        match self.checkbox {
            Some('x') | Some('X') => Some(ChunkStatus::Done),
            Some('~') | Some('/') => Some(ChunkStatus::InProgress),
            Some('-') => Some(ChunkStatus::Obsoleted),
            _ => None,
        }
    }
}

/// Parse a list item: `- text`, `* [x] text`, `+ text`, `1. text`, `2) text`,
/// with optional `[ ]`/`[x]`/`[~]`/`[-]`/`[/]` checkbox. Indentation (spaces,
/// tabs=4) is preserved so nesting can be detected.
fn parse_list_item(raw: &str) -> Option<ListItem> {
    let mut indent = 0usize;
    let mut rest = raw;
    for ch in raw.chars() {
        match ch {
            ' ' => indent += 1,
            '\t' => indent += 4,
            _ => break,
        }
        rest = &rest[ch.len_utf8()..];
    }

    let after_bullet = strip_bullet(rest)?;

    // Optional checkbox.
    let (checkbox, text) = if let Some(r) = after_bullet.strip_prefix('[')
        && r.len() >= 2
        && r.as_bytes()[1] == b']'
    {
        let inner = r.chars().next().unwrap_or(' ');
        let after = r[2..].trim_start();
        (Some(inner), after.to_string())
    } else {
        (None, after_bullet.trim_start().to_string())
    };

    if text.trim().is_empty() {
        return None;
    }
    Some(ListItem {
        indent,
        checkbox,
        text,
    })
}

/// Strip a leading bullet marker (`- `, `* `, `+ `, `1. `, `2) `) and return the
/// item text after it, or `None` if `rest` isn't a list item.
fn strip_bullet(rest: &str) -> Option<&str> {
    if let Some(r) = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
    {
        return Some(r);
    }
    // Numbered: digits then `.`/`)` then space.
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits < rest.len() {
        let after = &rest[digits..];
        return after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "));
    }
    None
}

const LEADING_GLYPHS: &[char] = &[
    '✅', '✔', '☑', '🚧', '🏗', '❌', '🚫', '⛔', '🔜', '⬜', '◻', '🔲', '🎉', '☐', '▪', '•', '→',
    '▸',
];

/// Strip markdown emphasis/code/links, strikethrough, leading status glyphs, and
/// a trailing provenance parenthetical (`(commit: …)`), yielding a clean title.
fn clean_inline(s: &str) -> String {
    let mut t = unwrap_links(s)
        .replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('`', "");
    // Drop any run of leading status glyphs / whitespace.
    while let Some(first) = t.chars().next() {
        if first.is_whitespace() || LEADING_GLYPHS.contains(&first) {
            t = t[first.len_utf8()..].to_string();
        } else {
            break;
        }
    }
    // Strip a trailing provenance parenthetical.
    for marker in [" (commit", " (commits", " (OBSOLETED", " (uncommitted"] {
        if let Some(idx) = t.find(marker) {
            t.truncate(idx);
        }
    }
    t.trim()
        .trim_end_matches([':', '—', '-'])
        .trim()
        .to_string()
}

/// Convert markdown links `[text](url)` → `text` (leaves other text intact).
fn unwrap_links(s: &str) -> String {
    if !s.contains("](") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'['
            && let Some(close) = s[i..].find("](")
            && let Some(paren_end) = s[i + close..].find(')')
        {
            out.push_str(&s[i + 1..i + close]);
            i += close + paren_end + 1;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_id<'a>(c: &'a [ImportedChunk], id: &str) -> &'a ImportedChunk {
        c.iter().find(|x| x.id == id).unwrap_or_else(|| {
            panic!(
                "missing {id} in {:?}",
                c.iter().map(|x| &x.id).collect::<Vec<_>>()
            )
        })
    }

    #[test]
    fn heading_structured_phases_with_task_acceptance() {
        let md = "\
# My Project Roadmap

## Done

### Stage 1 — Workspace (commit: abc123)

- [x] set up the crate
- [x] CI green

## Pending

#### Stage 2 — Auth

- [ ] login flow

**Acceptance:** users can log in
";
        let c = parse_markdown(md);
        assert_eq!(by_id(&c, "stage-1").status, ChunkStatus::Done);
        assert_eq!(by_id(&c, "stage-1").title, "Stage 1 — Workspace");
        assert_eq!(
            by_id(&c, "stage-1").acceptance,
            vec!["set up the crate", "CI green"]
        );
        assert_eq!(by_id(&c, "stage-2").status, ChunkStatus::Pending);
        assert!(
            by_id(&c, "stage-2")
                .acceptance
                .iter()
                .any(|a| a.contains("log in"))
        );
    }

    #[test]
    fn section_plus_checklist_each_item_is_a_chunk() {
        let md = "\
# Roadmap

## Done
- [x] **Phase A — Bootstrap** — the first thing
- [x] Phase B — Pipeline

## Pending (priority order)
- [ ] Phase C — Polish
- [ ] Phase D — Launch

## Backlog
- some loose idea
";
        let c = parse_markdown(md);
        assert_eq!(c.len(), 5);
        assert_eq!(by_id(&c, "phase-a").status, ChunkStatus::Done);
        assert_eq!(
            by_id(&c, "phase-a").title,
            "Phase A — Bootstrap — the first thing"
        );
        assert_eq!(by_id(&c, "phase-c").status, ChunkStatus::Pending);
        assert_eq!(by_id(&c, "some-loose-idea").status, ChunkStatus::Backlog);
    }

    #[test]
    fn pure_checklist_no_headings() {
        let md = "\
- [x] Buy milk
- [ ] Walk the dog
- [~] Write the report
- [ ] Ship it 🎉
";
        let c = parse_markdown(md);
        assert_eq!(c.len(), 4);
        assert_eq!(by_id(&c, "buy-milk").status, ChunkStatus::Done);
        assert_eq!(by_id(&c, "walk-the-dog").status, ChunkStatus::Pending);
        assert_eq!(
            by_id(&c, "write-the-report").status,
            ChunkStatus::InProgress
        );
        // emoji marker wins for the last one
        assert_eq!(by_id(&c, "ship-it").status, ChunkStatus::Done);
    }

    #[test]
    fn nested_checklist_children_become_acceptance() {
        let md = "\
## Pending
- [ ] Phase X — Big feature
  - [ ] sub-step one
  - [ ] sub-step two
- [ ] Phase Y — Another
";
        let c = parse_markdown(md);
        assert_eq!(c.len(), 2);
        assert_eq!(
            by_id(&c, "phase-x").acceptance,
            vec!["sub-step one", "sub-step two"]
        );
    }

    #[test]
    fn strikethrough_and_emoji_and_structural_headings() {
        let md = "\
# Plan

## Architecture
This is prose, not a chunk. It even has a list:
- not a chunk because... actually it is under a structural heading

### ~~Stage 9 — Cancelled idea~~

### ✅ Stage 10 — Shipped thing

## Notes
- a note
";
        let c = parse_markdown(md);
        // 'Architecture' is structural → its bullet still becomes a chunk only
        // if treated as top-level; structural sets current=None so the bullet is
        // a chunk under no section (Pending). Stages 9/10 are headings.
        assert_eq!(by_id(&c, "stage-9").status, ChunkStatus::Obsoleted);
        assert_eq!(by_id(&c, "stage-10").status, ChunkStatus::Done);
        assert_eq!(by_id(&c, "stage-10").title, "Stage 10 — Shipped thing");
    }

    #[test]
    fn numbered_lists_and_duplicate_titles() {
        let md = "\
## Todo
1. First task
2. Second task
3. First task
";
        let c = parse_markdown(md);
        assert_eq!(c.len(), 3);
        assert!(c.iter().any(|x| x.id == "first-task"));
        assert!(c.iter().any(|x| x.id == "first-task-2")); // uniquified
        assert!(c.iter().all(|x| x.status == ChunkStatus::Pending));
    }

    #[test]
    fn slugified_arbitrary_titles() {
        let md = "## Backlog\n- Add a dark-mode toggle to the settings screen\n";
        let c = parse_markdown(md);
        assert_eq!(
            super::super::slugify("Add a dark-mode toggle to the settings screen"),
            c[0].id
        );
        assert_eq!(c[0].status, ChunkStatus::Backlog);
    }

    #[test]
    fn captures_preamble_chunk_notes_and_note_sections() {
        let md = "\
# My Roadmap

A one-line intro that explains the project.

## Pending

### Stage 1 — Build the thing
Design narrative: we chose approach A over B because it scales.
More narrative on the second line.
- [ ] the actual task

## Verify
```
just test
```

## Research notes

We benchmarked X vs Y and X won by 2x.
- a loose research bullet
";
        let r = parse_roadmap(md);
        assert_eq!(r.preamble, "A one-line intro that explains the project.");
        let p1 = r.chunks.iter().find(|c| c.id == "stage-1").unwrap();
        assert!(p1.notes.contains("approach A over B"));
        assert!(p1.notes.contains("second line"));
        assert_eq!(p1.acceptance, vec!["the actual task"]);
        // `## Verify` dropped; `## Research notes` preserved verbatim.
        assert_eq!(r.notes.len(), 1);
        assert_eq!(r.notes[0].heading, "Research notes");
        assert!(r.notes[0].body.contains("X won by 2x"));
        assert!(r.notes[0].body.contains("- a loose research bullet"));
    }

    #[test]
    fn paragraph_bullets_become_title_plus_description_not_a_paragraph_title() {
        // The ryg bloat: a backlog bullet written as a full paragraph. It must
        // become a concise title + a description, not a paragraph-length title.
        let md = "\
## Backlog
- Imported charts always feel one tier too hard. This is a follow-up to milestone 191 \
where the difficulty curve was tuned but the import path was never recalibrated to match.
";
        let c = parse_markdown(md);
        assert_eq!(c.len(), 1);
        assert!(
            c[0].title.chars().count() <= super::super::MAX_TITLE_LEN,
            "title too long: {:?}",
            c[0].title
        );
        assert_eq!(c[0].title, "Imported charts always feel one tier too hard");
        assert!(c[0].description.contains("follow-up to milestone 191"));
        assert_eq!(c[0].status, ChunkStatus::Backlog);
    }

    #[test]
    fn instructional_sections_and_code_blocks_are_dropped() {
        // Mirrors a real ROADMAP.md head: a `## Verify` section with a fenced
        // command block and a numbered workflow — none of it is real work.
        let md = "\
# My Game — Roadmap

## Verify

```bash
cmake --build build && ./run
```

MCP workflow:
1. game_build — compile
2. game_screenshot — capture

## Pending (priority order)
- [ ] v2.0 — The real next thing
";
        let c = parse_markdown(md);
        // Only the one genuine pending item survives.
        assert_eq!(
            c.len(),
            1,
            "got: {:?}",
            c.iter().map(|x| &x.id).collect::<Vec<_>>()
        );
        assert_eq!(c[0].title, "v2.0 — The real next thing");
        assert_eq!(c[0].status, ChunkStatus::Pending);
        assert!(!c.iter().any(|x| x.id == "verify"));
        assert!(!c.iter().any(|x| x.title.contains("game_build")));
    }

    /// The exporter writes the blocker as a sub-bullet, and every sub-bullet it
    /// writes that is not rescued here comes back as an acceptance criterion no
    /// human wrote. This is the rescue, and the assertion is about what does NOT
    /// appear: the line is dropped, not reconstructed, because rebuilding a
    /// `BlockedBy` would mean inventing the `blocked_at` the export never wrote.
    #[test]
    fn an_exported_blocker_line_does_not_come_back_as_an_acceptance_criterion() {
        let md = "\
# Roadmap

## Pending

- [ ] **Dome slit lags near the meridian** (45) — the flip already costs the most time
  - name: Dome slit lag
  - blocked by: awaiting_human — somebody has to stand in the dome and watch it happen (check:mirror-recoating-audit)
  - acceptance: the lag is measured on both sides of the meridian
";
        let c = parse_markdown(md);
        let chunk = &c[0];
        assert_eq!(chunk.name, "Dome slit lag", "the name is still rescued");
        assert_eq!(
            chunk.acceptance,
            vec!["acceptance: the lag is measured on both sides of the meridian"],
            "the blocker line must be dropped rather than filed as a criterion: {:?}",
            chunk.acceptance
        );
        assert!(
            !chunk.acceptance.iter().any(|a| a.contains("blocked by")),
            "no acceptance criterion may be fabricated from the blocker line"
        );
    }
}
