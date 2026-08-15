//! Per-project projection consent — the machine-local half of "opt-in".
//!
//! Projection is opt-in per chunk AND per project, and the two halves live in
//! different places on purpose. The per-chunk half
//! ([`crate::roadmap::domain::TrackerOptIn`]) is roadmap state, because which
//! chunks are in scope is a decision about the plan and has to travel to every
//! machine. This half does not travel: it names a provider and a destination
//! (`owner/repo`) that are specific to one checkout, and sits next to the
//! credential-custody secret that authorizes the write.
//!
//! The shape is deliberately borrowed from [`crate::telemetry::consent`], which
//! is this crate's other human-decided opt-in, down to the two properties that
//! matter: a missing or corrupt file loads as DISABLED rather than erroring, so
//! the failure mode is silence; and there is a single predicate
//! ([`should_project`]) that answers "may anything leave?", so the default-off
//! guarantee is one function rather than a condition spread across call sites.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::infra::persistence::locked_merge_write;

/// How the current setting came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Never decided — the default, and disabled.
    Default,
    /// A human ran `tracker enable` / `tracker disable`.
    Explicit,
}

/// A SECOND destination that runs alongside the primary one, and takes its
/// item identities from it.
///
/// # Why this exists, and why it is not just a second `provider`
///
/// [`TrackerConfig`] names exactly ONE provider, and `enable` overwrites it.
/// Every consumer reads that single value — the push, the sweep, `include`,
/// `preview`, the receipt, and both doorbell guards. For every provider that
/// can CREATE its own items that is the whole story: switching destinations
/// means switching destinations.
///
/// A GitHub Projects v2 board is not such a provider. A board item wraps
/// content that already exists, so `projects_v2` is patch-only by construction
/// and refuses any item whose `external_id` is `None`
/// (projects-v2-board-link-seeding). Under a single-provider config that makes
/// the board unusable in BOTH directions: pointed at it, a project stops
/// mirroring to Issues, so the issue's own title, body and state stop tracking
/// the plan while the board's Status column starts to — two half-truths where
/// there was one whole one; and every chunk is refused anyway, because the
/// board has no link records of its own and nothing produces the first.
///
/// So the board is not a destination you switch TO. It is a companion to one,
/// and this field is the only place that fact can be stated. Naming the lane is
/// deliberately explicit rather than inferred: the rejected alternative was a
/// `cannot_create` capability flag on [`super::TrackerCapabilities`], which
/// would have added a field to a four-adapter contract in order to hold a
/// guess, and then guessed again about which sibling to read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionLane {
    /// The second provider key. MUST differ from [`TrackerConfig::provider`] —
    /// two lanes under one key would put two link records in a fight over one
    /// `external_id`, which is the distinction the registry gate
    /// `the_board_is_reachable_through_the_one_lookup_and_is_not_the_issues_provider`
    /// exists to hold.
    pub provider: String,
    /// Where the companion writes, in whatever form ITS provider parses — a
    /// board is addressed by project URL, not by `owner/repo`, so a bare
    /// provider key would be unusable. Opaque here for the same reason
    /// [`TrackerConfig::target`] is.
    pub target: String,
}

/// The persisted per-project tracker configuration. The default projects
/// nothing, to nowhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerConfig {
    pub enabled: bool,
    /// Lowercase provider key — `"github"`. `None` while undecided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Where the provider should write, in whatever form that provider parses
    /// (`owner/repo` for GitHub). Opaque here: the core does not know what a
    /// repository is, which is what keeps a provider out of the core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// ISO-8601 moment of the explicit decision; `None` while defaulted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    /// The human-readable name of the ROOF the whole roadmap files under (a
    /// Linear *initiative*). THE NAMING DECISION, in writing: `None` — the
    /// overwhelmingly common case — falls back to the project directory's
    /// basename (`think-and-ship`), because that is what a human would have
    /// typed; the derived project id (`think-and-ship-676f38`) is NEVER used,
    /// because a hash suffix in a workspace sidebar is a bug report waiting to
    /// be filed. Set this when one roadmap spans several directories or the
    /// directory name is not the product's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<String>,
    /// A second lane that runs after the primary one and inherits its item
    /// identities. `None` — every project that will ever run this binary except
    /// the ones mirroring to a board — means one lane, exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<CompanionLane>,
    pub source: ConfigSource,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            target: None,
            decided_at: None,
            initiative: None,
            companion: None,
            source: ConfigSource::Default,
        }
    }
}

/// Tracker configuration persistence error.
#[derive(Debug, thiserror::Error)]
pub enum TrackerConfigError {
    #[error("tracker config io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// Scoped by project id, unlike telemetry consent: two checkouts on one machine
/// project to different repositories, and one enabling the other is a surprise.
fn config_path(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir.join("tracker").join(format!("{project_id}.json"))
}

/// Load this project's configuration. A missing or unreadable file is the
/// default (disabled) — never an error, so a broken file can only fail CLOSED.
#[must_use]
pub fn load(data_dir: &Path, project_id: &str) -> TrackerConfig {
    std::fs::read_to_string(config_path(data_dir, project_id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Turn projection on for this project. `now` is ISO-8601. Concurrent writers
/// merge newest-`decided_at`-wins, matching the telemetry rule.
pub fn enable(
    data_dir: &Path,
    project_id: &str,
    provider: &str,
    target: &str,
    now: &str,
) -> Result<TrackerConfig, TrackerConfigError> {
    let provider = provider.trim().to_ascii_lowercase();
    let target = target.trim().to_string();
    if provider.is_empty() {
        return Err(TrackerConfigError::Invalid(
            "enabling projection needs a provider".to_string(),
        ));
    }
    if target.is_empty() {
        return Err(TrackerConfigError::Invalid(
            "enabling projection needs a destination for the provider to write to".to_string(),
        ));
    }
    // Carry the fields this decision is NOT about — a re-setup must not
    // silently discard an initiative name a human chose, the courtesy
    // `disable` already extends to provider and target.
    let prior = load(data_dir, project_id);
    write(
        data_dir,
        project_id,
        TrackerConfig {
            enabled: true,
            provider: Some(provider),
            target: Some(target),
            decided_at: Some(now.to_string()),
            source: ConfigSource::Explicit,
            ..prior
        },
    )
}

/// Name the initiative roof, or leave it alone.
///
/// The reachable surface for the escape hatch 10b left edit-the-JSON only
/// (tracker-initiative-name-reachable). A blank or whitespace-only name is a
/// no-op rather than an error or a clearing: `--initiative ""` must not mint a
/// roof named `""`, and this setter deliberately cannot RESET to the
/// directory-basename default — deleting the field from the JSON stays the one
/// way to do that, stated here so the asymmetry is a decision and not a gap.
pub fn set_initiative(
    data_dir: &Path,
    project_id: &str,
    name: &str,
    now: &str,
) -> Result<TrackerConfig, TrackerConfigError> {
    let name = name.trim();
    let prior = load(data_dir, project_id);
    if name.is_empty() {
        return Ok(prior);
    }
    write(
        data_dir,
        project_id,
        TrackerConfig {
            initiative: Some(name.to_string()),
            decided_at: Some(now.to_string()),
            source: ConfigSource::Explicit,
            ..prior
        },
    )
}

/// Name the companion lane, or clear it.
///
/// A blank `provider` CLEARS the lane — unlike [`set_initiative`], which
/// deliberately cannot reset, because there the blank case is a slip
/// (`--initiative ""`) and here it is the only way to undo a two-lane decision
/// without editing JSON. A lane that names the primary provider is refused at
/// the door rather than stored and refused later, so the invalid state is not
/// representable on disk in the first place.
pub fn set_companion(
    data_dir: &Path,
    project_id: &str,
    provider: &str,
    target: &str,
    now: &str,
) -> Result<TrackerConfig, TrackerConfigError> {
    let provider = provider.trim().to_ascii_lowercase();
    let target = target.trim().to_string();
    let prior = load(data_dir, project_id);
    let companion = if provider.is_empty() {
        None
    } else {
        if target.is_empty() {
            return Err(TrackerConfigError::Invalid(
                "a companion lane needs a destination of its own — a board is addressed by \
                 project URL, not by the primary provider's target"
                    .to_string(),
            ));
        }
        if prior.provider.as_deref() == Some(provider.as_str()) {
            return Err(TrackerConfigError::Invalid(same_key_refusal(&provider)));
        }
        Some(CompanionLane { provider, target })
    };
    write(
        data_dir,
        project_id,
        TrackerConfig {
            companion,
            decided_at: Some(now.to_string()),
            source: ConfigSource::Explicit,
            ..prior
        },
    )
}

/// The ONE message for a lane that names the primary provider, so the refusal
/// reads the same wherever it is reached from.
fn same_key_refusal(provider: &str) -> String {
    format!(
        "the companion lane cannot be '{provider}' — that is already this project's provider, and \
         two lanes under one key would put two link records in a fight over one external id"
    )
}

/// The companion lane, if this project has a usable one.
///
/// `Ok(None)` is the overwhelmingly common answer and means one lane. `Err` is
/// a lane that exists and cannot be honoured — a hand-edited config naming the
/// primary provider, or one with no destination. That case is an ERROR rather
/// than a silent `None` on purpose: a companion configured and then quietly
/// ignored is a push that looks like it worked and mirrored half of what the
/// human asked for, which is the exact failure mode this whole chunk exists to
/// remove.
pub fn companion_lane(config: &TrackerConfig) -> Result<Option<&CompanionLane>, String> {
    let Some(lane) = config.companion.as_ref() else {
        return Ok(None);
    };
    if lane.provider.trim().is_empty() || lane.target.trim().is_empty() {
        return Err("the companion lane needs a provider and a destination".to_string());
    }
    if config.provider.as_deref() == Some(lane.provider.as_str()) {
        return Err(same_key_refusal(&lane.provider));
    }
    Ok(Some(lane))
}

/// Turn projection off, keeping the provider and target so re-enabling does not
/// require retyping them.
pub fn disable(
    data_dir: &Path,
    project_id: &str,
    now: &str,
) -> Result<TrackerConfig, TrackerConfigError> {
    let prior = load(data_dir, project_id);
    write(
        data_dir,
        project_id,
        TrackerConfig {
            enabled: false,
            decided_at: Some(now.to_string()),
            source: ConfigSource::Explicit,
            ..prior
        },
    )
}

fn write(
    data_dir: &Path,
    project_id: &str,
    state: TrackerConfig,
) -> Result<TrackerConfig, TrackerConfigError> {
    locked_merge_write(&config_path(data_dir, project_id), &state, |ours, disk| {
        if disk.decided_at.as_deref() > ours.decided_at.as_deref() {
            disk
        } else {
            ours.clone()
        }
    })?;
    Ok(load(data_dir, project_id))
}

/// THE egress predicate: nothing leaves unless a human explicitly enabled this
/// project AND named a provider AND named a destination.
///
/// Every one of those is required rather than merely checked, because the
/// guarantee this module owes is that upgrading cannot fill anyone's tracker.
/// An enabled flag with no target is a half-finished decision, not consent.
#[must_use]
pub fn should_project(config: &TrackerConfig) -> bool {
    config.enabled
        && config.provider.as_ref().is_some_and(|p| !p.is_empty())
        && config.target.as_ref().is_some_and(|t| !t.is_empty())
}

/// Whether a chunk born RIGHT NOW inherits this project's opt-in, and into
/// which provider. `None` — the answer for almost every project that will ever
/// run this binary — means the chunk is born silent, exactly as before.
///
/// # This reverses a documented default, so here is the argument
///
/// [`crate::roadmap::domain::TrackerOptIn`] says silence is the default, and the
/// reason it gives is precise: *nobody's tracker fills up because they upgraded*.
/// Held literally, that rule also means a project whose owner deliberately
/// connected a tracker in January is still, in July, mirroring only the forty
/// chunks that existed on the day they ran `tracker setup` — because nothing in
/// the workflow ever grows the set. That is what actually happened here
/// (`tracker-optin-never-grows`): 46 of 354 chunks were in scope, and the ones
/// missing were precisely the current work. A plan that mirrors only its own
/// past is worse than one that mirrors nothing, because it looks like it works.
///
/// The reversal is safe because the reason survives it intact. The guarantee is
/// owed to people who never decided — and this predicate is strictly stronger
/// than [`should_project`], the egress rule: on top of enabled + provider +
/// target it additionally requires [`ConfigSource::Explicit`], a human having
/// run `tracker on` / `tracker setup`, recorded with a `decided_at` timestamp.
/// A config that merely *has* a provider and a target — written by a migration,
/// a template, a copied data dir, or some future default — inherits nothing.
/// Upgrading still cannot fill a stranger's tracker; what changed is only that
/// a person who already said yes does not have to keep saying it.
///
/// The blast radius is bounded twice more, in the caller rather than here:
/// inheritance is applied at chunk BIRTH only ([`crate::roadmap::engine`]'s
/// `add_chunk`), so there is no code path that could retroactively sweep the
/// chunks that already exist — the ~20 unrequested issues that would have
/// created were the whole reason a bulk top-up verb was declined; and a chunk
/// explicitly opted OUT stays out, because opting out is recorded rather than
/// deleted and birth is the only moment this fires.
///
/// Kept pure and separate from [`load`] for the reason the usage counter's
/// `counting_enabled` is: a decision reachable only through a filesystem read is
/// a decision no test can interrogate.
#[must_use]
pub fn inherited_opt_in(config: &TrackerConfig) -> Option<&str> {
    if !should_project(config) || config.source != ConfigSource::Explicit {
        return None;
    }
    config.provider.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The load-bearing default. If this ever flips, upgrading the binary starts
    /// writing to people's issue trackers.
    #[test]
    fn a_fresh_project_projects_nothing() {
        let dir = TempDir::new().expect("tempdir");
        let cfg = load(dir.path(), "proj");
        assert!(!cfg.enabled);
        assert_eq!(cfg.source, ConfigSource::Default);
        assert!(!should_project(&cfg));
    }

    #[test]
    fn enable_persists_provider_and_target() {
        let dir = TempDir::new().expect("tempdir");
        let cfg = enable(
            dir.path(),
            "proj",
            "GitHub",
            "owner/repo",
            "2026-07-25T09:00:00Z",
        )
        .expect("enable");
        // The provider key folds to lowercase, as everywhere else in the seam.
        assert_eq!(cfg.provider.as_deref(), Some("github"));
        assert_eq!(cfg.target.as_deref(), Some("owner/repo"));
        assert!(should_project(&cfg));
        assert_eq!(load(dir.path(), "proj"), cfg);
    }

    #[test]
    fn disable_keeps_the_target_but_stops_egress() {
        let dir = TempDir::new().expect("tempdir");
        enable(
            dir.path(),
            "proj",
            "github",
            "owner/repo",
            "2026-07-25T09:00:00Z",
        )
        .expect("enable");
        let off = disable(dir.path(), "proj", "2026-07-25T09:05:00Z").expect("disable");
        assert!(!should_project(&off));
        assert_eq!(off.target.as_deref(), Some("owner/repo"));
    }

    /// Two checkouts on one machine must not enable each other.
    #[test]
    fn configuration_is_scoped_to_one_project() {
        let dir = TempDir::new().expect("tempdir");
        enable(
            dir.path(),
            "proj-a",
            "github",
            "owner/a",
            "2026-07-25T09:00:00Z",
        )
        .expect("enable");
        assert!(!should_project(&load(dir.path(), "proj-b")));
    }

    /// An enabled flag with no destination is a half-finished decision. Treating
    /// it as consent would be the one way a default-off system still writes.
    #[test]
    fn enabled_without_a_target_is_not_consent() {
        let cfg = TrackerConfig {
            enabled: true,
            provider: Some("github".into()),
            target: None,
            decided_at: Some("2026-07-25T09:00:00Z".into()),
            source: ConfigSource::Explicit,
            ..TrackerConfig::default()
        };
        assert!(!should_project(&cfg));
    }

    /// The reversal, in its intended direction: a human ran `tracker on`, so the
    /// work they add afterwards is in scope without them saying so again.
    #[test]
    fn an_explicitly_enabled_project_lends_its_opt_in_to_new_chunks() {
        let dir = TempDir::new().expect("tempdir");
        let cfg =
            enable(dir.path(), "proj", "linear", "THI", "2026-07-27T09:00:00Z").expect("enable");
        assert_eq!(inherited_opt_in(&cfg), Some("linear"));
    }

    /// THE guarantee the reversal must not cost: a config that merely names a
    /// provider and a destination — no human decision behind it — lends nothing.
    /// If this ever returns Some, upgrading can fill a stranger's tracker again.
    #[test]
    fn a_configured_but_undecided_project_lends_nothing() {
        let cfg = TrackerConfig {
            enabled: true,
            provider: Some("linear".into()),
            target: Some("THI".into()),
            decided_at: None,
            source: ConfigSource::Default,
            ..TrackerConfig::default()
        };
        // Egress would be permitted — that rule does not read `source`.
        assert!(should_project(&cfg));
        // Inheritance is strictly stronger, and this is where they part.
        assert_eq!(inherited_opt_in(&cfg), None);
    }

    /// Turning mirroring off stops the inheritance too, without retyping the
    /// destination — `disable` deliberately keeps provider and target.
    #[test]
    fn a_disabled_project_lends_nothing_even_though_it_remembers_where() {
        let dir = TempDir::new().expect("tempdir");
        enable(dir.path(), "proj", "linear", "THI", "2026-07-27T09:00:00Z").expect("enable");
        let off = disable(dir.path(), "proj", "2026-07-27T09:05:00Z").expect("disable");
        assert_eq!(off.target.as_deref(), Some("THI"));
        assert_eq!(inherited_opt_in(&off), None);
    }

    /// A fresh project is the overwhelmingly common case and it must lend nothing.
    #[test]
    fn a_fresh_project_lends_nothing() {
        let dir = TempDir::new().expect("tempdir");
        assert_eq!(inherited_opt_in(&load(dir.path(), "proj")), None);
    }

    /// The companion lane round-trips, and the primary lane is untouched by it —
    /// naming a second destination must never quietly move the first.
    #[test]
    fn a_companion_lane_is_stored_beside_the_primary_without_disturbing_it() {
        let dir = TempDir::new().expect("tempdir");
        enable(
            dir.path(),
            "proj",
            "github",
            "owner/repo",
            "2026-07-29T09:00:00Z",
        )
        .expect("enable");
        let cfg = set_companion(
            dir.path(),
            "proj",
            "GitHub_Projects",
            "orgs/acme/projects/12",
            "2026-07-29T09:01:00Z",
        )
        .expect("set companion");

        // The key folds to lowercase, as everywhere else in the seam.
        assert_eq!(
            companion_lane(&cfg)
                .expect("usable")
                .map(|l| l.provider.as_str()),
            Some("github_projects")
        );
        assert_eq!(cfg.provider.as_deref(), Some("github"));
        assert_eq!(cfg.target.as_deref(), Some("owner/repo"));
        assert_eq!(load(dir.path(), "proj"), cfg);
    }

    /// The invalid state is not representable on disk: a lane naming the primary
    /// provider is refused at the door, and nothing is written.
    #[test]
    fn a_companion_lane_cannot_be_the_primary_provider() {
        let dir = TempDir::new().expect("tempdir");
        enable(
            dir.path(),
            "proj",
            "github",
            "owner/repo",
            "2026-07-29T09:00:00Z",
        )
        .expect("enable");

        let err = set_companion(
            dir.path(),
            "proj",
            "github",
            "owner/repo",
            "2026-07-29T09:01:00Z",
        )
        .expect_err("must refuse");

        assert!(err.to_string().contains("github"));
        assert!(load(dir.path(), "proj").companion.is_none());
        // Paired positive, same project: a DIFFERENT key is accepted, so the
        // refusal is about the collision and not about this config.
        assert!(
            set_companion(
                dir.path(),
                "proj",
                "github_projects",
                "orgs/acme/projects/12",
                "2026-07-29T09:02:00Z",
            )
            .is_ok()
        );
    }

    /// A lane with nowhere to write is refused: a board is addressed by project
    /// URL, and inheriting the primary's `owner/repo` would send it nowhere real.
    #[test]
    fn a_companion_lane_needs_a_destination_of_its_own() {
        let dir = TempDir::new().expect("tempdir");
        enable(
            dir.path(),
            "proj",
            "github",
            "owner/repo",
            "2026-07-29T09:00:00Z",
        )
        .expect("enable");

        assert!(
            set_companion(
                dir.path(),
                "proj",
                "github_projects",
                "  ",
                "2026-07-29T09:01:00Z"
            )
            .is_err()
        );
        assert!(load(dir.path(), "proj").companion.is_none());
    }

    /// Clearing is reachable without editing JSON — the asymmetry with
    /// `set_initiative` is deliberate and stated there.
    #[test]
    fn a_blank_provider_clears_the_companion_lane() {
        let dir = TempDir::new().expect("tempdir");
        enable(
            dir.path(),
            "proj",
            "github",
            "owner/repo",
            "2026-07-29T09:00:00Z",
        )
        .expect("enable");
        set_companion(
            dir.path(),
            "proj",
            "github_projects",
            "orgs/acme/projects/12",
            "2026-07-29T09:01:00Z",
        )
        .expect("set");

        let cleared = set_companion(dir.path(), "proj", "", "", "2026-07-29T09:02:00Z")
            .expect("clearing is allowed");

        assert!(cleared.companion.is_none());
        // The primary survived the clearing.
        assert_eq!(cleared.target.as_deref(), Some("owner/repo"));
    }

    /// A project that never named one has no lane, which is the answer for
    /// almost every project that will ever run this binary.
    #[test]
    fn a_fresh_project_has_no_companion_lane() {
        let dir = TempDir::new().expect("tempdir");
        assert_eq!(
            companion_lane(&load(dir.path(), "proj")).expect("no lane is fine"),
            None
        );
    }

    /// Corrupt state must fail closed — silence, not a panic and not egress.
    #[test]
    fn a_corrupt_file_loads_as_disabled() {
        let dir = TempDir::new().expect("tempdir");
        let path = config_path(dir.path(), "proj");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{ not json").expect("write");
        assert!(!should_project(&load(dir.path(), "proj")));
    }
}
