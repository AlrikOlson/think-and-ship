//! Credential custody — the port that lets an adapter authenticate without
//! learning where its token came from.
//!
//! # The shape
//!
//! ```text
//!   TrackerPort  ──asks──▶  CredentialPort ──▶ Credential { secret, scheme }
//!                                │
//!                                ├── CredentialStore  (where it lives)
//!                                └── OAuth exchange   (how it is renewed)
//! ```
//!
//! An adapter receives a [`domain::Credential`]: a secret and the scheme it must
//! be presented under. It never sees a refresh token, a client secret, or which
//! grant produced any of it — so no adapter can accidentally become responsible
//! for renewal, and renewal stays one thing that happens in one place.
//!
//! # Why the scheme travels with the secret
//!
//! Because a second provider proved it has to. Linear sends a personal API key
//! as a bare `Authorization: <key>` and an OAuth token as
//! `Authorization: Bearer <token>`. A port that returns a `String` forces every
//! adapter to infer the prefix from the token's shape, which is a guess that
//! breaks the first time a provider changes a key format. That finding came out
//! of building the Linear adapter and it is the single most load-bearing thing
//! about this port's signature.
//!
//! # Provider profiles
//!
//! **Jira — OAuth 2.0 (3LO), NOT Forge.** Atlassian Connect is deprecated with
//! apps required on Forge by Q4 2026, and Atlassian steers integrations to
//! "Forge or 3LO". Forge is the wrong shape here for a hosting-model reason, not
//! a preference one: Forge runs your code *inside Atlassian's runtime*, and this
//! is an external Rust service plus a Cloudflare Worker calling the Cloud REST
//! API from its own infrastructure. That is precisely the documented 3LO case.
//! The cost accepted with it: per-site consent, an accessible-resources cloudid
//! lookup before any API call, and refresh-token rotation on every refresh. All
//! three are paid in [`atlassian`] and in the two fields Jira added to
//! [`domain::StoredCredential`] — `client_secret`, because Atlassian requires
//! it on the REFRESH and not only the exchange, and `site`, because a 3LO token
//! is granted to an account rather than to a site and carries no address of its
//! own.
//!
//! **Linear** — OAuth for the product path, personal API key for solo use.
//!
//! **GitHub** — a GitHub App is preferred over a PAT: installation tokens carry
//! finer scoping and a larger rate budget. The registration itself is automated
//! in [`github_app`], which POSTs a manifest so the permission set is decided in
//! code and reviewable in a diff, and takes delivery of the private key, the
//! client secret and the webhook secret in one machine-to-machine exchange —
//! nothing is copied out of a browser. Two clicks stay human and GitHub does not
//! allow otherwise: CREATE the registration, then INSTALL it.
//!
//! See [`store`] for what encryption at rest does and does not defend against;
//! it is stated plainly there rather than implied.

pub mod atlassian;
pub mod authcode;
pub mod domain;
pub mod github_app;
pub mod oauth;
pub mod store;

pub use atlassian::{AccessibleResource, accessible_resources, jira_api_base, select_site};
pub use authcode::{LoopbackReceiver, Pkce, authorize_url, exchange_code, new_state, verify_state};
pub use domain::{AuthScheme, Credential, GrantKind, Secret, StoredCredential};
pub use github_app::{
    AppManifest, DEFAULT_EVENTS, DEFAULT_PERMISSIONS, Owner, Permission, RegisteredApp,
    convert_manifest, manifest_form_html,
};
pub use oauth::{CredentialPort, OAuthClient, OAuthConfig, Resolver};
pub use store::{
    CredentialError, CredentialStore, EnvCredentialStore, FileCredentialStore, KEYCHAIN_SERVICE,
    KeychainCommand, KeychainCredentialStore, KeychainDialect, KeychainOutcome, KeychainRunner,
    ProcessRunner,
};
