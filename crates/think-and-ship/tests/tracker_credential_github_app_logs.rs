//! THE no-secret-in-logs proof, alone in its own test binary.
//!
//! # Why this is not in the file next door
//!
//! It was, and it passed — sometimes. `tracing` caches a callsite's *interest*
//! globally the first time that callsite is evaluated. Sibling tests in the same
//! binary call the same conversion with no subscriber installed, and whichever
//! test reaches the callsite first decides whether the event is ever built. So
//! the proof passed when the harness happened to schedule it first and reported
//! "the production log line did not reach the capture" when it did not. A proof
//! that depends on test ordering is not a proof.
//!
//! Two things fix it, and both are deliberate: this is the ONLY test in its
//! binary, so nothing races it, and it installs the subscriber with
//! `set_global_default`, which rebuilds the interest cache rather than hoping it
//! was never poisoned.
//!
//! # What it proves
//!
//! Deliberately breaking an earlier version established the rule: an assertion
//! that inspects only a return value stays green while the forbidden thing
//! happens beside it.
//! So this runs the REAL conversion against a mock serving sentinel secrets with
//! a subscriber capturing everything, renders every human-facing surface into the
//! same buffer, and scans the lot — while also asserting the buffer contains
//! output it definitely should, because a scan over an empty capture passes
//! vacuously and would be the same bug wearing a different hat.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use think_and_ship::tracker::credential::github_app;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PEM: &str = "-----BEGIN RSA PRIVATE KEY-----SENTINEL-PEM-MATERIAL-9f13";
const CLIENT_SECRET: &str = "SENTINEL-CLIENT-SECRET-4d7a";
const WEBHOOK_SECRET: &str = "SENTINEL-WEBHOOK-SECRET-b0c2";
const CODE: &str = "SENTINEL-MANIFEST-CODE-77ab";

/// A writer that keeps everything, so the test can read back what a subscriber
/// would have written to a terminal or a log file.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("captured output")).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("captured output")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn no_secret_reaches_the_captured_output_of_a_real_conversion() {
    let captured = Captured::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    )
    .expect("this binary holds exactly one test, so nothing else has claimed the default");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/app-manifests/{CODE}/conversions")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 654_321,
            "slug": "think-and-ship-dogfood",
            "node_id": "MDM6SW50ZWdyYXRpb24x",
            "name": "think-and-ship",
            "html_url": "https://github.com/apps/think-and-ship-dogfood",
            "client_id": "Iv1.0000public0000",
            "client_secret": CLIENT_SECRET,
            "pem": PEM,
            "webhook_secret": WEBHOOK_SECRET,
            "permissions": {
                "issues": "write",
                "metadata": "read",
                "organization_projects": "write",
            },
            "events": ["issues"],
        })))
        .mount(&server)
        .await;

    let app = github_app::convert_manifest_at(&reqwest::Client::new(), &server.uri(), CODE)
        .await
        .expect("conversion");

    // Everything a human is ever shown about a registration, written to the
    // same place a log would go.
    let mut sink = captured.clone();
    writeln!(sink, "{}", app.report()).expect("write report");
    writeln!(sink, "{app:?}").expect("write debug");
    tracing::info!(?app, "the struct itself, rendered through Debug");

    let output = captured.text();

    // NOT VACUOUS: the capture holds real output about this registration,
    // including the line the production path emits.
    assert!(
        output.contains("registered a GitHub App from a manifest"),
        "the production log line did not reach the capture, so the scan below would \
         prove nothing:\n{output}"
    );
    assert!(output.contains("think-and-ship-dogfood"), "{output}");

    for (name, secret) in [
        ("pem", PEM),
        ("client_secret", CLIENT_SECRET),
        ("webhook_secret", WEBHOOK_SECRET),
        ("manifest code", CODE),
    ] {
        assert!(
            !output.contains(secret),
            "the {name} reached captured output:\n{output}"
        );
    }

    // And the material really was delivered — the scan passed because the
    // secrets are held, not because they were never received.
    assert_eq!(app.pem.expose(), PEM);
    assert_eq!(app.client_secret.expose(), CLIENT_SECRET);
    assert_eq!(
        app.webhook_secret
            .as_ref()
            .map(think_and_ship::tracker::credential::Secret::expose),
        Some(WEBHOOK_SECRET)
    );
}
