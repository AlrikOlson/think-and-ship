//! Declaring a project's identity, and noticing when nothing agrees with it.
//!
//! # Two claims, one file
//!
//! **Declaring is not configuring.** `init` writes `.think-and-ship/project.json`
//! and also authors an MCP entry for every client it finds. A user who wants to
//! say "this repository is one project" should not thereby acquire a
//! `.cursor/mcp.json` and a `.windsurf/mcp.json` in a directory they commit. The
//! identity-only path is held to that literally: the set of files in a directory
//! before and after must differ by exactly the identity file.
//!
//! **A declaration nothing agrees with is a silent detachment.**
//! `write_project_file` refuses to overwrite, so no code path can produce a
//! disagreement — but the file is committed and hand-editable, and an edited id
//! files every future record under a name none of the existing ones share. Each
//! store stays internally consistent; the history simply stops being reachable.
//!
//! # Why the file-set assertion is written the way it is
//!
//! A gate that compares two sets reports green when both are empty. So the
//! fixture seeds real files, and the assertions demand a floor on what was
//! scanned and the presence of the identity file in the after-set. Breaking the
//! walker turns those red rather than quietly turning the comparison green.
//!
//! # Why the disagreement rule takes a table
//!
//! [`identity_disagreement`] is given the per-family record counts instead of
//! reading them. A rule that reads its own inputs can only be exercised against
//! the families this build happens to persist, and "the answer comes from the
//! stores" becomes a restatement of the implementation. Here the table is a
//! parameter, so the tests drive families that do not exist.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use think_and_ship::cli::setup::{MarkPlan, identity_disagreement, mark_in, plan_mark};
use think_and_ship::infra::{PROJECT_DIR, PROJECT_FILE, declared_identity_in};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A directory that looks like a real project a person would mark: sources, a
/// readme, and the marker directories of two agent clients. The clients matter —
/// they are what `init` reads to decide whose MCP config to author, so their
/// presence is what makes "wrote nothing else" a claim with something behind it.
fn seed_project(root: &Path) {
    let files: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"demo\"\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("README.md", "# demo\n"),
        ("CLAUDE.md", "# demo\n"),
        (".cursor/rules/house.md", "be nice\n"),
        (".windsurf/rules/house.md", "be nice\n"),
        (".vscode/settings.json", "{}\n"),
    ];
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("seeded paths have a parent")).unwrap();
        std::fs::write(&path, body).unwrap();
    }
}

/// Every file under `root`, as repo-relative paths with forward slashes.
fn file_set(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

/// The identity file's repo-relative path, spelled the way [`file_set`] does.
fn identity_file() -> String {
    format!("{PROJECT_DIR}/{PROJECT_FILE}")
}

fn holdings(rows: &[(&str, usize)]) -> Vec<(String, usize)> {
    rows.iter()
        .map(|(unit, count)| ((*unit).to_string(), *count))
        .collect()
}

// ---------------------------------------------------------------------------
// Claim 1: declaring an identity writes the identity file and nothing else
// ---------------------------------------------------------------------------

/// THE GATE. The whole effect of declaring an identity is one file.
#[test]
fn declaring_an_identity_writes_one_file_and_nothing_else() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_project(root);

    let before = file_set(root);
    // Non-vacuity: a walker that found nothing would make the comparison below
    // trivially true, which is how a set-difference gate fails open.
    assert!(
        before.len() >= 7,
        "the fixture should have been scanned, found {} file(s): {before:?}",
        before.len(),
    );
    assert!(
        !before.contains(&identity_file()),
        "precondition: the fixture declares no identity",
    );

    mark_in(root, None, false).expect("marking an unmarked directory succeeds");

    let after = file_set(root);
    let added: Vec<&String> = after.difference(&before).collect();
    let removed: Vec<&String> = before.difference(&after).collect();

    assert_eq!(
        added,
        vec![&identity_file()],
        "declaring an identity must add exactly the identity file",
    );
    assert!(
        removed.is_empty(),
        "declaring an identity must remove nothing, removed {removed:?}",
    );
    assert!(
        after.contains(&identity_file()),
        "the identity file must be there afterwards, or the comparison proved nothing",
    );
}

/// The file that appears is a real declaration, not an empty placeholder — and
/// it carries the id the directory ALREADY resolved to, because every record the
/// project holds is filed under that one.
#[test]
fn the_declared_id_is_the_one_the_project_already_had() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_project(root);

    let before = plan_mark(root);
    let MarkPlan::Declare { id: expected, .. } = before.clone() else {
        panic!("an unmarked directory should plan to declare, got {before:?}");
    };

    mark_in(root, Some("Demo"), false).unwrap();

    let declared = declared_identity_in(root).expect("the identity file parses");
    assert_eq!(
        declared.id, expected,
        "marking records the id the project already answers to; a fresh one would \
         detach everything it holds",
    );
    assert_eq!(declared.name.as_deref(), Some("demo"));
}

/// A dry run is a report, and reports do not write.
#[test]
fn a_dry_run_declares_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_project(root);

    let before = file_set(root);
    let plan = mark_in(root, None, true).unwrap();

    assert!(matches!(plan, MarkPlan::Declare { .. }));
    assert_eq!(file_set(root), before, "a dry run must touch no file");
}

/// Marking twice is not re-marking. The second call reports the existing
/// declaration and leaves it alone.
#[test]
fn marking_twice_leaves_the_first_answer_standing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_project(root);

    mark_in(root, None, false).unwrap();
    let first = declared_identity_in(root).unwrap().id;
    let after_first = file_set(root);

    let plan = mark_in(root, Some("renamed"), false).unwrap();
    assert!(
        matches!(plan, MarkPlan::AlreadyDeclared { .. }),
        "a declared project reports its declaration, got {plan:?}",
    );
    assert_eq!(declared_identity_in(root).unwrap().id, first);
    assert_eq!(file_set(root), after_first);
}

// ---------------------------------------------------------------------------
// Claim 2: a subdirectory of a declared project cannot declare its own identity
// ---------------------------------------------------------------------------

/// Resolution takes the NEAREST declaration walking up, so a second file inside
/// a marked repository shadows the root's for that subtree — one repository
/// answering as two projects, which is the state the declaration exists to end.
#[test]
fn a_subdirectory_of_a_declared_project_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    seed_project(root);
    mark_in(root, None, false).unwrap();

    let inner = root.join("crates").join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("lib.rs"), "// inner\n").unwrap();

    let plan = plan_mark(&inner);
    assert!(
        matches!(plan, MarkPlan::DeclaredByAncestor { .. }),
        "a subdirectory inside a declared project is already spoken for, got {plan:?}",
    );

    let before = file_set(root);
    mark_in(&inner, None, false).unwrap();
    assert_eq!(
        file_set(root),
        before,
        "a refusal writes nothing, least of all a second identity file",
    );

    let nested: PathBuf = inner.join(PROJECT_DIR).join(PROJECT_FILE);
    assert!(!nested.exists(), "no second declaration inside the project");
}

// ---------------------------------------------------------------------------
// Claim 3: a declaration the records disagree with is visible
// ---------------------------------------------------------------------------

/// The rule reads the table it is given, including families this build has never
/// heard of. Hardcoding roadmap/think/signal would pass every test that used
/// them and this one alone goes red.
#[test]
fn the_disagreement_is_read_from_the_table_it_is_given() {
    let foreign = holdings(&[("ledger", 12), ("transcript", 4)]);
    let found = identity_disagreement(Some("renamed-by-hand"), "demo-abc123", &foreign)
        .expect("a declaration that differs, with records behind the difference");

    assert_eq!(found.declared, "renamed-by-hand");
    assert_eq!(found.detached, "demo-abc123");
    assert_eq!(found.holding, foreign);
    assert_eq!(found.summary(), "12 ledger(s), 4 transcript(s)");
}

/// Agreement is silence. This is the state of every correctly marked project,
/// and the one the repository itself must be in.
#[test]
fn a_declaration_that_agrees_reports_nothing() {
    let big = holdings(&[("chunk", 503), ("step", 792)]);
    assert_eq!(
        identity_disagreement(Some("demo-abc123"), "demo-abc123", &big),
        None,
        "the declared id and the derived id are the same project; the records are \
         exactly where they belong",
    );
}

/// A difference with nothing behind it detaches nothing. Marking a fresh clone
/// at a new path differs from what that path would derive, and there is no
/// history there to lose — reporting it would train people to ignore the row.
#[test]
fn a_difference_with_no_records_is_not_a_finding() {
    assert_eq!(
        identity_disagreement(Some("shared-identity"), "clone-b-991122", &[]),
        None,
        "no store, nothing detached",
    );
    assert_eq!(
        identity_disagreement(
            Some("shared-identity"),
            "clone-b-991122",
            &holdings(&[("chunk", 0), ("step", 0)]),
        ),
        None,
        "empty stores are not lost history",
    );
}

/// An undeclared project cannot disagree with itself.
#[test]
fn an_undeclared_project_has_nothing_to_disagree_with() {
    assert_eq!(
        identity_disagreement(None, "demo-abc123", &holdings(&[("chunk", 503)])),
        None,
    );
}

/// Only the families holding something are named. A zero row is proof the family
/// was checked, not a thing to put in front of a person.
#[test]
fn empty_families_are_left_out_of_the_finding() {
    let mixed = holdings(&[("chunk", 503), ("signal", 0), ("step", 792)]);
    let found = identity_disagreement(Some("renamed"), "demo-abc123", &mixed).unwrap();
    assert_eq!(
        found.holding,
        holdings(&[("chunk", 503), ("step", 792)]),
        "a family holding nothing has nothing to detach",
    );
    assert_eq!(found.summary(), "503 chunk(s), 792 step(s)");
}
