//! NDJSON-over-Unix-socket fan-out, shared by both tool families.
//!
//! Every emitted frame carries a `family` tag so a single viewer can
//! interleave `think_*` and `ship_*` events on one timeline without
//! maintaining two socket readers.
//!
//! Construct with [`Broadcaster::spawn`]; missing tokio runtime or a bind
//! failure returns `None` and the server keeps running unobserved.
//!
//! Unix sockets don't exist on Windows, so the socket machinery is
//! `cfg(unix)`; on other platforms [`Broadcaster::spawn`] always returns
//! `None` — the exact "run without broadcast" degraded mode callers already
//! handle for a bind failure.

use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

use serde::Serialize;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::sync::Mutex;
use tokio::sync::mpsc;
#[cfg(unix)]
use tokio::sync::watch;
use tracing::info;
#[cfg(unix)]
use tracing::{debug, warn};

/// Identifies which family emitted a frame. Wire form is the lowercase
/// name (`"think"` / `"ship"` / `"roadmap"` / `"signal"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Think,
    Ship,
    Roadmap,
    Signal,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Ship => "ship",
            Self::Roadmap => "roadmap",
            Self::Signal => "signal",
        }
    }
}

#[derive(Clone)]
pub struct Broadcaster {
    tx: mpsc::UnboundedSender<String>,
    /// How many clients the accept loop has actually registered. See
    /// [`Broadcaster::subscriber_count`] for why connecting is not the
    /// same thing as being registered.
    #[cfg(unix)]
    subscribers: watch::Receiver<usize>,
}

impl Broadcaster {
    /// Bind a Unix socket at `path` and start the accept + fan-out tasks.
    /// Returns `None` if no tokio runtime is active or the socket can't
    /// be bound — callers should treat that as "run without broadcast".
    #[cfg(unix)]
    pub fn spawn(path: PathBuf) -> Option<Self> {
        if tokio::runtime::Handle::try_current().is_err() {
            return None;
        }

        if std::fs::symlink_metadata(&path)
            .is_ok_and(|m| std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()))
        {
            let _ = std::fs::remove_file(&path);
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                warn!(target: "think_and_ship::broadcast", "could not bind {}: {e}", path.display());
                return None;
            }
        };

        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (sub_tx, subscribers) = watch::channel(0usize);

        tokio::spawn(accept_loop(listener, Arc::clone(&clients), sub_tx.clone()));
        tokio::spawn(fanout_loop(rx, clients, sub_tx));

        info!(target: "think_and_ship::broadcast", "listening at {}", path.display());
        Some(Self { tx, subscribers })
    }

    /// Unix sockets don't exist on this platform — always `None`, the same
    /// "run without broadcast" mode a bind failure produces on Unix.
    #[cfg(not(unix))]
    pub fn spawn(path: PathBuf) -> Option<Self> {
        info!(
            target: "think_and_ship::broadcast",
            "broadcast socket {} unavailable on this platform (Unix sockets only); running unobserved",
            path.display()
        );
        None
    }

    /// Encode a family-tagged frame: `{"family": "<f>", ...payload}`.
    ///
    /// `payload` must serialize to a JSON object so the `family` field can
    /// be flattened onto it. Returns `Err` if encoding fails; otherwise
    /// the frame is queued for fan-out and the caller does not block.
    pub fn emit<T: Serialize>(&self, family: Family, payload: &T) -> Result<(), EmitError> {
        let mut value = serde_json::to_value(payload).map_err(EmitError::Encode)?;
        let obj = value.as_object_mut().ok_or(EmitError::PayloadNotObject)?;
        obj.insert(
            "family".to_string(),
            serde_json::Value::String(family.as_str().to_string()),
        );
        let line = serde_json::to_string(&value).map_err(EmitError::Encode)?;
        self.tx.send(line).map_err(|_| EmitError::Closed)
    }

    /// How many clients `accept_loop` has pushed into the fan-out list.
    ///
    /// **Connecting is not the same as being registered.** `spawn` binds
    /// and listens synchronously, so a client's `UnixStream::connect`
    /// completes off the kernel's listen backlog without the accept task
    /// having been polled even once. Until that task runs, the client is
    /// not in `clients`, and `fanout_loop` drops every frame it is
    /// handed — it locks an empty vector and iterates nothing. Frames
    /// emitted in that window are gone, not queued.
    ///
    /// Measured at 3 lost runs in 300 on an idle machine, and 279 in 300
    /// with the runtime's workers saturated. A `sleep` before connecting
    /// only pre-warms the accept task so it is parked on `accept()`
    /// rather than cold in the spawn queue; it narrows the window and
    /// never closes it, which is why this was a CI-only flake.
    ///
    /// Use [`Self::wait_for_subscribers`] to wait for the window to close.
    #[cfg(unix)]
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        *self.subscribers.borrow()
    }

    /// Resolve once at least `n` clients are registered, or `false` if
    /// `timeout` elapses first.
    ///
    /// This waits on the count actually changing rather than sleeping for
    /// a guessed duration: the `timeout` bounds *failure*, never success,
    /// so a slow machine waits longer instead of racing. Returns `false`
    /// rather than panicking so callers choose how a timeout is reported.
    #[cfg(unix)]
    pub async fn wait_for_subscribers(&self, n: usize, timeout: Duration) -> bool {
        let mut rx = self.subscribers.clone();
        let reached = async {
            loop {
                if *rx.borrow_and_update() >= n {
                    return true;
                }
                // Only the accept loop holds the sender, and it runs for
                // the process's life — an error means it is gone and the
                // count can never rise again.
                if rx.changed().await.is_err() {
                    return false;
                }
            }
        };
        matches!(tokio::time::timeout(timeout, reached).await, Ok(true))
    }
}

#[derive(Debug)]
pub enum EmitError {
    Encode(serde_json::Error),
    PayloadNotObject,
    Closed,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "could not encode broadcast frame: {e}"),
            Self::PayloadNotObject => {
                write!(f, "broadcast payload must serialize to a JSON object")
            }
            Self::Closed => write!(f, "broadcaster channel is closed"),
        }
    }
}

impl std::error::Error for EmitError {}

#[cfg(unix)]
async fn accept_loop(
    listener: UnixListener,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    subscribers: watch::Sender<usize>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                debug!(target: "think_and_ship::broadcast", "client connected");
                let registered = {
                    let mut guard = clients.lock().await;
                    guard.push(stream);
                    guard.len()
                };
                // Published AFTER the push, and only after the lock is
                // released. A waiter woken by this value must find the
                // client already in the fan-out list, or the wait would
                // hand back the very race it exists to close.
                let _ = subscribers.send(registered);
            }
            Err(e) => {
                warn!(target: "think_and_ship::broadcast", "accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(unix)]
async fn fanout_loop(
    mut rx: mpsc::UnboundedReceiver<String>,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    subscribers: watch::Sender<usize>,
) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        let bytes = line.as_bytes();
        let mut guard = clients.lock().await;
        let taken = std::mem::take(&mut *guard);
        let had = taken.len();
        let mut survivors = Vec::with_capacity(had);
        for mut stream in taken {
            match stream.write_all(bytes).await {
                Ok(()) => survivors.push(stream),
                Err(_) => debug!(target: "think_and_ship::broadcast", "client disconnected"),
            }
        }
        let remaining = survivors.len();
        *guard = survivors;
        drop(guard);
        // Pruning happens here, so the count has to be republished here
        // too — otherwise `subscriber_count` would report clients that
        // dropped off, which is a worse lie than reporting none.
        if remaining != had {
            let _ = subscribers.send(remaining);
        }
    }
}

// The socket machinery is unix-only, so its tests are too.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde::Serialize;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[derive(Serialize)]
    struct Sample {
        kind: &'static str,
        n: u32,
    }

    #[derive(Serialize)]
    struct NotAnObject(u32);

    #[tokio::test]
    async fn spawn_returns_some_with_valid_path() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock);
        assert!(b.is_some());
    }

    #[tokio::test]
    async fn emit_flattens_family_onto_payload() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock.clone()).expect("spawn");

        let stream = UnixStream::connect(&sock).await.unwrap();
        // Connecting does not make this client a subscriber; emitting
        // before the accept loop registers it drops the frame and this
        // test then fails on a read timeout. See `subscriber_count`.
        assert!(
            b.wait_for_subscribers(1, Duration::from_secs(10)).await,
            "accept loop never registered the connected client",
        );
        let mut reader = BufReader::new(stream);

        b.emit(Family::Think, &Sample { kind: "step", n: 7 })
            .unwrap();

        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();

        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["family"], "think");
        assert_eq!(v["kind"], "step");
        assert_eq!(v["n"], 7);
    }

    /// The reason `wait_for_subscribers` exists, stated as a property:
    /// a frame emitted while nothing is registered is GONE, not buffered
    /// for whoever connects next.
    ///
    /// Deterministic by construction — no client exists at all when the
    /// first frame is emitted, so this does not depend on scheduling.
    /// It is the half that makes the wait load-bearing: if frames were
    /// queued until a subscriber appeared, waiting would be decorative.
    #[tokio::test]
    async fn a_frame_emitted_before_anyone_subscribes_is_dropped_not_queued() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock.clone()).expect("spawn");

        // Nobody has connected yet. This frame has no destination.
        b.emit(Family::Think, &Sample { kind: "lost", n: 1 })
            .unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        assert!(
            b.wait_for_subscribers(1, Duration::from_secs(10)).await,
            "accept loop never registered the connected client",
        );
        let mut reader = BufReader::new(stream);

        b.emit(Family::Think, &Sample { kind: "kept", n: 2 })
            .unwrap();

        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .expect("a frame emitted after registration must arrive")
            .unwrap();

        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            v["kind"], "kept",
            "the pre-subscription frame must not be replayed; got {v}",
        );
    }

    /// The window itself: `spawn` binds synchronously, so the socket is
    /// connectable immediately, yet nothing is registered until the
    /// accept task runs. `wait_for_subscribers` is what closes it.
    #[tokio::test]
    async fn connecting_is_not_subscribing_and_the_wait_is_what_closes_the_gap() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock.clone()).expect("spawn");

        // Bound and listening, but the accept loop has registered nobody.
        assert_eq!(
            b.subscriber_count(),
            0,
            "a freshly spawned broadcaster has no subscribers",
        );

        let _stream = UnixStream::connect(&sock).await.unwrap();
        assert!(
            b.wait_for_subscribers(1, Duration::from_secs(10)).await,
            "accept loop never registered the connected client",
        );
        assert_eq!(
            b.subscriber_count(),
            1,
            "the wait must not resolve before the client is in the fan-out list",
        );
    }

    /// The timeout bounds failure, and reports it as `false` rather than
    /// hanging or panicking — nobody ever connects here.
    #[tokio::test]
    async fn waiting_for_a_subscriber_that_never_arrives_reports_false() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock).expect("spawn");

        assert!(
            !b.wait_for_subscribers(1, Duration::from_millis(50)).await,
            "no client connected, so the wait must time out to false",
        );
    }

    /// Extract a function name from a definition line, or `None`.
    /// Handles the `pub` / `pub(crate)` / `async` prefixes these files use.
    fn fn_name_of(line: &str) -> Option<&str> {
        let mut t = line.trim_start();
        for prefix in ["pub(crate) ", "pub ", "async "] {
            t = t.strip_prefix(prefix).unwrap_or(t);
        }
        // `async` can follow `pub`, so allow one more pass.
        t = t.strip_prefix("async ").unwrap_or(t);
        let rest = t.strip_prefix("fn ")?;
        Some(rest.split(['(', '<', ' ']).next().unwrap_or(rest))
    }

    /// Split a source file into per-function blocks, keyed by name.
    /// Everything before the first `fn` lands under `<file prologue>`.
    fn fn_blocks(src: &str) -> Vec<(String, String)> {
        let mut blocks: Vec<(String, String)> = Vec::new();
        let mut name = String::from("<file prologue>");
        let mut body = String::new();
        for line in src.lines() {
            if let Some(next) = fn_name_of(line) {
                blocks.push((std::mem::take(&mut name), std::mem::take(&mut body)));
                name = next.to_string();
            }
            body.push_str(line);
            body.push('\n');
        }
        blocks.push((name, body));
        blocks
    }

    /// A live line: not a comment, not a doc comment.
    fn live(l: &&str) -> bool {
        !l.trim_start().starts_with("//")
    }

    /// REACHABILITY, not behaviour: every in-process test that connects
    /// to a live `Broadcaster` must also wait for the accept loop to
    /// register it.
    ///
    /// This gate exists because deleting the wait at a CALL SITE is
    /// invisible to every behavioural test here. The wait's absence does
    /// not produce a failure, it produces a *probability* of one — green
    /// on an idle laptop, red 93% of the time on a contended runner.
    /// A deliberate-breakage run that removed the wait from
    /// `think_broadcast.rs` left the whole suite green while restoring
    /// the original CI flake.
    /// Behaviour cannot see that; only reachability can.
    ///
    /// The window is asserted as hard as the rule: an unreadable path
    /// panics rather than silently covering nothing, and the total
    /// connect count is pinned so a file that stops being scanned — or
    /// grows a connect nobody noticed — fails loudly instead of open.
    /// Blocks are split per function so a wait in a NEIGHBOURING test
    /// cannot vouch for a connect in this one.
    #[test]
    fn every_broadcast_test_that_connects_also_waits_for_registration() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let files = [
            manifest.join("src/infra/broadcast.rs"),
            manifest.join("tests/think_broadcast.rs"),
        ];
        // Split so this gate's own source does not match its own needle —
        // it scans the file it lives in, and the first run counted itself.
        let connect_call = concat!("UnixStream", "::connect");
        let mut total_connects = 0usize;

        for path in &files {
            let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
                panic!(
                    "gate window unreadable: {} ({e}) — a source gate that \
                     cannot read its input covers nothing",
                    path.display(),
                )
            });

            for (fname, block) in fn_blocks(&src) {
                let connects = block
                    .lines()
                    .filter(live)
                    .filter(|l| l.contains(connect_call))
                    .count();
                if connects == 0 {
                    continue;
                }
                total_connects += connects;

                let waits = block
                    .lines()
                    .filter(live)
                    .filter(|l| l.contains("wait_for_subscribers"))
                    .count();
                assert!(
                    waits >= 1,
                    "{}::{fname} connects to a live broadcaster but never waits for \
                     registration. Connecting completes off the listen backlog; until \
                     the accept loop runs, this client is in no fan-out list and every \
                     frame emitted is dropped. Add a wait_for_subscribers — a sleep is \
                     a race that merely usually wins.",
                    path.display(),
                );
            }
        }

        assert_eq!(
            total_connects, 4,
            "expected 4 live in-process connect sites across the broadcast tests; \
             found {total_connects}. If a file moved or was renamed this gate stopped \
             covering it — fix the window, do not adjust the count to match.",
        );
    }

    /// ORDERING, which no behavioural test here can see: the accept loop
    /// must publish the subscriber count AFTER pushing the client into
    /// the fan-out list.
    ///
    /// Get it backwards and `wait_for_subscribers` resolves on a client
    /// that is not yet reachable, so the wait hands back the very race it
    /// exists to close. A deliberate-breakage run that published before
    /// pushing left all six behavioural tests green: the window it opens is a few
    /// instructions wide, so it fails probabilistically — which is
    /// exactly the property that made the original bug a CI-only flake.
    /// A comment saying "after the push" is not a gate; this is.
    ///
    /// This is a text gate, not a semantic one. It proves the two
    /// statements appear in the right order, not that no future refactor
    /// could reintroduce the gap by other means.
    #[test]
    fn the_accept_loop_publishes_the_count_only_after_the_client_is_reachable() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let accept_loops = [manifest.join("src/infra/broadcast.rs")];
        // Split so this gate's own source cannot satisfy its own needles.
        let push_call = concat!("guard.push", "(stream)");
        let publish_call = concat!("subscribers", ".send(");

        let mut checked = 0usize;
        for path in &accept_loops {
            let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
                panic!(
                    "gate window unreadable: {} ({e}) — a source gate that \
                     cannot read its input covers nothing",
                    path.display(),
                )
            });

            let block = fn_blocks(&src)
                .into_iter()
                .find(|(name, _)| name == "accept_loop")
                .unwrap_or_else(|| {
                    panic!(
                        "no accept_loop found in {} — the window moved and this \
                         gate stopped covering it",
                        path.display(),
                    )
                })
                .1;

            let position = |needle: &str| {
                block
                    .lines()
                    .filter(live)
                    .position(|l| l.contains(needle))
                    .unwrap_or_else(|| {
                        panic!("no live `{needle}` in {}'s accept_loop", path.display())
                    })
            };
            let pushed_at = position(push_call);
            let published_at = position(publish_call);

            assert!(
                pushed_at < published_at,
                "{}'s accept_loop publishes the subscriber count at line {published_at} \
                 of the function but only pushes the client at line {pushed_at}. A waiter \
                 woken by that count would emit into a fan-out list the client is not in \
                 yet, and the frame would be dropped — the exact race the wait exists to \
                 close.",
                path.display(),
            );
            checked += 1;
        }

        assert_eq!(
            checked, 1,
            "expected to check the accept loop; checked {checked}",
        );
    }

    #[tokio::test]
    async fn non_object_payload_returns_error() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("broadcast.sock");
        let b = Broadcaster::spawn(sock).expect("spawn");
        let err = b.emit(Family::Ship, &NotAnObject(1)).unwrap_err();
        assert!(matches!(err, EmitError::PayloadNotObject));
    }
}
