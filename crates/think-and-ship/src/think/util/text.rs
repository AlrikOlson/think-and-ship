//! UTF-8-safe text helpers used by the engine for response excerpts.

/// Returns a small excerpt around `byte_pos` (inclusive of `match_len` chars
/// at that position), with `context_chars` of surrounding context on each
/// side. Adds ellipses when truncated.
///
/// Boundary-safe: the byte position is mapped to a char index, then expanded
/// by `context_chars` chars (not bytes) so the result never splits a
/// multi-byte glyph.
pub fn excerpt_around(
    text: &str,
    byte_pos: usize,
    match_len: usize,
    context_chars: usize,
) -> String {
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    let match_char_idx = char_indices
        .iter()
        .position(|(bi, _)| *bi >= byte_pos)
        .unwrap_or(char_indices.len());
    let start = match_char_idx.saturating_sub(context_chars);
    let end_match = char_indices
        .iter()
        .position(|(bi, _)| *bi >= byte_pos + match_len)
        .unwrap_or(char_indices.len());
    let end = end_match
        .saturating_add(context_chars)
        .min(char_indices.len());

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    for (_, ch) in &char_indices[start..end] {
        out.push(*ch);
    }
    if end < char_indices.len() {
        out.push('…');
    }
    out
}

/// Truncate a string to at most `max` chars (graphemes-naïve; byte-safe via
/// `char_indices`). Adds an ellipsis when truncated.
pub fn truncate_excerpt(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::with_capacity(max + 1);
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}
