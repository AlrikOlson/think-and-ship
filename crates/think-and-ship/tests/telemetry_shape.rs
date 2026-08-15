//! 31h-a end-to-end privacy proof: real domain records with PLANTED secrets,
//! projected through the real envelope builders (the exact path 31h-c will
//! use), must produce a shape whose serialization contains none of them —
//! and, stronger, no free-text input string at all.

use think_and_ship::cloud::build::{from_chunk, from_step};
use think_and_ship::cloud::envelope::UnifiedRecordEnvelope;
use think_and_ship::roadmap::domain::Chunk;
use think_and_ship::telemetry::{extract, scan};
use think_and_ship::think::domain::step::ThinkStep;

const SALT: &[u8] = b"itest-install-salt";

/// Secrets planted across free-text fields. Each must be provably absent.
const PLANTED: &[&str] = &[
    "AKIAIOSFODNN7EXAMPLE",
    "sk_live_51HxTotallyFakeKey99",
    "ghp_16chartokenFAKEFAKEFAKE",
    "alice.engineer@secret-corp.example",
    "eyJhbGciOiJIUzI1NiJ9.fake.jwt",
    "-----BEGIN RSA PRIVATE KEY-----",
];

fn planted_step() -> ThinkStep {
    serde_json::from_value(serde_json::json!({
        "step_number": 7,
        "estimated_total": 9,
        "purpose": format!("Investigate the leak of {}", PLANTED[0]),
        "context": format!("Customer reported {} in logs; contact {}", PLANTED[1], PLANTED[3]),
        "thought": format!("The token {} was committed. JWT seen: {}", PLANTED[2], PLANTED[4]),
        "outcome": format!("Rotated everything including {}", PLANTED[5]),
        "next_action": "rotate keys",
        "rationale": "secrets must not persist",
        "tools_used": ["Bash", "Edit", "Bash"],
        "dependencies": [6],
        "timestamp": "2026-06-10T01:00:00Z",
    }))
    .expect("step deserializes via serde defaults")
}

fn planted_chunk() -> Chunk {
    serde_json::from_value(serde_json::json!({
        "id": "rotate-creds",
        "title": format!("Rotate {} before launch", PLANTED[0]),
        "status": "in_progress",
        "priority": 100,
        "description": format!("Found {} in CI logs", PLANTED[1]),
        "notes": "",
        "acceptance": [format!("no more {}", PLANTED[2])],
        "deps": [],
        "cross_refs": ["think:7", "task:verify"],
        "shared": false,
        "created_at": "2026-06-10T00:00:00Z",
        "updated_at": "2026-06-10T02:30:00Z",
    }))
    .expect("chunk deserializes")
}

fn envelopes() -> Vec<UnifiedRecordEnvelope> {
    vec![
        from_step("tenant-x", &planted_step()),
        from_chunk("tenant-x", &planted_chunk(), &[], &[]),
    ]
}

#[test]
fn planted_secrets_never_survive_extraction() {
    let envs = envelopes();
    // Sanity: the secrets ARE in the input projection (the test would be
    // vacuous otherwise).
    let input = serde_json::to_string(&envs).expect("serialize input");
    for secret in PLANTED {
        assert!(input.contains(secret), "test setup lost planted {secret}");
    }

    let shape = extract(&envs, SALT).expect("clean shape");
    let output = serde_json::to_string(&shape).expect("serialize shape");
    for secret in PLANTED {
        assert!(
            !output.contains(secret),
            "planted secret survived: {secret}"
        );
    }
    assert!(
        scan(&output).is_empty(),
        "scrub detectors fired on the shape"
    );
}

/// The stronger property: NO free-text input string survives. Every string
/// in the input projection (≥4 chars, not a closed-vocabulary token) must be
/// absent from the serialized shape.
#[test]
fn no_input_string_survives_serialization() {
    // Closed vocabulary legitimately shared between input and output.
    const VOCAB: &[&str] = &[
        "think",
        "roadmap",
        "step",
        "chunk",
        "in_progress",
        "done",
        "supports",
        "refutes",
        "depends_on",
    ];

    fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_strings(item, out);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values() {
                    collect_strings(item, out);
                }
            }
            _ => {}
        }
    }

    let envs = envelopes();
    let mut inputs = Vec::new();
    for env in &envs {
        collect_strings(
            &serde_json::to_value(env).expect("env to value"),
            &mut inputs,
        );
    }
    assert!(inputs.len() > 10, "input projection should be string-rich");

    let shape = extract(&envs, SALT).expect("clean shape");
    let output = serde_json::to_string(&shape).expect("serialize shape");

    for s in inputs {
        if s.len() < 4 || VOCAB.contains(&s.as_str()) {
            continue;
        }
        assert!(
            !output.contains(&s),
            "input string leaked into the shape: {s:?}"
        );
    }
}

/// The cross-language e2e half on the Rust side: the FULL wire report built
/// from planted-secret records satisfies the TypeScript ingest's contract
/// (backend/src/telemetry.ts isValidReport + reportLooksSensitive) and
/// carries no planted material. The TS suite holds the other half against
/// the same contract.
#[test]
fn wire_report_satisfies_the_ingest_contract() {
    let envs = envelopes();
    let shape = extract(&envs, SALT).expect("clean shape");
    let report = think_and_ship::telemetry::build_report(SALT, shape);
    let wire = serde_json::to_string(&report).expect("serialize report");

    for secret in PLANTED {
        assert!(
            !wire.contains(secret),
            "planted secret on the wire: {secret}"
        );
    }
    // isValidReport's constraints: 16-hex install + the schema marker.
    assert_eq!(report.install.len(), 16);
    assert!(
        report
            .install
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert!(wire.contains("\"schema\":\"telemetry-shape/1\""));
    // MAX_REPORT_BYTES on the ingest edge.
    assert!(wire.len() < 65_536, "report too large for the ingest cap");
    // reportLooksSensitive's detectors must not fire (mirrored here by scan).
    assert!(scan(&wire).is_empty());
}

#[test]
fn structure_survives_what_text_does_not() {
    let shape = extract(&envelopes(), SALT).expect("clean shape");
    // Two records: one think.step, one roadmap.chunk.
    assert_eq!(shape.records.get("think.step"), Some(&1));
    assert_eq!(shape.records.get("roadmap.chunk"), Some(&1));
    // The chunk's lifecycle state survives as vocabulary.
    assert_eq!(shape.statuses.get("roadmap.chunk.in_progress"), Some(&1));
    // The step's dependency edge + the chunk's two cross_refs.
    assert_eq!(shape.graph.nodes, 2);
    assert_eq!(shape.graph.edges, 3);
    // Tool usage: 3 uses, Bash→Edit and Edit→Bash bigrams.
    assert_eq!(shape.tools.values().sum::<usize>(), 3);
    assert_eq!(
        shape
            .tool_bigrams
            .values()
            .flat_map(|m| m.values())
            .sum::<usize>(),
        2
    );
    // chunk created→updated spans 2.5h → lt1d bucket.
    assert_eq!(shape.lifetimes.get("lt1d"), Some(&1));
}
