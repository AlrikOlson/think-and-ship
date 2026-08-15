//! `CloudClient` — pushes unified record envelopes to the per-tenant cloud
//! backend. The backend derives the tenant from the Bearer token
//! and overrides any `x-tenant-id`, so the client only sends the token + the
//! envelope to `PUT /v1/records`.

use serde::Deserialize;

use crate::cloud::envelope::UnifiedRecordEnvelope;

/// The `GET /v1/records` list response envelope (`{ "records": [...] }`).
#[derive(Deserialize)]
struct ListResponse {
    records: Vec<serde_json::Value>,
}

/// A push failure: a transport error, or a non-2xx response from the backend.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("cloud transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("cloud backend returned {status}: {body}")]
    Status { status: u16, body: String },
}

/// Whether a push failure is worth replaying: transport errors (offline) and
/// 5xx are; a 4xx contract rejection would fail forever.
fn retryable(e: &CloudError) -> bool {
    match e {
        CloudError::Http(_) => true,
        CloudError::Status { status, .. } => *status >= 500,
    }
}

/// What the backend did with the pushed record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// The record was new (`201`).
    Created,
    /// The record was content-identical to one already stored (`200`).
    Deduped,
}

/// A thin HTTP client for one tenant's cloud backend, authenticated by a Bearer
/// token whose `tenant` claim must match the envelopes it pushes. With an
/// [`Outbox`](crate::cloud::outbox::Outbox) attached, a push that fails on
/// transport or a 5xx is queued for replay instead of dropped
/// (sync-offline-queue); clones share the one queue.
#[derive(Debug, Clone)]
pub struct CloudClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
    outbox: Option<std::sync::Arc<crate::cloud::outbox::Outbox>>,
    /// The W3C `traceparent` to send downstream, resolved ONCE at construction.
    /// `None` — the default — means no caller context, so no header.
    traceparent: Option<String>,
}

impl CloudClient {
    /// Build a client for `base_url` (e.g. `https://…workers.dev`) authenticated
    /// with `token`. A trailing slash on `base_url` is normalized away.
    #[must_use]
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::new(),
            outbox: None,
            traceparent: None,
        }
    }

    /// Adopt the project's caller trace context, so every push is a child of
    /// our exported workspace span (SEP-414 downstream half). Resolved once
    /// here rather than per call: the context is last-writer-wins per project,
    /// and re-reading it mid-flush would let a concurrent tool call move the
    /// parent of a replay already in flight.
    #[must_use]
    pub fn with_trace_context(self, project: &str) -> Self {
        self.with_traceparent(crate::trace_context::outbound_traceparent(project))
    }

    /// Set the downstream `traceparent` directly. See
    /// [`GithubTracker::with_traceparent`](crate::tracker::GithubTracker::with_traceparent).
    #[must_use]
    pub fn with_traceparent(mut self, traceparent: Option<String>) -> Self {
        self.traceparent = traceparent;
        self
    }

    /// Attach the durable outbound queue (sync-offline-queue). Failed pushes
    /// (transport / 5xx) enqueue for replay via [`Self::flush_outbox`].
    #[must_use]
    pub fn with_outbox(mut self, outbox: std::sync::Arc<crate::cloud::outbox::Outbox>) -> Self {
        self.outbox = Some(outbox);
        self
    }

    /// The attached outbox, if any (tests + diagnostics).
    #[must_use]
    pub fn outbox(&self) -> Option<&std::sync::Arc<crate::cloud::outbox::Outbox>> {
        self.outbox.as_ref()
    }

    /// The normalized backend base URL (no trailing slash). The realtime
    /// subscriber derives its `wss://…/v1/events` endpoint from it.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The Bearer token, for sibling cloud transports (the WS upgrade request
    /// carries the same credential as every HTTP call). Crate-internal.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Push one envelope to the cloud (`PUT /v1/records`, Bearer auth). Maps the
    /// backend status to a [`PushOutcome`]; any non-2xx is a [`CloudError`].
    /// With an outbox attached, a RETRYABLE failure (transport / 5xx) queues
    /// the envelope for replay — the error is still returned so callers keep
    /// their log-and-drop shape. A 4xx is a contract rejection that would
    /// fail forever: logged loudly, never queued.
    pub async fn push(&self, envelope: &UnifiedRecordEnvelope) -> Result<PushOutcome, CloudError> {
        let value = serde_json::to_value(envelope).unwrap_or(serde_json::Value::Null);
        let result = self.push_json(&value).await;
        if let Err(e) = &result
            && let Some(outbox) = &self.outbox
        {
            if retryable(e) {
                let key = format!(
                    "{}/{}/{}",
                    envelope.family.as_str(),
                    envelope.kind.as_str(),
                    envelope.id
                );
                tracing::warn!(
                    target: "think_and_ship::cloud",
                    "push failed, queued for replay ({key}): {e}"
                );
                outbox.enqueue(key, value);
            } else {
                tracing::warn!(
                    target: "think_and_ship::cloud",
                    "push REJECTED by the contract (not queued — would fail forever): {e}"
                );
            }
        }
        result
    }

    /// The raw push of an already-serialized envelope (the outbox replay path).
    async fn push_json(&self, value: &serde_json::Value) -> Result<PushOutcome, CloudError> {
        let mut req = self
            .http
            .put(format!("{}/v1/records", self.base_url))
            .bearer_auth(&self.token);
        // SEP-414's downstream half. `None` — no caller context adopted — adds
        // no header at all, which is the point: an unparented call must not
        // acquire a fabricated root just because it went over HTTP.
        if let Some(tp) = &self.traceparent {
            req = req.header("traceparent", tp.clone());
        }
        let response = req.json(value).send().await?;
        match response.status().as_u16() {
            201 => Ok(PushOutcome::Created),
            200 => Ok(PushOutcome::Deduped),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(CloudError::Status { status, body })
            }
        }
    }

    /// Replay the queued pushes oldest-first (sync-offline-queue). Stops at
    /// the first retryable failure (still offline — the rest stay queued); a
    /// 4xx rejection drops that entry (it would fail forever) and continues.
    /// Concurrent flushes are coalesced (boot + realtime race). Returns how
    /// many entries were drained.
    pub async fn flush_outbox(&self) -> usize {
        let Some(outbox) = &self.outbox else { return 0 };
        if outbox.is_empty() || !outbox.begin_flush() {
            return 0;
        }
        let mut drained = 0;
        for entry in outbox.snapshot() {
            match self.push_json(&entry.envelope).await {
                Ok(_) => {
                    outbox.remove(&entry);
                    drained += 1;
                }
                Err(e) if retryable(&e) => {
                    tracing::debug!(
                        target: "think_and_ship::cloud",
                        "outbox flush paused ({} left): {e}",
                        outbox.len()
                    );
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "think_and_ship::cloud",
                        "outbox entry {} rejected by the contract — dropping it: {e}",
                        entry.key
                    );
                    outbox.remove(&entry);
                    drained += 1;
                }
            }
        }
        outbox.end_flush();
        if drained > 0 {
            tracing::info!(
                target: "think_and_ship::cloud",
                "outbox flushed {drained} queued push(es)"
            );
        }
        drained
    }

    /// Fetch one record by identity (`GET /v1/records/{family}/{kind}/{id}`,
    /// Bearer auth). `200` → `Some`, `404` → `None`, any other non-2xx → error.
    pub async fn get(
        &self,
        family: &str,
        kind: &str,
        id: &str,
    ) -> Result<Option<serde_json::Value>, CloudError> {
        let response = self
            .http
            .get(format!("{}/v1/records/{family}/{kind}/{id}", self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await?;
        match response.status().as_u16() {
            200 => Ok(Some(response.json().await?)),
            404 => Ok(None),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(CloudError::Status { status, body })
            }
        }
    }

    /// List the tenant's records (`GET /v1/records`, optionally `?family=` and
    /// `?since=` — the rows-read cursor: only records with `updated >=` the
    /// watermark come back; the inclusive boundary is safe because merges are
    /// idempotent), unwrapping the `{ "records": [...] }` envelope.
    pub async fn list(
        &self,
        family: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, CloudError> {
        let mut request = self
            .http
            .get(format!("{}/v1/records", self.base_url))
            .bearer_auth(&self.token);
        if let Some(f) = family {
            request = request.query(&[("family", f)]);
        }
        if let Some(w) = since {
            request = request.query(&[("since", w)]);
        }
        let response = request.send().await?;
        if response.status().is_success() {
            Ok(response.json::<ListResponse>().await?.records)
        } else {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            Err(CloudError::Status { status, body })
        }
    }

    /// Fetch the cross-ref graph neighborhood of an endpoint (the moat) —
    /// `GET /v1/graph/neighbors?endpoint=<prefix:value>`.
    pub async fn neighbors(&self, endpoint: &str) -> Result<serde_json::Value, CloudError> {
        let response = self
            .http
            .get(format!("{}/v1/graph/neighbors", self.base_url))
            .query(&[("endpoint", endpoint)])
            .bearer_auth(&self.token)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            Err(CloudError::Status { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::envelope::{Family, Kind};
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env(tenant: &str) -> UnifiedRecordEnvelope {
        UnifiedRecordEnvelope::owner(
            tenant,
            Family::Roadmap,
            Kind::Chunk,
            "c1",
            "2026-06-08T00:00:00Z",
            json!({ "id": "c1", "status": "backlog" }),
            vec![],
        )
    }

    #[tokio::test]
    async fn push_sends_a_bearer_put_with_the_envelope_body_and_maps_201() {
        let server = MockServer::start().await;
        let envelope = env("t");
        Mock::given(method("PUT"))
            .and(path("/v1/records"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(serde_json::to_value(&envelope).unwrap()))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;

        let client = CloudClient::new(server.uri(), "tok");
        assert_eq!(client.push(&envelope).await.unwrap(), PushOutcome::Created);
    }

    /// SEP-414 downstream half, asserted on the wire rather than on the field.
    /// Both directions in one test, because the ABSENCE is the half that a
    /// "just add the header" implementation gets wrong: an unparented push must
    /// not acquire a fabricated root.
    #[tokio::test]
    async fn push_carries_the_traceparent_only_when_a_context_was_adopted() {
        const CHILD: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-a1b2c3d4e5f60718-01";

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/records"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;

        CloudClient::new(server.uri(), "tok")
            .with_traceparent(Some(CHILD.to_string()))
            .push(&env("with"))
            .await
            .expect("push");
        // The default — no caller context ever adopted.
        CloudClient::new(server.uri(), "tok")
            .push(&env("without"))
            .await
            .expect("push");

        let seen: Vec<Option<String>> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|r| {
                r.headers
                    .get("traceparent")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].as_deref(),
            Some(CHILD),
            "adopted context propagates"
        );
        assert_eq!(seen[1], None, "no context means no header, never a root");
    }

    #[tokio::test]
    async fn push_maps_200_to_deduped() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/records"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = CloudClient::new(server.uri(), "tok");
        assert_eq!(client.push(&env("t")).await.unwrap(), PushOutcome::Deduped);
    }

    #[tokio::test]
    async fn push_maps_non_2xx_to_a_status_error() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/records"))
            .respond_with(
                ResponseTemplate::new(422).set_body_string(r#"{"error":"schema_invalid"}"#),
            )
            .mount(&server)
            .await;

        let client = CloudClient::new(server.uri(), "tok");
        match client.push(&env("t")).await {
            Err(CloudError::Status { status, body }) => {
                assert_eq!(status, 422);
                assert!(body.contains("schema_invalid"));
            }
            other => panic!("expected a Status error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_returns_some_on_200_and_none_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/records/roadmap/chunk/c1"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "c1" })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/records/roadmap/chunk/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "not_found" })))
            .mount(&server)
            .await;

        let client = CloudClient::new(server.uri(), "tok");
        assert!(
            client
                .get("roadmap", "chunk", "c1")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            client
                .get("roadmap", "chunk", "missing")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn list_unwraps_records_and_passes_the_family_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/records"))
            .and(query_param("family", "roadmap"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "records": [{ "id": "a" }, { "id": "b" }] })),
            )
            .mount(&server)
            .await;

        let client = CloudClient::new(server.uri(), "tok");
        assert_eq!(client.list(Some("roadmap"), None).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn neighbors_passes_the_endpoint_query_and_returns_the_graph() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/graph/neighbors"))
            .and(query_param("endpoint", "think:9"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "endpoint": "think:9", "neighbors": [] })),
            )
            .mount(&server)
            .await;

        let client = CloudClient::new(server.uri(), "tok");
        let graph = client.neighbors("think:9").await.unwrap();
        assert_eq!(graph["endpoint"], "think:9");
    }

    /// Live read against the REAL deployed backend (lists the tenant's records —
    /// may be empty). Gated on env so it never runs unprompted.
    #[tokio::test]
    #[ignore = "requires a live backend + token (THINK_AND_SHIP_CLOUD_URL/_TOKEN)"]
    async fn live_list_from_real_backend() {
        let url = std::env::var("THINK_AND_SHIP_CLOUD_URL").expect("THINK_AND_SHIP_CLOUD_URL");
        let token =
            std::env::var("THINK_AND_SHIP_CLOUD_TOKEN").expect("THINK_AND_SHIP_CLOUD_TOKEN");
        let client = CloudClient::new(url, token);
        let records = client.list(None, None).await.expect("live list");
        // The call succeeded; the count is whatever the tenant holds.
        let _ = records.len();
    }

    /// Live integration against the REAL deployed backend. Gated on env so it
    /// never runs unprompted: set `THINK_AND_SHIP_CLOUD_URL` +
    /// `THINK_AND_SHIP_CLOUD_TOKEN` (a JWT whose `tenant` claim matches
    /// `THINK_AND_SHIP_CLOUD_TENANT`, default `rust-live`), then
    /// `cargo test -- --ignored live_push_to_real_backend`.
    #[tokio::test]
    #[ignore = "requires a live backend + token (THINK_AND_SHIP_CLOUD_URL/_TOKEN)"]
    async fn live_push_to_real_backend() {
        let url = std::env::var("THINK_AND_SHIP_CLOUD_URL").expect("THINK_AND_SHIP_CLOUD_URL");
        let token =
            std::env::var("THINK_AND_SHIP_CLOUD_TOKEN").expect("THINK_AND_SHIP_CLOUD_TOKEN");
        let tenant =
            std::env::var("THINK_AND_SHIP_CLOUD_TENANT").unwrap_or_else(|_| "rust-live".into());

        let client = CloudClient::new(url, token);
        let outcome = client.push(&env(&tenant)).await.expect("live push");
        assert!(matches!(
            outcome,
            PushOutcome::Created | PushOutcome::Deduped
        ));
    }
}
