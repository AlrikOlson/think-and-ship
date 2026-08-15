//! The connection — what `connect` produced, as an object that outlives the
//! process that produced it.
//!
//! Before this module, a connection was not a thing. It was *implied* by the
//! coincidence of three artifacts that knew nothing about each other:
//!
//!   1. a token in the OS credential store, which holds the secret and NOTHING
//!      else — no url, no workspace, no identity, no host;
//!   2. an `env` block inside ONE MCP config file, which is where the cloud url
//!      and the profile NAME actually lived;
//!   3. the ambient process environment, the only channel from (2) to (1), and
//!      one that exists solely for a process an MCP host spawned.
//!
//! So the MCP config file was the connection database, and exactly one consumer
//! could query it. Four things followed, all of them observed rather than
//! predicted: a human in a shell was never connected (`sync push` failed on
//! BOTH halves at once after a successful `connect`); `status` had nothing to
//! report, because there was no object to read; the failure message advised
//! running `connect`, which was already satisfied and could not help; and
//! writing that one file to the wrong client DESTROYED the connection rather
//! than misrouting it.
//!
//! The split adopted here is the settled one rather than an invention: `gh`
//! keeps the non-secret half (host, user, protocol) in `hosts.yml`, the secret
//! in the OS keyring, and `gh auth status` prints both plus WHICH SOURCE the
//! token came from. wrangler, stripe and fly split the same way.
//!
//! THE RECORD IS NOT A PLACE A SECRET MAY LIVE, and that is load-bearing rather
//! than stylistic: this is a plain file readable by the user, which is exactly
//! why `gh` does not put the token in `hosts.yml` either. The secret stays in
//! the credential store; this file only ever says which store key to ask for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The environment key naming the cloud API base.
///
/// Still the override an operator or a CI job sets, and still the thing that
/// wins — see [`pick`].
pub const URL_ENV: &str = "THINK_AND_SHIP_CLOUD_URL";

/// The file `connect` writes, beside the data dir. Keyed by project, because
/// the credential store is too: `provider_key` namespaces the token as
/// `cloud-<project>`, so two projects connected to different workspaces must
/// not overwrite each other here either.
const FILE_NAME: &str = "connections.json";

/// Written into every record so a future format change can recognise this one.
const FILE_VERSION: u32 = 1;

/// Where a resolved value came from.
///
/// Carried rather than discarded because `status` has to be able to say it.
/// "You are connected" and "you are connected BECAUSE this env var is set" are
/// different facts, and a user debugging a machine that behaves differently
/// under an MCP host than in their own shell needs the second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An explicit environment variable. Always wins.
    Environment,
    /// The connection record `connect` wrote.
    Stored,
}

impl Source {
    /// How to name this source to a human, mid-sentence.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Source::Environment => "the environment",
            Source::Stored => "the stored connection",
        }
    }
}

/// One machine-local connection to a cloud workspace.
///
/// Everything here is non-secret by construction. If a field ever needs to hold
/// a secret, it belongs in the credential store instead and this record should
/// name it, exactly as `profile` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// The cloud API base this project syncs to.
    pub cloud_url: String,
    /// The credential-store profile holding the token. A NAME, never a token.
    pub profile: String,
    /// When `connect` proved this connection, ISO-8601.
    pub connected_at: String,
}

/// The on-disk shape: a map from project id to connection, like `hosts.yml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ConnectionFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    connections: BTreeMap<String, Connection>,
}

/// Where the record lives for a given data dir.
#[must_use]
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Read the whole file. A missing or unreadable file is an empty map, never an
/// error: a machine that has never connected is a valid machine, and a
/// corrupted record must not take down a command that would otherwise work from
/// the environment.
fn read_file(data_dir: &Path) -> ConnectionFile {
    std::fs::read_to_string(path_in(data_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_file(data_dir: &Path, file: &ConnectionFile) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let text = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    std::fs::write(path_in(data_dir), text)
}

/// The connection recorded for `project_id`, if any.
#[must_use]
pub fn load_in(data_dir: &Path, project_id: &str) -> Option<Connection> {
    read_file(data_dir).connections.remove(project_id)
}

/// Record a proven connection for `project_id`, replacing any earlier one.
///
/// Replaces rather than merges for the same reason `adopt` replaces the stored
/// token: re-connecting is how a user fixes a connection, so a retry must leave
/// exactly one live answer behind.
pub fn save_in(data_dir: &Path, project_id: &str, connection: &Connection) -> std::io::Result<()> {
    let mut file = read_file(data_dir);
    file.version = FILE_VERSION;
    file.connections
        .insert(project_id.to_string(), connection.clone());
    write_file(data_dir, &file)
}

/// Forget the connection for `project_id`. Returns whether one was there.
///
/// Idempotent, because `disconnect` must be safe to run on a machine that is
/// already disconnected — the same posture `CredentialStore::delete` holds.
pub fn forget_in(data_dir: &Path, project_id: &str) -> std::io::Result<bool> {
    let mut file = read_file(data_dir);
    let had = file.connections.remove(project_id).is_some();
    if had {
        file.version = FILE_VERSION;
        write_file(data_dir, &file)?;
    }
    Ok(had)
}

/// Apply the precedence rule to one value.
///
/// PURE, and separated from every ambient read on purpose: this is the rule the
/// whole module exists to state, and a rule that can only be exercised by
/// mutating process environment is a rule no test can hold still.
///
/// An explicit environment value is never second-guessed by a stored record.
/// That ordering is documented in `DEPLOY.md` and CI depends on it — an
/// operator who sets the variable has said something explicit, and a stored
/// record from a previous interactive `connect` must not quietly outrank it.
/// An empty or whitespace-only environment value is treated as unset, because
/// `FOO=` in a shell profile means "I turned this off", not "sync to the empty
/// string".
#[must_use]
pub fn pick(env_value: Option<&str>, stored: Option<&str>) -> Option<(String, Source)> {
    if let Some(value) = env_value {
        let value = value.trim();
        if !value.is_empty() {
            return Some((value.to_string(), Source::Environment));
        }
    }
    let value = stored?.trim();
    if value.is_empty() {
        return None;
    }
    Some((value.to_string(), Source::Stored))
}

/// The data dir the record lives beside (respects `THINK_AND_SHIP_DATA_DIR`).
#[must_use]
pub fn data_dir() -> PathBuf {
    crate::infra::PersistenceConfig::from_env().data_dir
}

/// This project's id — the key the record is filed under.
#[must_use]
pub fn project_id() -> String {
    crate::think::config::resolve_project_id()
}

/// The connection recorded for the project in the current working directory.
#[must_use]
pub fn load() -> Option<Connection> {
    load_in(&data_dir(), &project_id())
}

/// Record a proven connection for the project in the current working directory.
pub fn save(connection: &Connection) -> std::io::Result<()> {
    save_in(&data_dir(), &project_id(), connection)
}

/// Forget the connection for the project in the current working directory.
pub fn forget() -> std::io::Result<bool> {
    forget_in(&data_dir(), &project_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn conn(url: &str, profile: &str) -> Connection {
        Connection {
            cloud_url: url.to_string(),
            profile: profile.to_string(),
            connected_at: "2026-07-31T23:00:00Z".to_string(),
        }
    }

    /// ACCEPTANCE, and the gate that was missing: with NO environment at all, a
    /// stored connection still resolves.
    ///
    /// This is the exact shape of the reported failure — `sync push` in a plain
    /// shell after a successful `connect` — and nothing in the suite could see
    /// it, because every test of this lane supplied the environment an MCP host
    /// would have injected. The MCP lane was silently standing in for the shell
    /// lane.
    #[test]
    fn an_empty_environment_still_resolves_a_stored_connection() {
        let stored = conn("https://api.example", "acme-1234");

        let (url, source) = pick(None, Some(&stored.cloud_url)).expect("url resolves with no env");
        assert_eq!(url, "https://api.example");
        assert_eq!(source, Source::Stored);

        let (profile, source) = pick(None, Some(&stored.profile)).expect("profile resolves");
        assert_eq!(profile, "acme-1234");
        assert_eq!(source, Source::Stored);
    }

    /// The documented precedence, in both directions: an explicit environment
    /// value wins, and an empty one is treated as unset rather than as an
    /// instruction to sync to the empty string.
    #[test]
    fn an_explicit_environment_value_is_never_second_guessed() {
        assert_eq!(
            pick(Some("https://from-env"), Some("https://from-store")),
            Some(("https://from-env".to_string(), Source::Environment)),
            "an operator who set the variable said something explicit",
        );
        assert_eq!(
            pick(Some("   "), Some("https://from-store")),
            Some(("https://from-store".to_string(), Source::Stored)),
            "FOO= means 'off', not 'sync to the empty string'",
        );
        assert_eq!(pick(None, None), None, "neither source is not an error");
        assert_eq!(pick(Some(""), Some("  ")), None);
    }

    /// Save → load → forget, per project, with the second project untouched.
    ///
    /// Keyed by project because the credential store is: two projects connected
    /// to different workspaces must not overwrite each other, and disconnecting
    /// one must not silently disconnect the rest.
    #[test]
    fn a_connection_round_trips_and_is_scoped_to_its_project() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        assert_eq!(load_in(dir, "alpha"), None, "never connected");
        assert!(
            !forget_in(dir, "alpha").unwrap(),
            "forgetting is idempotent"
        );

        save_in(dir, "alpha", &conn("https://alpha.example", "alpha-1")).unwrap();
        save_in(dir, "beta", &conn("https://beta.example", "beta-1")).unwrap();

        assert_eq!(
            load_in(dir, "alpha").unwrap().cloud_url,
            "https://alpha.example"
        );
        assert_eq!(load_in(dir, "beta").unwrap().profile, "beta-1");

        // Re-connecting replaces rather than accumulating.
        save_in(dir, "alpha", &conn("https://alpha-2.example", "alpha-2")).unwrap();
        assert_eq!(load_in(dir, "alpha").unwrap().profile, "alpha-2");

        assert!(forget_in(dir, "alpha").unwrap());
        assert_eq!(load_in(dir, "alpha"), None);
        assert_eq!(
            load_in(dir, "beta").unwrap().profile,
            "beta-1",
            "disconnecting one project must not disconnect the rest",
        );
    }

    /// The record must never carry a secret. A plain user-readable file is the
    /// wrong home for a token, which is the whole reason the profile is a NAME.
    #[test]
    fn the_serialized_record_carries_no_secret() {
        let tmp = TempDir::new().unwrap();
        save_in(
            tmp.path(),
            "alpha",
            &conn("https://alpha.example", "alpha-1"),
        )
        .unwrap();
        let text = std::fs::read_to_string(path_in(tmp.path())).unwrap();

        for forbidden in ["token", "secret", "access", "bearer", "password"] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "the connection record must stay non-secret; found {forbidden:?} in:\n{text}",
            );
        }
        assert!(text.contains("\"version\": 1"), "the format is versioned");
    }

    /// A corrupted or truncated record must not take down a command that could
    /// still work from the environment. Absence and garbage are the same answer.
    #[test]
    fn an_unreadable_record_reads_as_no_connection() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(path_in(tmp.path()), "{ this is not json").unwrap();
        assert_eq!(load_in(tmp.path(), "alpha"), None);
        // And it is still writable afterwards — a bad file is replaced, not fatal.
        save_in(tmp.path(), "alpha", &conn("https://alpha.example", "a")).unwrap();
        assert_eq!(load_in(tmp.path(), "alpha").unwrap().profile, "a");
    }
}
