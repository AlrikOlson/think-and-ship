//! The docs' own commands, run through the real parser.
//!
//! # Why this file exists
//!
//! The README once told readers to run `think-and-ship trace export --otel`.
//! That flag never existed. It had propagated into the command table, the
//! Jaeger demo block, the prose, `docs/ARCHITECTURE.md`, and three Rust doc
//! comments — and every gate in the repo was green the entire time, because no
//! gate reads the documentation's own commands. It was found by hand, by
//! deciding to drive the real binary instead of trusting the unit tests.
//!
//! Building this gate immediately found a second one: `think-and-ship --check`,
//! documented as the *post-install verification step*, which no version of the
//! binary has ever accepted.
//!
//! So: extract every invocation of our own binary from the prose, and assert it
//! parses against the real [`Cli`] grammar.
//!
//! # Argument-level, not subcommand-level
//!
//! `--otel` sits on `trace export`, which is a *real* subcommand. A check that
//! only verified subcommand paths would have shrugged at it, and a `--help`
//! probe would have too — `trace export --help` succeeds no matter what other
//! arguments you would have passed. The only mechanism that catches a fake flag
//! on a real subcommand is the actual parser, so this file calls
//! [`Cli::try_parse_from`] and lets clap be the single source of truth. Nothing
//! here reimplements clap's matching rules; a second implementation of the
//! grammar is the exact class of bug this gate exists to kill.
//!
//! # Parse, never execute
//!
//! `try_parse_from` is a pure function of the argv. Documented commands write
//! state (`init`, `roadmap import`), hit the network (`connect`, `sync push`,
//! `tracker push`), and start servers (`serve`) — none of them run here.
//!
//! # What counts as a documented invocation
//!
//! Two syntactic kinds, deliberately handled differently:
//!
//! - **Fenced** — a line inside a shell-tagged code fence. These are literal,
//!   runnable commands and are validated literally. Fences tagged anything else
//!   are skipped, which is what keeps the server-log block in README (whose
//!   lines begin `think-and-ship http on http://…`) from being mistaken for a
//!   command.
//! - **Inline** — a backtick span, which is where the command table and the
//!   prose references live. These are written in documentation meta-notation:
//!   `[--out FILE]` optionals, `<id>` and ALLCAPS placeholders, and alternation
//!   `markdown\|json`. The extractor *expands* that notation rather than
//!   skipping it, because the command table is precisely where `--otel` was
//!   enshrined as reference material.
//!
//! Optionals are read as *simultaneously* available: `cmd [--a] [--b]` asserts
//! that a reader may write `cmd --a --b`. That is the maximal claim a table row
//! makes, and reading it any weaker would have missed the second defect this
//! gate found — `corpus eval [--prequential] [--learned]`, two flags clap
//! declares `conflicts_with` each other. A row that lists mutually exclusive
//! flags side by side must say so, and now does.
//!
//! # The relaxation, stated out loud
//!
//! A documented command may legitimately elide a required argument — prose says
//! "the `think-and-ship trace promote` CLI" to *name* a command, not to give a
//! runnable line. So [`ErrorKind::MissingRequiredArgument`] and
//! [`ErrorKind::MissingSubcommand`] are tolerated, and `DisplayHelp` /
//! `DisplayVersion` are outright passes (they mean the flag was recognized).
//! Everything else — an unknown argument, an invalid subcommand, a bad value —
//! is a documentation defect. `--otel` and `--check` are both `UnknownArgument`,
//! so the relaxation does not touch the class of bug this gate is for. See
//! [`an_elided_required_argument_is_tolerated`] for the executable statement of
//! this rule.
//!
//! # Failing open is the real risk
//!
//! A gate that scans source text reports green when its extractor finds
//! *nothing*. That has bitten this project before. So the extractor's own
//! non-vacuity is asserted by three separate tests — a floor on the count, a
//! set of known invocations demanded by exact argv, and coverage of both
//! syntactic kinds and both corpus roots. Breaking the extractor turns those
//! red; it cannot quietly turn the corpus check green.

use clap::Parser;
use clap::error::ErrorKind;
use std::path::{Path, PathBuf};
use think_and_ship::cli::args::Cli;

/// Our binary's name, as it appears in the prose and as argv[0] when parsing.
const BIN: &str = "think-and-ship";

/// Cartesian expansion of a meta-notation spec is bounded so a future doc line
/// cannot silently explode the corpus. Exceeding it panics rather than skips —
/// a skipped command is an unchecked command.
const MAX_EXPANSION: usize = 32;

/// Which syntactic kind of documentation a command was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// A line inside a shell-tagged code fence: a literal, runnable command.
    Fenced,
    /// A backtick span: the command table and prose references, written in
    /// documentation meta-notation.
    Inline,
}

/// One documented invocation, resolved to a concrete argv.
#[derive(Debug, Clone)]
struct DocCommand {
    file: String,
    line: usize,
    source: Source,
    /// The text as it appears in the doc, for the failure message.
    raw: String,
    /// The argv *after* argv[0], ready for `try_parse_from`.
    argv: Vec<String>,
}

impl DocCommand {
    fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

/// The repo root, derived from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/think-and-ship is two levels below the repo root")
        .to_path_buf()
}

/// The documents whose commands are held to the grammar.
///
/// `CHANGELOG.md` is deliberately excluded: it is a historical record, and a
/// command that was correct in v0.2.0 should not be rewritten to satisfy
/// today's grammar.
fn corpus_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = vec![
        root.join("README.md"),
        root.join("DEPLOY.md"),
        root.join("AGENTS.md"),
    ];

    let docs = root.join("docs");
    let mut from_docs: Vec<PathBuf> = std::fs::read_dir(&docs)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", docs.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    from_docs.sort();
    files.extend(from_docs);

    files.retain(|p| p.exists());
    files
}

/// Every documented invocation across the whole corpus.
fn documented_commands() -> Vec<DocCommand> {
    let root = repo_root();
    let mut all = Vec::new();
    for path in corpus_files() {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        // Labels are repo-relative with forward slashes on every platform:
        // the corpus-coverage assertion matches on the "docs/" prefix, which
        // a Windows `\` separator would silently defeat.
        let label = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");
        all.extend(extract(&label, &text));
    }
    all
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Fence tags whose contents are shell commands.
///
/// Anything else — `text`, `json`, or an untagged fence — is prose, data, or
/// program output, and is not held to the grammar.
fn is_command_fence(tag: &str) -> bool {
    matches!(tag.trim(), "sh" | "bash" | "shell" | "zsh" | "console")
}

fn extract(file: &str, text: &str) -> Vec<DocCommand> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;

    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;

        if let Some(rest) = line.trim_start().strip_prefix("```") {
            fence = match fence {
                Some(_) => None,
                None => Some(rest.trim().to_string()),
            };
            continue;
        }

        match &fence {
            Some(tag) if is_command_fence(tag) => {
                out.extend(fenced_command(file, line_no, line));
            }
            // A non-command fence: skipped wholesale.
            Some(_) => {}
            None => out.extend(inline_commands(file, line_no, line)),
        }
    }

    out
}

/// A literal command line inside a shell fence.
fn fenced_command(file: &str, line_no: usize, line: &str) -> Option<DocCommand> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let without_prompt = trimmed.strip_prefix("$ ").unwrap_or(trimmed).trim_start();
    let command = cut_at_shell_operator(without_prompt);
    let body = strip_env_prefix(command)?;

    Some(DocCommand {
        file: file.to_string(),
        line: line_no,
        source: Source::Fenced,
        raw: trimmed.to_string(),
        argv: tokenize(body),
    })
}

/// Every backtick span on a prose line that invokes our binary.
fn inline_commands(file: &str, line_no: usize, line: &str) -> Vec<DocCommand> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let Some(end) = (i + 1..chars.len())
            .position(|j| chars[j] == '`')
            .map(|p| p + i + 1)
        else {
            break;
        };
        let span: String = chars[i + 1..end].iter().collect();
        for argv in expand_spec(&span) {
            out.push(DocCommand {
                file: file.to_string(),
                line: line_no,
                source: Source::Inline,
                raw: span.clone(),
                argv,
            });
        }
        i = end + 1;
    }

    out
}

/// Expand one backtick span written in documentation meta-notation into every
/// concrete argv it claims is valid.
///
/// Returns empty for a span that does not invoke our binary — which is most of
/// them, since backticks are also how the docs quote type names, env vars, and
/// file paths.
fn expand_spec(span: &str) -> Vec<Vec<String>> {
    let trimmed = span.trim();
    let without_prompt = trimmed.strip_prefix("$ ").unwrap_or(trimmed).trim_start();
    let Some(body) = strip_env_prefix(without_prompt) else {
        return Vec::new();
    };

    // Each token expands to one-or-more alternatives; the command is the
    // cartesian product across tokens.
    let per_token: Vec<Vec<String>> = tokenize(body)
        .iter()
        .map(|token| alternatives(token))
        .filter(|alts| !alts.is_empty())
        .collect();

    let total: usize = per_token.iter().map(Vec::len).product::<usize>().max(1);
    assert!(
        total <= MAX_EXPANSION,
        "documented command `{span}` expands to {total} variants (cap {MAX_EXPANSION}); \
         tighten the notation rather than letting the gate skip it",
    );

    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for alts in per_token {
        out = out
            .into_iter()
            .flat_map(|prefix| {
                alts.iter().map(move |alt| {
                    let mut next = prefix.clone();
                    next.push(alt.clone());
                    next
                })
            })
            .collect();
    }
    out
}

/// The concrete values one meta-notation token stands for.
///
/// - `[--dry-run]` → the flag, brackets dropped (an optional is still a claim
///   that the flag exists).
/// - `markdown\|json` → two alternatives.
/// - `<id>`, `FILE`, `N` → a placeholder, substituted with `1`, which is
///   simultaneously a valid integer, string, and path — so one substitution
///   serves every argument type in the grammar.
fn alternatives(token: &str) -> Vec<String> {
    let unescaped = token.replace("\\|", "|");
    let bare: String = unescaped
        .chars()
        .filter(|c| !matches!(c, '[' | ']'))
        .collect();
    if bare.is_empty() {
        return Vec::new();
    }

    bare.split('|')
        .filter(|alt| !alt.is_empty())
        .map(|alt| {
            if is_placeholder(alt) {
                "1".to_string()
            } else {
                alt.to_string()
            }
        })
        .collect()
}

/// A stand-in for a value the reader supplies: `<id>`, or an ALLCAPS metavar.
fn is_placeholder(token: &str) -> bool {
    if token.starts_with('<') && token.ends_with('>') && token.len() > 2 {
        return true;
    }
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && token.chars().any(|c| c.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// Shell-ish tokenizing
// ---------------------------------------------------------------------------

/// Truncate at the first shell operator, so a redirect or pipe is not mistaken
/// for an argument. `> ROADMAP.md` is the shell's business, not clap's.
fn cut_at_shell_operator(s: &str) -> &str {
    let mut cut = s.len();
    for (i, c) in s.char_indices() {
        if matches!(c, '|' | '>' | '<' | ';' | '&') {
            cut = cut.min(i);
        }
        // A trailing comment, which the corpus uses to annotate command lines.
        if c == '#' && i > 0 && s[..i].ends_with(char::is_whitespace) {
            cut = cut.min(i);
        }
    }
    s[..cut].trim_end()
}

/// Drop any leading `KEY=value` environment prefixes, then require that what
/// remains starts with our binary. Returns the argument text, or `None` when
/// this is not an invocation of ours.
fn strip_env_prefix(s: &str) -> Option<&str> {
    let mut rest = s.trim();
    loop {
        let (head, tail) = match rest.split_once(char::is_whitespace) {
            Some((head, tail)) => (head, tail.trim_start()),
            None => (rest, ""),
        };

        if head == BIN {
            return Some(tail);
        }
        // An env assignment: `THINK_AND_SHIP_PERSIST=true`.
        let is_env = head.split_once('=').is_some_and(|(key, _)| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_uppercase() || c == '_')
        });
        if !is_env || tail.is_empty() {
            return None;
        }
        rest = tail;
    }
}

/// Split on whitespace, honoring double quotes.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut started = false;

    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Run one argv through the real grammar. `Some(reason)` is a documentation
/// defect; `None` means the command is reachable.
///
/// See the module docs for why `MissingRequiredArgument` / `MissingSubcommand`
/// are tolerated and everything else is not.
fn reachability_failure(argv: &[String]) -> Option<String> {
    let full: Vec<String> = std::iter::once(BIN.to_string())
        .chain(argv.iter().cloned())
        .collect();

    match Cli::try_parse_from(full) {
        Ok(_) => None,
        Err(err) => match err.kind() {
            ErrorKind::MissingRequiredArgument
            | ErrorKind::MissingSubcommand
            | ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => None,
            kind => {
                let rendered = err.render().to_string();
                let first = rendered
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                Some(format!("{kind:?} — {first}"))
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Claim 1: the corpus is reachable
// ---------------------------------------------------------------------------

/// THE GATE. Every command the documentation tells a reader to run must parse
/// against the grammar that reader's binary actually has.
#[test]
fn every_documented_command_is_reachable() {
    let commands = documented_commands();
    let mut failures = Vec::new();

    for command in &commands {
        if let Some(reason) = reachability_failure(&command.argv) {
            failures.push(format!(
                "  {loc}\n    documented: {raw}\n    parsed as:  {argv:?}\n    rejected:   {reason}",
                loc = command.location(),
                raw = command.raw,
                argv = command.argv,
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} documented command(s) cannot be run by a reader:\n{}",
        failures.len(),
        commands.len(),
        failures.join("\n"),
    );
}

// ---------------------------------------------------------------------------
// Claims 2-4: the extractor is not vacuous
//
// Without these, breaking the extractor turns the gate above green.
// ---------------------------------------------------------------------------

/// A floor on the corpus size. An extractor that quietly matches nothing —
/// the classic fail-open for a source-scanning gate — goes red here.
#[test]
fn extraction_is_not_vacuous() {
    let commands = documented_commands();
    assert!(
        commands.len() >= 40,
        "expected the docs to yield at least 40 invocations, found {} — \
         the extractor has probably stopped matching, which would make \
         every_documented_command_is_reachable pass by finding nothing",
        commands.len(),
    );
}

/// A count is satisfiable by an extractor that degraded to matching only the
/// easy cases, so demand specific invocations by exact argv — one per
/// extraction feature the gate depends on.
#[test]
fn extraction_finds_the_hard_cases_by_exact_argv() {
    let found: Vec<Vec<String>> = documented_commands().into_iter().map(|c| c.argv).collect();

    let required: &[(&str, &[&str])] = &[
        // A plain fenced command.
        (
            "a fenced command line",
            &["trace", "export", "--out", "trace.json"],
        ),
        // An env-var prefix must be stripped, and a `>` redirect cut.
        (
            "an env-prefixed, redirected fenced command",
            &["roadmap", "export", "--format", "markdown"],
        ),
        // The command table's optionals must be unwrapped, not skipped: this
        // is the row shape that carried `--otel`.
        (
            "a table row with bracketed optionals",
            &["trace", "export", "--out", "1"],
        ),
        // Alternation in a value slot.
        (
            "alternation in a value slot",
            &["roadmap", "export", "--format", "json"],
        ),
        // Alternation in a subcommand slot.
        ("alternation in a subcommand slot", &["telemetry", "push"]),
        // An angle-bracket placeholder alongside an ALLCAPS one.
        (
            "placeholders of both spellings",
            &["trace", "promote", "--session", "1", "--step", "1"],
        ),
    ];

    for (what, argv) in required {
        let wanted: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        assert!(
            found.contains(&wanted),
            "the extractor no longer finds {what} ({argv:?}); \
             it is documented, so its absence means extraction is broken",
        );
    }
}

/// Both syntactic kinds and both corpus roots must be live. A regression that
/// dropped inline spans would leave the whole command table unchecked while
/// the gate stayed green on fenced blocks alone.
#[test]
fn extraction_covers_both_kinds_and_both_roots() {
    let commands = documented_commands();

    let fenced = commands
        .iter()
        .filter(|c| c.source == Source::Fenced)
        .count();
    let inline = commands
        .iter()
        .filter(|c| c.source == Source::Inline)
        .count();
    assert!(fenced >= 10, "only {fenced} fenced command(s) extracted");
    assert!(inline >= 20, "only {inline} inline command(s) extracted");

    assert!(
        commands.iter().any(|c| c.file == "README.md"),
        "no commands extracted from README.md",
    );
    assert!(
        commands.iter().any(|c| c.file.starts_with("docs/")),
        "no commands extracted from docs/",
    );
}

// ---------------------------------------------------------------------------
// Claims 5-7: the validator has teeth
//
// These are corpus-independent. They stay meaningful on a day the docs happen
// to be clean, which is every day the gate is working.
// ---------------------------------------------------------------------------

/// The original defect, as an executable assertion: a fake flag on a *real*
/// subcommand is rejected. A subcommand-only check would pass this.
#[test]
fn a_fake_flag_on_a_real_subcommand_is_rejected() {
    let real = ["trace".to_string(), "export".to_string()];
    assert!(
        reachability_failure(&real).is_none(),
        "`trace export` is a real command; the fixture is wrong",
    );

    let with_fake_flag = [
        "trace".to_string(),
        "export".to_string(),
        "--otel".to_string(),
        "--out".to_string(),
        "trace.json".to_string(),
    ];
    let reason = reachability_failure(&with_fake_flag)
        .expect("`trace export --otel` must be rejected — it is the defect this gate exists for");
    assert!(
        reason.contains("UnknownArgument"),
        "expected an unknown-argument rejection, got: {reason}",
    );
}

/// The second live defect this gate caught: a fake flag on the *root*.
#[test]
fn a_fake_root_flag_is_rejected() {
    let reason = reachability_failure(&["--check".to_string()])
        .expect("`think-and-ship --check` must be rejected — the root has no such flag");
    assert!(
        reason.contains("UnknownArgument"),
        "expected an unknown-argument rejection, got: {reason}",
    );
}

/// A subcommand that does not exist is rejected too — the easy half of the job,
/// asserted so a refactor cannot lose it while keeping the flag check.
#[test]
fn a_fake_subcommand_is_rejected() {
    assert!(
        reachability_failure(&["teleport".to_string()]).is_some(),
        "an invented subcommand must be rejected",
    );
    assert!(
        reachability_failure(&["trace".to_string(), "teleport".to_string()]).is_some(),
        "an invented nested subcommand must be rejected",
    );
}

/// The relaxation, stated as a test rather than as a comment: prose that *names*
/// a command without giving its required arguments is not a defect. If this ever
/// starts failing, the tolerance was tightened and the docs need full argv.
#[test]
fn an_elided_required_argument_is_tolerated() {
    // `trace promote` requires --session; the docs name it without one.
    assert!(
        reachability_failure(&["trace".to_string(), "promote".to_string()]).is_none(),
        "naming a command without its required arguments is elision, not unreachability",
    );
    // The bare binary name is a name, not a claim about arguments.
    assert!(
        reachability_failure(&[]).is_none(),
        "the bare binary name makes no argument claim",
    );
}

// ---------------------------------------------------------------------------
// Claim 8: the meta-notation reader itself
// ---------------------------------------------------------------------------

/// The notation expander is the one piece of bespoke logic between the docs and
/// clap, so it gets its own unit assertions rather than being covered only
/// through the corpus.
#[test]
fn meta_notation_expands_as_documented() {
    assert_eq!(
        expand_spec("think-and-ship roadmap next"),
        vec![vec!["roadmap".to_string(), "next".to_string()]],
    );

    // Optionals are unwrapped, not dropped — the flag inside is still a claim.
    assert_eq!(
        expand_spec("think-and-ship repair [--dry-run]"),
        vec![vec!["repair".to_string(), "--dry-run".to_string()]],
    );

    // Alternation multiplies.
    assert_eq!(
        expand_spec("think-and-ship telemetry status\\|on\\|off\\|push").len(),
        4,
    );

    // Placeholders of both spellings become a value valid for every arg type.
    assert_eq!(
        expand_spec("think-and-ship connect [--url URL]"),
        vec![vec![
            "connect".to_string(),
            "--url".to_string(),
            "1".to_string()
        ]],
    );

    // Not our binary: no claim, no commands.
    assert!(expand_spec("cargo install think-and-ship").is_empty());
    assert!(expand_spec("think-and-ship:0.2.0").is_empty());
    assert!(expand_spec("THINK_AND_SHIP_PERSIST").is_empty());
}

/// Server log output and prose fences must not be mistaken for commands. The
/// README's untagged block contains the line `think-and-ship http on http://…`,
/// which is what the server prints, not something a reader can run.
#[test]
fn non_shell_fences_are_not_commands() {
    let doc = "\
before

```
think-and-ship http on http://0.0.0.0:8080/mcp
```

```sh
think-and-ship doctor
```
";
    let found = extract("fixture.md", doc);
    assert_eq!(
        found.len(),
        1,
        "expected only the sh-fenced command, got {found:#?}",
    );
    assert_eq!(found[0].argv, vec!["doctor".to_string()]);
}
