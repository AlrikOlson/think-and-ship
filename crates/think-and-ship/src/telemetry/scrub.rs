//! Detector-based output verification (defense-in-depth for 31h-a).
//!
//! The extractor's primary guarantee is structural: it only ever emits
//! closed-vocabulary tokens and salted-hash pseudonyms. This module is the
//! second, independent layer — a gitleaks-style detector (patterns + an
//! entropy heuristic, no ML) run over the *serialized* shape, so a future
//! regression that lets payload text slip through fails loudly instead of
//! shipping. 2026 at-source practice is detector-based; gateway-class NER
//! stacks (Presidio / LLM Guard) are deliberately out of scope here.

/// One detector hit in scanned output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubFinding {
    /// Which detector fired (stable identifier, e.g. `"aws-access-key"`).
    pub detector: &'static str,
    /// The offending fragment (truncated to 24 chars — enough to locate it
    /// in a test failure without copying the secret around whole).
    pub excerpt: String,
}

fn finding(detector: &'static str, fragment: &str) -> ScrubFinding {
    ScrubFinding {
        detector,
        excerpt: fragment.chars().take(24).collect(),
    }
}

/// Known high-signal secret prefixes (the gitleaks-style fixed-prefix class).
const SECRET_PREFIXES: &[(&str, &str)] = &[
    ("aws-access-key", "AKIA"),
    ("stripe-secret-key", "sk_live_"),
    ("stripe-test-key", "sk_test_"),
    ("github-token", "ghp_"),
    ("github-oauth", "gho_"),
    ("slack-token", "xoxb-"),
    ("jwt", "eyJ"),
    ("private-key", "-----BEGIN"),
];

/// Shannon entropy in bits per character.
fn entropy_per_char(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Whether a token is one of OUR hash pseudonyms: fixed-width lowercase hex.
/// Hashes are high-entropy by design, so the entropy detector exempts them.
fn is_hash_pseudonym(token: &str) -> bool {
    token.len() == crate::telemetry::shape::HASH_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Scan serialized telemetry output for anything that looks like leaked
/// content: known secret prefixes, email addresses, and long high-entropy
/// runs that are not our own hash pseudonyms. Returns every finding (empty =
/// clean). Tokenization is on JSON-structural + whitespace boundaries, so a
/// secret embedded in prose still surfaces as its own token.
#[must_use]
pub fn scan(serialized: &str) -> Vec<ScrubFinding> {
    let mut findings = Vec::new();

    for token in serialized.split(|c: char| c.is_whitespace() || "\"{}[],:".contains(c)) {
        if token.is_empty() {
            continue;
        }
        for (name, prefix) in SECRET_PREFIXES {
            if token.contains(prefix) {
                findings.push(finding(name, token));
            }
        }
        // Email shape: local@domain.tld without pulling in a regex engine.
        if let Some(at) = token.find('@') {
            let (local, rest) = token.split_at(at);
            if !local.is_empty() && rest[1..].contains('.') {
                findings.push(finding("email", token));
            }
        }
        // High-entropy runs (random keys/base64 blobs). Our own pseudonyms
        // are exempt by exact shape, and the check only applies to tokens
        // carrying digits or uppercase — real key material virtually always
        // does, while our composite vocabulary (`roadmap.chunk.in_progress`)
        // never does and would otherwise false-positive on letter diversity.
        let mixed_charset = token
            .bytes()
            .any(|b| b.is_ascii_digit() || b.is_ascii_uppercase());
        if token.len() >= 20
            && mixed_charset
            && !is_hash_pseudonym(token)
            && entropy_per_char(token) > 3.8
        {
            findings.push(finding("high-entropy", token));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_known_secret_prefixes_and_emails() {
        let dirty = r#"{"x":"AKIAIOSFODNN7EXAMPLE","y":"alice@example.com","z":"ghp_abc123"}"#;
        let detectors: Vec<_> = scan(dirty).into_iter().map(|f| f.detector).collect();
        assert!(detectors.contains(&"aws-access-key"));
        assert!(detectors.contains(&"email"));
        assert!(detectors.contains(&"github-token"));
    }

    #[test]
    fn flags_high_entropy_but_exempts_our_pseudonyms() {
        let blob = "q9X2vK8pL3mN7rT5wY1zB6cD"; // 24 chars, mixed case+digits
        assert!(
            scan(&format!("{{\"a\":\"{blob}\"}}"))
                .iter()
                .any(|f| f.detector == "high-entropy")
        );
        // A 16-char lowercase-hex pseudonym (our hash shape) passes.
        assert!(scan(r#"{"a":"deadbeef01234567"}"#).is_empty());
    }

    #[test]
    fn clean_structural_output_has_no_findings() {
        let clean = r#"{"families":{"think":{"step":42}},"edges":7,"relations":{"supports":3}}"#;
        assert!(scan(clean).is_empty());
    }
}
