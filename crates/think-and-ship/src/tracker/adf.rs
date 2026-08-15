//! Atlassian Document Format — the body language of the Jira lane.
//!
//! # Why ADF and not wiki markup
//!
//! Jira Cloud offers two ways to write a description: REST v3 with an ADF
//! document, or REST v2 with wiki markup. The initial design assumed ADF, but
//! v2 + wiki markup was a live alternative that would delete this whole
//! module. It was decided on evidence, and the evidence did not land where
//! the initial assumption expected it to.
//!
//! The risk written down for wiki markup — that REST v2 is on a removal
//! roadmap — is not real. The deprecation everyone cites, CHANGE-2046, is
//! endpoint-level, and its paths are `/rest/api/{2|3|latest}/search` and
//! `/rest/api/{2|3|latest}/expression/eval`: it hits v2 and v3 identically.
//! Atlassian's own answer is that v3 is the ADF-aware counterpart of v2, not
//! a replacement that obsoletes it. On the *version* axis the two options are
//! symmetric and the question does not discriminate.
//!
//! It discriminates on the *format* axis, which is where the alternative
//! actually lives — the choice is "emit wiki markup", not "call v2":
//!
//! 1. Atlassian has said wiki markup support will be phased out and
//!    eventually removed in favour of markdown. The version is safe; the
//!    format is the thing with a stated end.
//! 2. ADF is what Jira Cloud *stores*. Wiki markup is converted on the way in
//!    and re-derived on the way out, so a round trip crosses two conversions.
//!    [`crate::tracker::project`]'s echo fence compares what came back with
//!    the hash of what we wrote, and the margin there is one byte: Linear
//!    stripping a single trailing newline on save made `content_hash` differ
//!    on all 46 projected items. A twice-converted substrate cannot carry
//!    that fence.
//! 3. The saving was illusory anyway. There is no official wiki-markup/ADF
//!    converter and no public conversion endpoint (JRACLOUD-77436 is still
//!    open), so emitting wiki markup does not delete translation work — it
//!    means owning an unofficial dialect of a format slated for removal, with
//!    nothing to be correct against.
//!
//! # The footer is visible here, and that is a property of Jira
//!
//! [`crate::tracker::project`] writes its provenance block as an HTML comment
//! precisely so it renders as nothing in every tracker that speaks Markdown.
//! **ADF has no comment node and no hidden-content node**, and neither does
//! wiki markup — so on Jira the block is visible whichever option had won.
//! It goes in a `codeBlock`, whose text is carried verbatim rather than
//! reflowed and re-marked-up the way a paragraph's is. Keeping those bytes
//! exact is what lets the inbound side parse it back out.
//!
//! # Scope: our own bodies, not the world's markdown
//!
//! This is deliberately not a general markdown parser. The input is the body
//! [`crate::tracker::project::to_work_item`] authors, whose whole language is
//! headings, bullet items, checklist items, paragraphs and the footer. The
//! renderer is TOTAL: anything it does not recognize becomes a paragraph of
//! plain text, so unknown syntax is passed through visibly rather than
//! dropped silently.

use serde_json::{Value, json};

/// The `version` every ADF document must carry. ADF is versioned as a whole;
/// `1` is the only value Jira Cloud accepts today.
pub const ADF_VERSION: u64 = 1;

/// Mirrors [`crate::tracker::project`]'s footer delimiters. Duplicated as a
/// prefix test rather than imported, because the renderer's job is to
/// recognize a footer in a body it did not build — including one read back
/// from Jira.
const FOOTER_PREFIX: &str = "<!-- think-and-ship:";

/// Render one of our canonical Markdown bodies as an ADF document.
///
/// Pure and total: no network, no token, no panics, and no input rejected.
/// The empty body renders as an empty document, which is what Jira wants for
/// "this issue has no description" — `content: []` rather than a null field.
#[must_use]
pub fn render_body(markdown: &str) -> Value {
    let mut content: Vec<Value> = Vec::new();
    let mut list_buffer: Vec<String> = Vec::new();
    let mut para_buffer: Vec<String> = Vec::new();

    // A list and a paragraph are both multi-line constructs, so each is
    // flushed when something that cannot continue it arrives. Doing it here
    // rather than with a lookahead keeps the walk single-pass and total.
    for line in markdown.lines() {
        let trimmed = line.trim_end();

        if trimmed.trim_start().starts_with(FOOTER_PREFIX) {
            flush_paragraph(&mut para_buffer, &mut content);
            flush_list(&mut list_buffer, &mut content);
            content.push(code_block(trimmed.trim()));
            continue;
        }

        if let Some(item) = bullet_item(trimmed) {
            flush_paragraph(&mut para_buffer, &mut content);
            list_buffer.push(item);
            continue;
        }

        if let Some((level, text)) = heading(trimmed) {
            flush_paragraph(&mut para_buffer, &mut content);
            flush_list(&mut list_buffer, &mut content);
            content.push(heading_node(level, &text));
            continue;
        }

        if trimmed.trim().is_empty() {
            flush_paragraph(&mut para_buffer, &mut content);
            flush_list(&mut list_buffer, &mut content);
            continue;
        }

        // Anything else is prose. An unrecognized construct lands here and is
        // shown as written rather than dropped — the total case.
        flush_list(&mut list_buffer, &mut content);
        para_buffer.push(trimmed.to_string());
    }

    flush_paragraph(&mut para_buffer, &mut content);
    flush_list(&mut list_buffer, &mut content);

    json!({ "version": ADF_VERSION, "type": "doc", "content": content })
}

/// The bullet text of a list line, or `None` if this is not one.
///
/// Checklist items keep their `[ ]` / `[x]` marker as literal text. ADF has a
/// `taskList`, but `taskItem` requires a `localId` and whether Jira rewrites
/// those on save is exactly the kind of fact this lane cannot learn without a
/// real tenant — and a rewritten id would break the very fence ADF was chosen
/// to protect. Faithful text now, an upgrade once someone can watch a save.
fn bullet_item(line: &str) -> Option<String> {
    let t = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// `(level, text)` for an ATX heading, clamped to ADF's 1..=6.
fn heading(line: &str) -> Option<(u64, String)> {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return None;
    }
    let hashes = t.chars().take_while(|c| *c == '#').count();
    // Seven or more hashes is not a heading in Markdown either; treat it as
    // prose rather than clamping something that was never a heading.
    if hashes > 6 {
        return None;
    }
    let rest = t[hashes..].strip_prefix(' ')?;
    Some((hashes as u64, rest.trim().to_string()))
}

fn heading_node(level: u64, text: &str) -> Value {
    json!({
        "type": "heading",
        "attrs": { "level": level },
        "content": [text_node(text)],
    })
}

fn text_node(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// The footer's home: visible, contained, and carried verbatim.
fn code_block(text: &str) -> Value {
    json!({
        "type": "codeBlock",
        "attrs": {},
        "content": [text_node(text)],
    })
}

/// Emit the buffered prose lines as one paragraph.
///
/// Lines within a paragraph are joined with a space rather than a newline: a
/// hard break inside an ADF paragraph is a `hardBreak` node, and our bodies
/// never mean one — they mean wrapped prose.
fn flush_paragraph(buffer: &mut Vec<String>, content: &mut Vec<Value>) {
    if buffer.is_empty() {
        return;
    }
    let text = buffer.join(" ");
    buffer.clear();
    content.push(json!({
        "type": "paragraph",
        "content": [text_node(&text)],
    }));
}

/// Emit the buffered bullets as one `bulletList`.
///
/// An ADF `listItem` must contain a block node, not text directly — the
/// paragraph wrapper is required by the schema, not decoration.
fn flush_list(buffer: &mut Vec<String>, content: &mut Vec<Value>) {
    if buffer.is_empty() {
        return;
    }
    let items: Vec<Value> = buffer
        .iter()
        .map(|item| {
            json!({
                "type": "listItem",
                "content": [{
                    "type": "paragraph",
                    "content": [text_node(item)],
                }],
            })
        })
        .collect();
    buffer.clear();
    content.push(json!({ "type": "bulletList", "content": items }));
}

/// Recover the plain text of an ADF document, depth-first.
///
/// The readable half of the round trip. Jira stores ADF, so an inbound
/// description arrives as a tree; this is what lets the reconcile side see
/// the same strings it wrote — most importantly the provenance footer, which
/// no amount of structural equality would help with if its bytes had been
/// reflowed on the way through.
///
/// Total by construction: a node shape it has never seen contributes its
/// `text` if it has one and nothing if it does not, so a future ADF node type
/// degrades to missing text rather than to an error.
#[must_use]
pub fn plain_text(doc: &Value) -> String {
    let mut blocks: Vec<String> = Vec::new();
    collect_blocks(doc, &mut blocks);
    blocks.join("\n")
}

fn collect_blocks(node: &Value, out: &mut Vec<String>) {
    // A leaf-ish block: take its inline text whole and do not descend, so a
    // listItem's paragraph does not also emit as a block of its own.
    if let Some("paragraph" | "heading" | "codeBlock" | "listItem") =
        node.get("type").and_then(Value::as_str)
    {
        let text = inline_text(node);
        if !text.is_empty() {
            out.push(text);
        }
        return;
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            collect_blocks(child, out);
        }
    }
}

fn inline_text(node: &Value) -> String {
    if let Some(t) = node.get("text").and_then(Value::as_str) {
        return t.to_string();
    }
    let Some(children) = node.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    children
        .iter()
        .map(inline_text)
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape Jira validates first. A document missing `version` or `type`
    /// is rejected before any of its content is looked at.
    #[test]
    fn an_empty_body_is_still_a_valid_document() {
        let doc = render_body("");
        assert_eq!(doc["version"], ADF_VERSION);
        assert_eq!(doc["type"], "doc");
        assert_eq!(doc["content"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_heading_carries_its_level() {
        let doc = render_body("## Acceptance");
        let node = &doc["content"][0];
        assert_eq!(node["type"], "heading");
        assert_eq!(node["attrs"]["level"], 2);
        assert_eq!(node["content"][0]["text"], "Acceptance");
    }

    /// The listItem wrapper is schema, not decoration: text directly inside a
    /// listItem is an invalid document.
    #[test]
    fn bullets_become_one_list_whose_items_wrap_text_in_a_paragraph() {
        let doc = render_body("- first\n- second");
        let list = &doc["content"][0];
        assert_eq!(list["type"], "bulletList");
        let items = list["content"].as_array().unwrap();
        assert_eq!(items.len(), 1 + 1);
        assert_eq!(items[0]["type"], "listItem");
        assert_eq!(items[0]["content"][0]["type"], "paragraph");
        assert_eq!(items[0]["content"][0]["content"][0]["text"], "first");
        assert_eq!(items[1]["content"][0]["content"][0]["text"], "second");
    }

    /// Checklist markers survive as text. If this ever becomes a taskList the
    /// change is deliberate and needs a live save to justify it.
    #[test]
    fn a_checklist_item_keeps_its_marker_as_literal_text() {
        let doc = render_body("- [ ] three gates green");
        let item = &doc["content"][0]["content"][0];
        assert_eq!(
            item["content"][0]["content"][0]["text"],
            "[ ] three gates green"
        );
    }

    /// The load-bearing one. The footer must arrive as a codeBlock and its
    /// bytes must be untouched, because the inbound side parses them back.
    #[test]
    fn the_provenance_footer_lands_in_a_code_block_byte_for_byte() {
        let footer = r#"<!-- think-and-ship: {"chunk":"jira-body-format","project":"p"} -->"#;
        let doc = render_body(footer);
        let node = &doc["content"][0];
        assert_eq!(node["type"], "codeBlock");
        assert_eq!(node["content"][0]["text"], footer);
    }

    /// A blank line separates blocks; it must not produce an empty paragraph,
    /// which Jira renders as a stray gap.
    #[test]
    fn blank_lines_separate_blocks_without_emitting_empty_paragraphs() {
        let doc = render_body("one\n\n\ntwo");
        let blocks = doc["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["content"][0]["text"], "one");
        assert_eq!(blocks[1]["content"][0]["text"], "two");
    }

    /// Wrapped prose is one paragraph, not one per source line.
    #[test]
    fn consecutive_prose_lines_join_into_a_single_paragraph() {
        let doc = render_body("a sentence that was\nwrapped by the author");
        let blocks = doc["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0]["content"][0]["text"],
            "a sentence that was wrapped by the author"
        );
    }

    /// Totality. Nothing is rejected and nothing is dropped: syntax we do not
    /// model is shown as written.
    #[test]
    fn unrecognized_syntax_is_passed_through_as_prose_rather_than_dropped() {
        let doc = render_body("| a | table |\n####### not a heading");
        let text = plain_text(&doc);
        assert!(text.contains("| a | table |"), "table row lost: {text}");
        assert!(
            text.contains("####### not a heading"),
            "over-long hash run lost: {text}"
        );
    }

    /// The chunk's round-trip criterion, on the whole authored shape rather
    /// than on a fragment.
    #[test]
    fn the_full_projected_shape_round_trips_readably() {
        let body = "\
Sub 2 of 6, split from tracker-jira-adapter.

## Acceptance

- [ ] the decision is made in writing
- [ ] three Rust gates green

<!-- think-and-ship: {\"chunk\":\"jira-body-format\"} -->";
        let doc = render_body(body);
        let text = plain_text(&doc);

        assert!(text.contains("Sub 2 of 6, split from tracker-jira-adapter."));
        assert!(text.contains("Acceptance"));
        assert!(text.contains("[ ] the decision is made in writing"));
        assert!(text.contains("[ ] three Rust gates green"));
        assert!(
            text.contains("<!-- think-and-ship: {\"chunk\":\"jira-body-format\"} -->"),
            "the footer did not survive the round trip: {text}"
        );
    }

    /// An ADF tree we did not build — the inbound direction. A node type this
    /// renderer never emits must not cost us the text around it.
    #[test]
    fn plain_text_survives_a_node_type_this_module_never_emits() {
        let doc = json!({
            "version": 1,
            "type": "doc",
            "content": [
                { "type": "panel", "attrs": { "panelType": "info" }, "content": [
                    { "type": "paragraph", "content": [{ "type": "text", "text": "inside a panel" }] }
                ]},
                { "type": "paragraph", "content": [{ "type": "text", "text": "after it" }] }
            ]
        });
        assert_eq!(plain_text(&doc), "inside a panel\nafter it");
    }

    /// Marks are formatting, not content: a linked span still yields its text.
    #[test]
    fn plain_text_reads_through_marks() {
        let doc = json!({
            "version": 1,
            "type": "doc",
            "content": [{ "type": "paragraph", "content": [
                { "type": "text", "text": "see " },
                { "type": "text", "text": "the docs", "marks": [
                    { "type": "link", "attrs": { "href": "https://example.com" } }
                ]}
            ]}]
        });
        assert_eq!(plain_text(&doc), "see the docs");
    }

    /// Determinism, for the same reason `provenance_footer` orders its keys:
    /// an unstable rendering would change the content hash every run and
    /// defeat the projector's no-op skip.
    #[test]
    fn rendering_is_deterministic() {
        let body = "## H\n\n- one\n- two\n\nprose";
        assert_eq!(
            serde_json::to_string(&render_body(body)).unwrap(),
            serde_json::to_string(&render_body(body)).unwrap()
        );
    }
}
