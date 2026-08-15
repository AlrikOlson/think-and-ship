//! The OTLP exporter specification's ENVIRONMENT CONTRACT — endpoints and
//! credentials, read the way the ecosystem already spells them.
//!
//! # Why this is its own module
//!
//! Two lanes need exactly the same answers. `otel send` (a CLI verb) and the
//! live emitter (a server-side background worker) both have to know where the
//! collector is and what header authenticates to it, and if each read the
//! environment its own way they would drift — one honouring the signal-specific
//! override and the other not, one percent-decoding and the other not. The
//! contract lives here once and both call it.
//!
//! # Why environment variables and not flags
//!
//! Every value here is either a URL the operator configures once per machine or
//! an API key. Credentials given as arguments are world-readable through `ps`
//! and persist in shell history, so a `--header` flag would be the unsafe way to
//! say a thing the spec already has a safe name for. That decision is argued in
//! full at [`HEADERS_ENV`].

use anyhow::Result;

/// The OTel-standard place to put OTLP credentials.
///
/// # Why an environment variable and not a `--header` flag
///
/// A `--header k=v` flag is what a curl user reaches for, and it was rejected.
/// Two reasons, and the second one is decisive.
///
/// First, this is not a new spelling to invent: the OTLP exporter spec already
/// defines `OTEL_EXPORTER_OTLP_HEADERS`, and every vendor's own onboarding page
/// already tells the reader to set it. A flag would be a second name for a
/// thing that has one.
///
/// Second, the payload here is ALWAYS a secret — `x-honeycomb-team`,
/// `DD-API-KEY`, Grafana's `Authorization: Basic …` are API keys by definition.
/// Arguments are world-readable through `ps` and land in shell history. So the
/// flag is not merely redundant, it is the unsafe way to say the same thing,
/// and shipping both would just be shipping the unsafe one with a note.
pub const HEADERS_ENV: &str = "OTEL_EXPORTER_OTLP_HEADERS";
/// The signal-specific override. Per the exporter spec the per-signal variable
/// REPLACES the general one rather than merging with it, which matters for the
/// realistic case: one collector for metrics, a different vendor for traces.
pub const TRACES_HEADERS_ENV: &str = "OTEL_EXPORTER_OTLP_TRACES_HEADERS";
/// The same override for the LOGS signal. Separate because it genuinely can
/// differ: sending traces to one vendor and logs to another is a common
/// arrangement, and it is the exporter spec's reason for having per-signal
/// variables at all.
pub const LOGS_HEADERS_ENV: &str = "OTEL_EXPORTER_OTLP_LOGS_HEADERS";

/// The BASE endpoint. Per the spec the signal path is APPENDED to this one —
/// `https://host` means `https://host/v1/traces`.
pub const ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
/// The traces endpoint, used AS GIVEN. The spec is explicit that this variable
/// is the full URL with no path appended, and that asymmetry with
/// [`ENDPOINT_ENV`] is the single most-reported OTLP misconfiguration — a
/// `…/v1/traces` value here plus appending yields `…/v1/traces/v1/traces` and a
/// 404 that reads like a broken collector.
pub const TRACES_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
/// The logs endpoint, used AS GIVEN — the same verbatim rule, and the same trap
/// one signal over.
pub const LOGS_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT";

/// The OTLP path for the trace signal.
const TRACES_PATH: &str = "/v1/traces";
/// The OTLP path for the log signal.
const LOGS_PATH: &str = "/v1/logs";

/// One OTLP signal's environment contract: which variable names it, and what
/// path it appends to a base endpoint.
///
/// Held as a type rather than duplicated per signal because the
/// append-vs-verbatim asymmetry is the single most commonly misconfigured thing
/// in OTLP, and a second copy of that rule is a second chance to get it wrong in
/// only one of the copies. Adding the logs signal is a `Signal` value, not a
/// forked `resolve_endpoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    /// The per-signal endpoint variable, taken verbatim when set.
    pub endpoint_env: &'static str,
    /// The per-signal headers variable, which REPLACES the general one.
    pub headers_env: &'static str,
    /// The path appended to [`ENDPOINT_ENV`] when only the base is set.
    pub path: &'static str,
}

/// The trace signal.
pub const TRACES: Signal = Signal {
    endpoint_env: TRACES_ENDPOINT_ENV,
    headers_env: TRACES_HEADERS_ENV,
    path: TRACES_PATH,
};

/// The log signal.
pub const LOGS: Signal = Signal {
    endpoint_env: LOGS_ENDPOINT_ENV,
    headers_env: LOGS_HEADERS_ENV,
    path: LOGS_PATH,
};

/// Which variable wins, held apart from `std::env` so precedence is testable
/// without mutating the process environment (which races under a test harness
/// that runs threads in parallel).
fn choose(
    specific: Option<String>,
    specific_name: &'static str,
    general: Option<String>,
    general_name: &'static str,
) -> Option<(&'static str, String)> {
    specific
        .filter(|v| !v.trim().is_empty())
        .map(|v| (specific_name, v))
        .or_else(|| {
            general
                .filter(|v| !v.trim().is_empty())
                .map(|v| (general_name, v))
        })
}

/// Resolve one signal's endpoint from its two variables, applying the spec's
/// append-vs-verbatim asymmetry.
///
/// Held pure (arguments in, no `std::env`) so both halves of that asymmetry can
/// be asserted without a process-wide mutation, and generic over the signal so
/// the asymmetry is stated once for every signal we will ever send.
pub fn resolve_endpoint(
    signal: Signal,
    specific: Option<String>,
    general: Option<String>,
) -> Option<String> {
    let (var, raw) = choose(specific, signal.endpoint_env, general, ENDPOINT_ENV)?;
    let raw = raw.trim().to_string();
    if var == signal.endpoint_env {
        // Verbatim. The user named the signal's endpoint; appending to it is the
        // classic `…/v1/traces/v1/traces` bug.
        return Some(raw);
    }
    Some(format!("{}{}", raw.trim_end_matches('/'), signal.path))
}

/// The configured traces endpoint, or `None` when the operator configured none.
///
/// `None` is the enable gate for live emission: an unconfigured server makes no
/// network calls. There is deliberately no separate on/off switch, because a
/// second switch for one decision only creates the state "endpoint set, nothing
/// emitted", whose only achievable purpose is to confuse.
#[must_use]
pub fn configured_endpoint() -> Option<String> {
    configured_signal_endpoint(TRACES)
}

/// The configured LOGS endpoint. Same enable gate, one signal over: an operator
/// who set only `OTEL_EXPORTER_OTLP_ENDPOINT` gets `…/v1/logs` for free, and one
/// who named a traces vendor explicitly does NOT accidentally get their logs
/// POSTed to a traces intake.
#[must_use]
pub fn configured_logs_endpoint() -> Option<String> {
    configured_signal_endpoint(LOGS)
}

fn configured_signal_endpoint(signal: Signal) -> Option<String> {
    resolve_endpoint(
        signal,
        std::env::var(signal.endpoint_env).ok(),
        std::env::var(ENDPOINT_ENV).ok(),
    )
}

/// Percent-decoding for header VALUES, which the W3C Baggage grammar the
/// exporter spec points at requires. Hand-rolled rather than pulling a crate in
/// for fifteen lines: an undecodable `%` sequence is passed through as a
/// literal, because a credential is likelier to contain a stray `%` than to be
/// a broken escape, and silently dropping a byte of someone's API key would
/// produce a 401 they could never explain.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

/// Parse the W3C-Baggage-shaped `k1=v1,k2=v2` list the exporter spec defines.
///
/// Optional whitespace around list members is allowed by the grammar and is
/// what a human writing the variable across a wrapped shell line will produce,
/// so it is trimmed rather than rejected.
///
/// A malformed entry is a hard error and names the variable it came from: the
/// alternative is silently sending fewer credentials than the user configured
/// and reporting the resulting 401 as if the endpoint were at fault.
pub fn parse_otlp_headers(var: &str, raw: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for member in raw.split(',') {
        let member = member.trim();
        if member.is_empty() {
            continue;
        }
        let Some((key, value)) = member.split_once('=') else {
            anyhow::bail!(
                "{var} is malformed: `{member}` has no `=`. The format is \
                 `key1=value1,key2=value2` (the OTLP exporter spec's W3C Baggage form)."
            );
        };
        let key = key.trim();
        if key.is_empty() {
            anyhow::bail!("{var} is malformed: an entry has an empty header name.");
        }
        out.push((key.to_string(), percent_decode(value.trim())));
    }
    Ok(out)
}

/// The configured OTLP headers for the traces signal, and the variable they
/// came from.
pub fn configured_headers() -> Result<Vec<(String, String)>> {
    configured_signal_headers(TRACES)
}

/// The configured OTLP headers for the logs signal.
pub fn configured_logs_headers() -> Result<Vec<(String, String)>> {
    configured_signal_headers(LOGS)
}

fn configured_signal_headers(signal: Signal) -> Result<Vec<(String, String)>> {
    let Some((var, raw)) = choose(
        std::env::var(signal.headers_env).ok(),
        signal.headers_env,
        std::env::var(HEADERS_ENV).ok(),
        HEADERS_ENV,
    ) else {
        return Ok(Vec::new());
    };
    parse_otlp_headers(var, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape every vendor's onboarding page prints.
    #[test]
    fn headers_parse_as_the_spec_spells_them() {
        let got = parse_otlp_headers(HEADERS_ENV, "x-honeycomb-team=abc123,DD-API-KEY=xyz")
            .expect("well-formed list");
        assert_eq!(
            got,
            vec![
                ("x-honeycomb-team".to_string(), "abc123".to_string()),
                ("DD-API-KEY".to_string(), "xyz".to_string()),
            ]
        );
    }

    /// Optional whitespace is IN the Baggage grammar, and it is what a human
    /// produces when the variable wraps across a shell line. Rejecting it would
    /// be a 401 whose cause is invisible.
    #[test]
    fn surrounding_whitespace_is_tolerated_not_rejected() {
        let got = parse_otlp_headers(HEADERS_ENV, " a=1 , b=2 ,").expect("OWS is legal");
        assert_eq!(
            got,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    /// Values are percent-encoded per the spec. Grafana Cloud's `Authorization:
    /// Basic <b64>` contains a space, so this is the difference between a
    /// working credential and a 401.
    #[test]
    fn values_are_percent_decoded() {
        let got = parse_otlp_headers(HEADERS_ENV, "Authorization=Basic%20aGk6dGhlcmU%3D")
            .expect("encoded value");
        assert_eq!(got[0].1, "Basic aGk6dGhlcmU=");
    }

    /// A stray `%` in a credential must survive rather than eat the next two
    /// bytes — dropping a byte of an API key produces a 401 nobody can explain.
    #[test]
    fn an_undecodable_percent_is_passed_through() {
        let got = parse_otlp_headers(HEADERS_ENV, "k=100%off").expect("literal percent");
        assert_eq!(got[0].1, "100%off");
    }

    /// Failing loudly beats sending fewer credentials than the user configured
    /// and blaming the far end. The message must name the variable, because the
    /// user set one of two possible variables.
    #[test]
    fn a_malformed_entry_names_the_variable_it_came_from() {
        let err = parse_otlp_headers(TRACES_HEADERS_ENV, "x-honeycomb-team abc")
            .expect_err("no `=` is malformed");
        let msg = err.to_string();
        assert!(msg.contains(TRACES_HEADERS_ENV), "{msg}");
        assert!(msg.contains("key1=value1"), "must show the format: {msg}");
    }

    /// The spec says the signal-specific variable REPLACES the general one. A
    /// merge would silently send a metrics credential to a traces intake.
    #[test]
    fn the_traces_variable_replaces_rather_than_merges() {
        let chosen = choose(
            Some("a=1".into()),
            TRACES_HEADERS_ENV,
            Some("b=2".into()),
            HEADERS_ENV,
        )
        .expect("some");
        assert_eq!(chosen, (TRACES_HEADERS_ENV, "a=1".to_string()));
        assert_eq!(
            choose(None, TRACES_HEADERS_ENV, Some("b=2".into()), HEADERS_ENV).expect("some"),
            (HEADERS_ENV, "b=2".to_string())
        );
        // An empty/whitespace value is "unset", not "send no headers on purpose".
        assert_eq!(
            choose(
                Some("   ".into()),
                TRACES_HEADERS_ENV,
                Some("b=2".into()),
                HEADERS_ENV
            )
            .expect("some"),
            (HEADERS_ENV, "b=2".to_string())
        );
        assert!(choose(None, TRACES_HEADERS_ENV, None, HEADERS_ENV).is_none());
    }

    /// The spec's append-vs-verbatim asymmetry, which is the single most
    /// commonly misconfigured thing in OTLP. Getting this backwards produces
    /// `/v1/traces/v1/traces` and a 404 that reads like a broken collector.
    #[test]
    fn the_base_endpoint_gains_the_signal_path_and_the_traces_one_does_not() {
        assert_eq!(
            resolve_endpoint(TRACES, None, Some("http://localhost:4318".into())).expect("base"),
            "http://localhost:4318/v1/traces"
        );
        // A trailing slash on the base must not double up.
        assert_eq!(
            resolve_endpoint(TRACES, None, Some("http://localhost:4318/".into())).expect("base"),
            "http://localhost:4318/v1/traces"
        );
        // The traces variable is taken exactly as given — no appending.
        assert_eq!(
            resolve_endpoint(
                TRACES,
                Some("https://api.honeycomb.io/v1/traces".into()),
                None
            )
            .expect("traces"),
            "https://api.honeycomb.io/v1/traces"
        );
        // And it wins over the base.
        assert_eq!(
            resolve_endpoint(
                TRACES,
                Some("https://vendor/v1/traces".into()),
                Some("http://localhost:4318".into())
            )
            .expect("traces wins"),
            "https://vendor/v1/traces"
        );
    }

    /// The logs signal obeys the SAME asymmetry, and — the point of making the
    /// resolver generic — one base endpoint feeds both signals down their own
    /// paths. If these two ever produce the same URL, every log we send lands in
    /// a traces intake and is dropped as malformed.
    #[test]
    fn one_base_endpoint_feeds_both_signals_down_their_own_paths() {
        let base = Some("http://localhost:4318".to_string());
        let traces = resolve_endpoint(TRACES, None, base.clone()).expect("traces");
        let logs = resolve_endpoint(LOGS, None, base).expect("logs");
        assert_eq!(traces, "http://localhost:4318/v1/traces");
        assert_eq!(logs, "http://localhost:4318/v1/logs");
        assert_ne!(traces, logs, "the two signals must not share a URL");
    }

    /// The verbatim half, for logs. A vendor's logs intake is named in full and
    /// must not gain `/v1/logs` a second time.
    #[test]
    fn the_logs_variable_is_taken_verbatim_and_ignores_the_traces_one() {
        assert_eq!(
            resolve_endpoint(
                LOGS,
                Some("https://vendor.example/otlp/v1/logs".into()),
                Some("http://localhost:4318".into())
            )
            .expect("logs wins"),
            "https://vendor.example/otlp/v1/logs"
        );
        // Setting ONLY the traces endpoint must not silently route logs there.
        // `configured_signal_endpoint` reads `signal.endpoint_env`, so the two
        // signals naming DIFFERENT variables is what makes that true.
        assert_ne!(TRACES.endpoint_env, LOGS.endpoint_env);
        assert_ne!(TRACES.headers_env, LOGS.headers_env);
        assert_eq!(LOGS.endpoint_env, LOGS_ENDPOINT_ENV);
        assert_eq!(LOGS.headers_env, LOGS_HEADERS_ENV);
    }

    /// No configuration means no emission, which is the enable gate. If this
    /// ever returns `Some` from an empty environment, an MCP server nobody
    /// configured starts making network calls.
    #[test]
    fn an_unconfigured_environment_yields_no_endpoint() {
        for signal in [TRACES, LOGS] {
            assert!(resolve_endpoint(signal, None, None).is_none());
            assert!(resolve_endpoint(signal, Some("  ".into()), Some("".into())).is_none());
        }
    }
}
