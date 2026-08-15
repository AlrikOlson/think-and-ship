//! Where the agent's cloud token lives, and how the spawned server finds it.
//!
//! # The problem this solves
//!
//! `connect` used to write the long-lived agent token straight into the MCP
//! config's `env` block. That file is read by an editor, committed by accident,
//! copied into support bundles and pasted into chats — `.mcp.json` and
//! `.cursor/mcp.json` sit in the repo root, and `~/.claude.json` is a
//! cloud-synced home-directory file. A bearer token good for months does not
//! belong in any of them.
//!
//! So the config now carries a PROFILE NAME, which is not a secret, and the
//! token goes to a [`CredentialStore`]. The name is the whole handshake: the CLI
//! writes it on the way out, the spawned server reads it on the way in, and a
//! human looking at the config can see which credential an agent will use
//! without the config being able to leak it.
//!
//! # Which store answers
//!
//! [`store_for`] prefers the OS keychain and falls back to the encrypted file
//! store. The fallback is not a degradation to plaintext — [`FileCredentialStore`]
//! encrypts — it is the answer for a headless container with no keyring and for
//! Windows, which has no CLI to drive. See
//! [`crate::tracker::credential::store`] for what each backend is worth.
//!
//! This choice is scoped to the CLOUD credential on purpose. The tracker's
//! providers keep the file store they were connected under: moving them would
//! strand every Linear and Jira credential already on disk, which is a
//! migration, not a side effect.

use std::path::Path;
use std::sync::Arc;

use crate::tracker::credential::{
    AuthScheme, CredentialStore, FileCredentialStore, KEYCHAIN_SERVICE, KeychainCredentialStore,
    Resolver, StoredCredential,
};

/// The MCP config key naming which stored profile holds the token.
///
/// Not a secret, and that is the point — this is what replaced
/// [`TOKEN_ENV`] in the written config.
pub const PROFILE_ENV: &str = "THINK_AND_SHIP_CLOUD_PROFILE";

/// The long-lived escape hatch: the token supplied directly.
///
/// Still supported and still documented — CI has no keychain and no interactive
/// connect, and `DEPLOY.md` tells operators to set it. It takes PRECEDENCE over
/// a profile, because an operator who sets it has said something explicit.
pub const TOKEN_ENV: &str = "THINK_AND_SHIP_CLOUD_TOKEN";

/// The store key for a profile.
///
/// Namespaced so a cloud profile can never collide with a tracker provider in
/// the same store — `cloud` is not a tracker and `linear` is not a profile, and
/// one flat keyspace holds both.
#[must_use]
pub fn provider_key(profile: &str) -> String {
    format!("cloud-{}", normalize_profile(profile))
}

/// The profile name for this workspace when the human names none.
///
/// The project id, so two projects connected to different tenants do not
/// overwrite each other's token, and disconnecting one does not silently
/// disconnect the rest.
#[must_use]
pub fn default_profile() -> String {
    normalize_profile(&crate::think::config::resolve_project_id())
}

/// Fold a profile name to the form used as a store key and written to config.
///
/// Lowercase, and anything that is not alphanumeric / `-` / `_` becomes `-`, so
/// a profile name is safe as a keychain account, a filename, and a JSON value
/// at once. An empty result becomes `default` rather than an empty key.
#[must_use]
pub fn normalize_profile(profile: &str) -> String {
    let folded: String = profile
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = folded.trim_matches('-');
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The store the cloud credential should use on this machine.
///
/// The keychain when this platform has one AND it answers; the encrypted file
/// store otherwise. Probing rather than assuming is what keeps a headless
/// container working: there, `secret-tool` is absent or D-Bus refuses, and the
/// probe reports it instead of every read failing later.
#[must_use]
pub fn store_for(data_dir: &Path) -> Arc<dyn CredentialStore> {
    match KeychainCredentialStore::for_this_platform(KEYCHAIN_SERVICE) {
        Some(keychain) if keychain.available() => Arc::new(keychain),
        _ => Arc::new(FileCredentialStore::new(data_dir)),
    }
}

/// Persist a freshly minted token under `profile`.
///
/// Routed through [`Resolver::adopt`] rather than calling `save` directly.
/// `adopt` is the single persistence path by design, so everything that lands
/// ends up under the same rotation and revoke machinery instead of a parallel
/// store that nobody remembers to update.
///
/// Re-adopting the same profile REPLACES what was there, which is what makes
/// reconnect and regenerate leave nothing stale behind.
pub fn adopt(
    resolver: &Resolver,
    profile: &str,
    token: &str,
    now: &str,
) -> Result<(), crate::tracker::credential::CredentialError> {
    resolver.adopt(&StoredCredential::personal_key(
        &provider_key(profile),
        token,
        // The agent token is presented as `Authorization: Bearer <token>`;
        // see `CloudClient`.
        AuthScheme::Bearer,
        now,
    ))
}

/// The profile a not-yet-proven credential is written under.
///
/// `connect` used to write the freshly minted token straight onto the real
/// profile and only then try to prove it, and [`adopt`] REPLACES — so every
/// failure after the mint had already destroyed a credential that was working a
/// second earlier. A rejected token did it, and so did a network blip, which is
/// the worse of the two because it needs no server misbehaviour at all.
///
/// Staging separates "stored" from "proven": the new token lands here, is read
/// back, is spent against the backend, and is promoted onto `profile` only once
/// all of that has passed. The staged entry is deleted on every path.
///
/// The suffix collides only with a real profile literally named
/// `<something>-connecting`, which is a name nothing in this codebase mints.
#[must_use]
pub fn staging_profile(profile: &str) -> String {
    format!("{profile}-connecting")
}

/// Read the token stored under `profile`, if any.
///
/// A store that cannot be read is `None` rather than an error: the caller's next
/// move is the same either way — run cloud sync off and say why — and a hard
/// failure here would take the whole server down over a feature that is
/// optional.
#[must_use]
pub fn resolve(store: &dyn CredentialStore, profile: &str) -> Option<String> {
    let stored = store.load(&provider_key(profile)).ok().flatten()?;
    let token = stored.access.expose().trim().to_string();
    if token.is_empty() { None } else { Some(token) }
}

/// Forget the token stored under `profile`. Idempotent, like the trait requires.
pub fn forget(
    store: &dyn CredentialStore,
    profile: &str,
) -> Result<(), crate::tracker::credential::CredentialError> {
    store.delete(&provider_key(profile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::credential::{
        KeychainCommand, KeychainCredentialStore, KeychainDialect, KeychainOutcome, KeychainRunner,
    };
    use std::sync::Mutex;

    /// A keychain that lives in a `HashMap`, driven through the same subprocess
    /// seam the real one uses. Every test in this file and in the store's own
    /// module goes through this — no test touches the developer's keychain.
    #[derive(Default)]
    struct FakeKeychain {
        items: Mutex<std::collections::HashMap<String, String>>,
        /// When set, every invocation reports this on stderr — the shape of a
        /// machine with no usable keyring.
        broken: Option<String>,
    }

    /// The measured size of `security`'s password prompt buffer. Anything longer
    /// arriving on stdin is truncated to this, silently, with exit 0.
    const SECURITY_PROMPT_BUFFER: usize = 128;

    impl FakeKeychain {
        /// The account is the last positional argument for both dialects'
        /// lookups, and follows `-a` on macOS. Reading it back out of the argv
        /// we built is deliberate: it proves the command actually carries it.
        fn account(command: &KeychainCommand) -> String {
            let args = &command.args;
            if let Some(i) = args.iter().position(|a| a == "-a") {
                return args[i + 1].clone();
            }
            let i = args
                .iter()
                .position(|a| a == "account")
                .expect("every command names an account");
            args[i + 1].clone()
        }

        /// The secret a store command is carrying, reproducing how the REAL tool
        /// would read it — truncation included.
        ///
        /// `security` takes it from argv after `-w`. If a future change moves it
        /// back to stdin, this models what actually happens there: the prompt's
        /// 128-byte buffer clips it and reports success. That is why this fake
        /// truncates instead of asserting — an assertion would say "you did it
        /// wrong", where truncation makes the round-trip test fail the same way
        /// the real keychain failed, which is the behaviour worth locking in.
        fn stored_secret(command: &KeychainCommand) -> String {
            if command.program == "security" {
                if let Some(i) = command.args.iter().position(|a| a == "-w")
                    && let Some(value) = command.args.get(i + 1)
                {
                    return value.clone();
                }
                let piped = command.stdin.clone().unwrap_or_default();
                let mut lines = piped.lines();
                let first = lines.next().unwrap_or_default();
                // The prompt asks twice and compares.
                if lines.next().unwrap_or_default() != first {
                    return String::new();
                }
                return first.chars().take(SECURITY_PROMPT_BUFFER).collect();
            }
            // secret-tool reads stdin, with no prompt and no buffer limit.
            command
                .stdin
                .clone()
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        }
    }

    impl KeychainRunner for FakeKeychain {
        fn run(&self, command: &KeychainCommand) -> std::io::Result<KeychainOutcome> {
            if let Some(why) = &self.broken {
                return Ok(KeychainOutcome::new(1, "", why));
            }
            let account = Self::account(command);
            let verb = command.args[0].as_str();
            let mut items = self.items.lock().unwrap();
            match verb {
                "add-generic-password" | "store" => {
                    items.insert(account, Self::stored_secret(command));
                    Ok(KeychainOutcome::new(0, "", ""))
                }
                "find-generic-password" | "lookup" => match items.get(&account) {
                    Some(secret) => Ok(KeychainOutcome::new(0, &format!("{secret}\n"), "")),
                    None if command.program == "security" => Ok(KeychainOutcome::new(44, "", "")),
                    None => Ok(KeychainOutcome::new(0, "", "")),
                },
                "delete-generic-password" | "clear" => {
                    let existed = items.remove(&account).is_some();
                    let code = if existed || command.program != "security" {
                        0
                    } else {
                        44
                    };
                    Ok(KeychainOutcome::new(code, "", ""))
                }
                other => panic!("the fake keychain saw an unknown verb: {other}"),
            }
        }
    }

    fn fake_store(dialect: KeychainDialect) -> Arc<KeychainCredentialStore> {
        Arc::new(KeychainCredentialStore::with_runner(
            "think-and-ship-test",
            dialect,
            Arc::new(FakeKeychain::default()),
        ))
    }

    /// The load-bearing round trip, proven on BOTH dialects rather than only the
    /// one this machine happens to be.
    #[test]
    fn a_token_adopted_under_a_profile_resolves_back_on_every_dialect() {
        for dialect in [KeychainDialect::MacOsSecurity, KeychainDialect::SecretTool] {
            let store = fake_store(dialect);
            let resolver = Resolver::new(store.clone());

            assert_eq!(
                resolve(store.as_ref(), "acme"),
                None,
                "{dialect:?}: nothing is stored yet",
            );

            adopt(&resolver, "acme", "cloud-token-abc", "2026-07-30T00:00:00Z").unwrap();
            assert_eq!(
                resolve(store.as_ref(), "acme").as_deref(),
                Some("cloud-token-abc"),
                "{dialect:?}: the adopted token resolves back",
            );

            // Reconnect: re-adopting REPLACES, so nothing stale survives.
            adopt(&resolver, "acme", "cloud-token-xyz", "2026-07-30T01:00:00Z").unwrap();
            assert_eq!(
                resolve(store.as_ref(), "acme").as_deref(),
                Some("cloud-token-xyz"),
                "{dialect:?}: re-adopting replaces rather than appends",
            );

            // Disconnect, twice — the trait requires idempotency.
            forget(store.as_ref(), "acme").unwrap();
            assert_eq!(
                resolve(store.as_ref(), "acme"),
                None,
                "{dialect:?}: the token is gone after forget",
            );
            forget(store.as_ref(), "acme").expect("forgetting an absent profile is success");
        }
    }

    /// Profiles must not read each other's tokens — the whole reason the key is
    /// namespaced per workspace.
    #[test]
    fn profiles_are_isolated_from_each_other_and_from_tracker_providers() {
        let store = fake_store(KeychainDialect::MacOsSecurity);
        let resolver = Resolver::new(store.clone());

        adopt(&resolver, "acme", "tok-acme", "2026-07-30T00:00:00Z").unwrap();
        adopt(&resolver, "globex", "tok-globex", "2026-07-30T00:00:00Z").unwrap();

        assert_eq!(resolve(store.as_ref(), "acme").as_deref(), Some("tok-acme"));
        assert_eq!(
            resolve(store.as_ref(), "globex").as_deref(),
            Some("tok-globex")
        );

        // Forgetting one leaves the other connected.
        forget(store.as_ref(), "acme").unwrap();
        assert_eq!(resolve(store.as_ref(), "acme"), None);
        assert_eq!(
            resolve(store.as_ref(), "globex").as_deref(),
            Some("tok-globex"),
            "disconnecting one workspace must not disconnect another",
        );

        // And a profile named after a tracker provider still cannot collide
        // with it, because the key is namespaced.
        assert_eq!(provider_key("linear"), "cloud-linear");
        assert_ne!(provider_key("linear"), "linear");
    }

    /// An unusable keyring must be distinguishable from an empty one. If these
    /// collapsed, a headless container would report "not connected yet" and a
    /// human would go re-run connect forever.
    #[test]
    fn an_unusable_keyring_is_an_error_not_an_empty_one() {
        let broken = Arc::new(KeychainCredentialStore::with_runner(
            "think-and-ship-test",
            KeychainDialect::SecretTool,
            Arc::new(FakeKeychain {
                items: Mutex::default(),
                broken: Some("Cannot autolaunch D-Bus without X11 $DISPLAY".into()),
            }),
        ));

        assert!(
            !broken.available(),
            "a keyring that errors on every call is not available",
        );
        let err = broken.load(&provider_key("acme")).unwrap_err();
        assert!(
            err.to_string().contains("D-Bus"),
            "the real reason must survive to the message, got: {err}",
        );
        // And `resolve` softens it to None, because cloud sync is optional and
        // must not take the server down.
        assert_eq!(resolve(broken.as_ref(), "acme"), None);
    }

    /// THE REGRESSION THE LIVE TEST FOUND, held down by a deterministic one.
    ///
    /// An agent token is a JWT — well past the 128 bytes `security`'s password
    /// prompt reads. The stdin path truncated it silently and exited 0, so a
    /// connect would report success and store a corrupted credential that only
    /// failed later, remotely, as a 401 nobody could trace back here.
    ///
    /// This test uses a realistically long token and asserts the round trip is
    /// byte-exact. Moving the secret back onto stdin makes the fake truncate it
    /// exactly as the real tool does, and this goes red.
    #[test]
    fn a_jwt_sized_token_survives_the_round_trip_without_truncation() {
        // The real shape and length: three base64 segments, ~330 bytes.
        let jwt = format!(
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.{}.{}",
            "a".repeat(220),
            "b".repeat(43)
        );
        assert!(
            jwt.len() > 128,
            "precondition: this token must exceed the prompt buffer, or the test \
             proves nothing (len {})",
            jwt.len(),
        );

        for dialect in [KeychainDialect::MacOsSecurity, KeychainDialect::SecretTool] {
            let store = fake_store(dialect);
            let resolver = Resolver::new(store.clone());
            adopt(&resolver, "acme", &jwt, "2026-07-30T00:00:00Z").unwrap();

            let got = resolve(store.as_ref(), "acme")
                .unwrap_or_else(|| panic!("{dialect:?}: the token must resolve back at all"));
            assert_eq!(
                got.len(),
                jwt.len(),
                "{dialect:?}: the token came back {} bytes instead of {} — it was \
                 truncated, which the keychain reports as success",
                got.len(),
                jwt.len(),
            );
            assert_eq!(got, jwt, "{dialect:?}: byte-exact round trip");
        }
    }

    /// A credential CLI that never answers must look like "no keychain here",
    /// not like an empty one and not like a crash.
    ///
    /// `ProcessRunner` kills a child that blows its deadline and reports an
    /// outcome with NO exit code. This asserts what that shape means downstream,
    /// which is the part a real timeout test could not pin without sleeping:
    /// unavailable, so the caller drops to the encrypted file store rather than
    /// leaving the server wedged at startup waiting on an unlock dialog.
    #[test]
    fn a_credential_cli_that_never_answers_reads_as_no_keychain() {
        struct Hung;
        impl KeychainRunner for Hung {
            fn run(&self, _command: &KeychainCommand) -> std::io::Result<KeychainOutcome> {
                // Exactly what ProcessRunner returns after killing a child that
                // outlived its deadline.
                Ok(KeychainOutcome {
                    code: None,
                    stdout: String::new(),
                    stderr: "`security` did not answer within 5s — it may be waiting on a \
                             keychain unlock prompt"
                        .into(),
                })
            }
        }

        for dialect in [KeychainDialect::MacOsSecurity, KeychainDialect::SecretTool] {
            let store = KeychainCredentialStore::with_runner(
                "think-and-ship-test",
                dialect,
                Arc::new(Hung),
            );
            assert!(
                !store.available(),
                "{dialect:?}: a CLI that never answers is not an available keychain",
            );
            let err = store
                .load(&provider_key("acme"))
                .expect_err("{dialect:?}: a hung keychain is an error, not an empty result");
            assert!(
                err.to_string().contains("unlock prompt"),
                "{dialect:?}: the reason must reach the message, got: {err}",
            );
        }
    }

    #[test]
    fn a_profile_name_is_folded_to_something_safe_as_a_key_and_never_empty() {
        assert_eq!(normalize_profile("Acme Corp"), "acme-corp");
        assert_eq!(normalize_profile("  spaced  "), "spaced");
        assert_eq!(normalize_profile("a/b:c"), "a-b-c");
        assert_eq!(normalize_profile("---"), "default");
        assert_eq!(normalize_profile(""), "default");
        // Idempotent, so a name read back out of config and re-normalized is
        // the same key.
        let once = normalize_profile("Acme Corp!");
        assert_eq!(normalize_profile(&once), once);
    }
}
