//! Cloud sync configuration: where the URL and the token come from at startup.

use crate::cloud::client::CloudClient;
use crate::cloud::connection::{Connection, Source, URL_ENV, pick};
use crate::cloud::credential::{PROFILE_ENV, TOKEN_ENV};
use crate::tracker::credential::CredentialStore;

/// Build a [`CloudClient`] for this project.
///
/// Returns `None` — cloud sync off, the default — unless a URL and a token both
/// resolve.
///
/// EACH HALF HAS TWO SOURCES, and the order between them is the same rule twice:
///
/// 1. The explicit environment variable, when set. An operator or a CI job that
///    exports one has said something explicit, and it must not be second-guessed
///    by a stored record. `DEPLOY.md` documents this path.
/// 2. Otherwise the connection [`connect`](crate::cli::connect) recorded — the
///    cloud url, and the profile NAME whose token lives in the credential store.
///
/// THE SECOND SOURCE IS WHY THIS FUNCTION EXISTS IN THIS SHAPE. It used to read
/// the url from `std::env` with no fallback at all, which made the MCP config's
/// `env` block the only place a connection was recorded — so a process an MCP
/// host had NOT spawned (a human running `sync push` in a shell) was never
/// connected, no matter how many times `connect` had succeeded. There is now one
/// resolver and both callers use it: if the CLI and the spawned server resolved
/// by different code, that defect would simply reappear in the gap between them.
///
/// Neither half resolving is cloud sync off, which is a valid state, not an error.
#[must_use]
pub fn client_from_env() -> Option<CloudClient> {
    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let store = crate::cloud::credential::store_for(&data_dir);
    client_from_env_with(store.as_ref())
}

/// [`client_from_env`] with the credential store supplied, so a test can prove
/// the profile hop without a real keychain and without touching the developer's.
#[must_use]
pub fn client_from_env_with(store: &dyn CredentialStore) -> Option<CloudClient> {
    client_with(
        store,
        &EnvOverrides::from_env(),
        crate::cloud::connection::load().as_ref(),
    )
}

/// The three environment overrides, as a value.
///
/// They are read in exactly one place ([`EnvOverrides::from_env`]) so that every
/// function below takes them as data. A precedence rule reachable only by
/// mutating process-global environment is a rule no test can hold still while
/// its neighbours run — and this rule is the whole point of the module.
#[derive(Debug, Default, Clone)]
pub struct EnvOverrides {
    pub url: Option<String>,
    pub token: Option<String>,
    pub profile: Option<String>,
}

impl EnvOverrides {
    /// The ONLY ambient environment read in this lane.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            url: std::env::var(URL_ENV).ok(),
            token: std::env::var(TOKEN_ENV).ok(),
            profile: std::env::var(PROFILE_ENV).ok(),
        }
    }
}

/// [`client_from_env_with`] with the environment and the record supplied.
///
/// This is the composition the CLI and the spawned server both run, with nothing
/// ambient left in it — so a test can drive the exact shape of the reported bug:
/// an EMPTY environment, a populated store, and a recorded connection.
#[must_use]
pub fn client_with(
    store: &dyn CredentialStore,
    env: &EnvOverrides,
    stored: Option<&Connection>,
) -> Option<CloudClient> {
    let (url, _) = resolve_url(env, stored)?;
    let (token, _) = resolve_token(store, env, stored)?;
    // SEP-414 downstream half: the push joins the caller's tree when one was
    // adopted, and is header-free when none was.
    Some(
        CloudClient::new(url, token)
            .with_trace_context(&crate::think::config::resolve_project_id()),
    )
}

/// The cloud API base: the explicit environment override, else the recorded
/// connection. The [`Source`] is carried rather than dropped so `status` can say
/// which one answered.
#[must_use]
pub fn resolve_url(env: &EnvOverrides, stored: Option<&Connection>) -> Option<(String, Source)> {
    pick(env.url.as_deref(), stored.map(|c| c.cloud_url.as_str()))
}

/// The profile whose token this project spends: the explicit environment
/// override, else the recorded connection.
#[must_use]
pub fn resolve_profile(
    env: &EnvOverrides,
    stored: Option<&Connection>,
) -> Option<(String, Source)> {
    pick(env.profile.as_deref(), stored.map(|c| c.profile.as_str()))
}

/// The token: the explicit environment override, else the profile's entry in the
/// credential store.
///
/// The token override is checked FIRST and independently of the profile, because
/// `THINK_AND_SHIP_CLOUD_TOKEN` is the CI path — a machine with no keychain and
/// no interactive connect — and there the profile is meaningless.
#[must_use]
pub fn resolve_token(
    store: &dyn CredentialStore,
    env: &EnvOverrides,
    stored: Option<&Connection>,
) -> Option<(String, Source)> {
    if let Some(token) = env.token.as_deref() {
        let token = token.trim();
        if !token.is_empty() {
            return Some((token.to_string(), Source::Environment));
        }
    }
    let (profile, source) = resolve_profile(env, stored)?;
    crate::cloud::credential::resolve(store, &profile).map(|token| (token, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::credential::{adopt, provider_key};
    use crate::tracker::credential::{
        CredentialError, FileCredentialStore, Resolver, StoredCredential,
    };
    use std::sync::Arc;
    use tempfile::TempDir;

    /// A store that refuses everything — the headless-container shape.
    struct UnusableStore;

    impl CredentialStore for UnusableStore {
        fn load(&self, _provider: &str) -> Result<Option<StoredCredential>, CredentialError> {
            Err(CredentialError::Invalid("no keyring here".into()))
        }
        fn save(&self, _credential: &StoredCredential) -> Result<(), CredentialError> {
            Err(CredentialError::Invalid("no keyring here".into()))
        }
        fn delete(&self, _provider: &str) -> Result<(), CredentialError> {
            Ok(())
        }
        fn providers(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// ACCEPTANCE: the spawned server resolves its token from the named profile.
    ///
    /// Parameterized on the store rather than mutating the environment for it,
    /// so this proves the profile hop without a real keychain.
    #[test]
    fn a_profile_resolves_to_the_stored_token() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(tmp.path()));
        let resolver = Resolver::new(store.clone());
        adopt(
            &resolver,
            "acme",
            "tok-from-the-store",
            "2026-07-30T00:00:00Z",
        )
        .unwrap();

        assert_eq!(
            crate::cloud::credential::resolve(store.as_ref(), "acme").as_deref(),
            Some("tok-from-the-store"),
            "the profile the config names is what the store answers",
        );
        // The key the server reads is the key connect wrote.
        assert!(store.load(&provider_key("acme")).unwrap().is_some());
    }

    fn connection(url: &str, profile: &str) -> Connection {
        Connection {
            cloud_url: url.to_string(),
            profile: profile.to_string(),
            connected_at: "2026-07-31T23:00:00Z".to_string(),
        }
    }

    /// ACCEPTANCE (connection-is-a-first-class-object), and the gate whose
    /// absence let this ship: with an EMPTY environment, a recorded connection
    /// plus a stored token is enough to build a client.
    ///
    /// This is the reported failure exactly. `sync push` in a plain shell, on a
    /// machine where `connect` had already succeeded, could not build a client,
    /// because the url and the profile name lived ONLY in an MCP config file's
    /// `env` block — injected into a process the MCP host spawned, and into
    /// nothing else. Every existing test of this lane supplied that environment,
    /// so the MCP lane was silently standing in for the shell lane and no test
    /// in the suite could see the difference.
    ///
    /// Note what is deliberately NOT tested through `client_from_env_with`: that
    /// function reads the process environment, which a sibling test could be
    /// mutating. The composition is exercised here with the environment passed
    /// as a value, so the rule holds still while its neighbours run.
    #[test]
    fn an_empty_environment_builds_a_client_from_the_recorded_connection() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(tmp.path()));
        let resolver = Resolver::new(store.clone());
        adopt(
            &resolver,
            "acme",
            "tok-from-the-store",
            "2026-07-30T00:00:00Z",
        )
        .unwrap();

        let recorded = connection("https://api.example", "acme");
        let empty = EnvOverrides::default();

        let client = client_with(store.as_ref(), &empty, Some(&recorded))
            .expect("a recorded connection plus a stored token is a usable client");
        assert_eq!(client.base_url(), "https://api.example");

        // And the half that proves it came from the RECORD rather than anywhere
        // else: drop the record and the same call answers nothing.
        assert!(
            client_with(store.as_ref(), &empty, None).is_none(),
            "with no environment and no record there is nothing to connect to",
        );
    }

    /// The documented precedence survives the refactor: an explicit environment
    /// value still outranks the recorded connection, in both halves.
    ///
    /// CI depends on this — `DEPLOY.md` tells operators to export the token, and
    /// a stored record from someone's earlier interactive `connect` must never
    /// quietly outrank what they exported.
    #[test]
    fn an_explicit_environment_still_outranks_the_recorded_connection() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(tmp.path()));
        let resolver = Resolver::new(store.clone());
        adopt(
            &resolver,
            "acme",
            "tok-from-the-store",
            "2026-07-30T00:00:00Z",
        )
        .unwrap();

        let recorded = connection("https://recorded.example", "acme");
        let env = EnvOverrides {
            url: Some("https://from-env.example".to_string()),
            token: Some("tok-from-env".to_string()),
            profile: None,
        };

        let (url, source) = resolve_url(&env, Some(&recorded)).unwrap();
        assert_eq!(url, "https://from-env.example");
        assert_eq!(source, Source::Environment);

        let (token, source) = resolve_token(store.as_ref(), &env, Some(&recorded)).unwrap();
        assert_eq!(token, "tok-from-env");
        assert_eq!(source, Source::Environment);

        // The CI shape: a token in the environment works with NO record and no
        // keychain profile at all, which is the whole reason it is checked first.
        let (token, _) = resolve_token(store.as_ref(), &env, None).unwrap();
        assert_eq!(token, "tok-from-env");
    }

    /// THE ABSENCE, held structurally: nothing in the resolving lane may reach
    /// for an MCP config file, now or later.
    ///
    /// The behavioural half of this rule lives in `cli::setup` — a legacy config
    /// on disk plus a token in the store still resolves to no client. But a
    /// behavioural test can only catch a fallback someone already wrote; the
    /// hazard here is the one that gets ADDED, quietly, the next time a machine
    /// turns up in the pre-record state and reading its config looks like the
    /// obvious fix. It is not: an MCP config readable by one consumer is exactly
    /// what made the config the connection database, made a shell permanently
    /// unconnected, and made a write to the wrong client destroy a connection
    /// instead of misrouting it.
    ///
    /// So the boundary is the gate. Migration is a `cli` verb that reads a config
    /// ONCE and leaves a record; resolution takes three plain values and owns no
    /// filesystem. Doc links across the boundary are fine and are excluded —
    /// what may not exist is code.
    #[test]
    fn no_resolving_path_can_reach_an_mcp_config() {
        for (module, source) in [
            ("cloud/config.rs", include_str!("config.rs")),
            ("cloud/connection.rs", include_str!("connection.rs")),
        ] {
            // Production half only. The test module has to be able to NAME the
            // forbidden strings in order to forbid them.
            let code: String = source
                .split("#[cfg(test)]")
                .next()
                .expect("split always yields a first segment")
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for forbidden in [
                // The config search, the entry reader, and the migration itself.
                "crate::cli",
                // Any direct reach for a client's config by name.
                "mcpServers",
                ".mcp.json",
                ".claude.json",
            ] {
                assert!(
                    !code.contains(forbidden),
                    "{module} names `{forbidden}` in code: resolution must never \
                     read an MCP config — that coupling is what 0ce19ce removed",
                );
            }
        }
    }

    /// A profile that was never connected resolves to nothing, and an unusable
    /// store also resolves to nothing rather than panicking — cloud sync is
    /// optional and must never take the server down.
    #[test]
    fn an_absent_profile_and_an_unusable_store_both_mean_no_token() {
        let tmp = TempDir::new().unwrap();
        let store = FileCredentialStore::new(tmp.path());
        assert_eq!(
            crate::cloud::credential::resolve(&store, "never-connected"),
            None
        );
        assert_eq!(
            crate::cloud::credential::resolve(&UnusableStore, "acme"),
            None
        );
    }
}
