//! Where credentials live, and what that actually protects against.
//!
//! # The honest threat model
//!
//! [`FileCredentialStore`] encrypts with ChaCha20-Poly1305 and keeps its key in
//! a sibling file at mode 0600. An attacker who can read one can read the other,
//! so **this does not stop someone who already has your user account**. Saying
//! otherwise would be theatre.
//!
//! What it does stop is the failure that actually happens: a token sitting in
//! plaintext somewhere it gets copied. Backups. A cloud-synced home directory. A
//! recursive grep pasted into a chat. `git add -A` in the wrong directory. A
//! support bundle. Those are the ways tokens leak in practice, and against every
//! one of them an opaque blob is meaningfully better than a readable string.
//!
//! Two structural guarantees matter more than the cipher, and they hold
//! regardless of backend: credentials live in their own file, never in the
//! roadmap store, and therefore never in a git-mirrored partition and never in
//! `roadmap_export` output. That is enforced by construction — the roadmap types
//! have nowhere to put a secret — and asserted by test.
//!
//! # The OS keychain, and why it took a different mechanism
//!
//! [`KeychainCredentialStore`] is the third implementation this trait existed to
//! make room for. It arrived late, and the reason is worth keeping: three
//! objections stood against it, and all three turned out to be objections to the
//! `keyring` CRATE rather than to the keychain as a destination.
//!
//! The crate's Linux backend needs a D-Bus session and an unlocked login
//! keyring, which a headless MCP server in a container does not have. macOS
//! prompts when a process reads an item another process stored — and that is
//! aimed squarely at this system, where the CLI stores the token and the spawned
//! server reads it. Worse, the macOS ACL is keyed on the reading binary's code
//! signature, and this server is deployed by rebuilding it, so every upgrade
//! would have re-prompted. And a dependency behaving differently on three
//! platforms, in a project that can verify one platform per run, buys a build
//! break someone else discovers.
//!
//! So this backend links nothing. It drives the platform's own credential CLI —
//! `security(1)` on macOS, `secret-tool(1)` on freedesktop — as a subprocess:
//!
//! - Nothing is linked, so no build can break and no library can fail to load.
//!   The three-platform objection has no mechanism left.
//! - An absent tool, or a D-Bus error, is an exit code. [`KeychainCredentialStore::available`]
//!   reads it and the caller falls through to [`FileCredentialStore`]. Headless
//!   degrades instead of failing.
//! - The process touching the keychain is Apple-signed `/usr/bin/security` at
//!   both ends, so the item's ACL never names our binary and no rebuild can
//!   invalidate it. Measured: a store in one process and a read from another
//!   raises no prompt. This is the objection the mechanism inverts rather than
//!   merely survives.
//!
//! # How the secret is handed over, and why macOS gets the worse of two options
//!
//! `secret-tool store` reads the secret from stdin, which is what you want: it
//! never reaches the process table.
//!
//! macOS has no such option, and finding that out took measurement. Three
//! behaviours of `security add-generic-password`, all measured on this platform:
//!
//! 1. `-w` takes its value greedily from argv. Given `-w` immediately followed
//!    by `-U`, it silently stores the literal string `-U` as the password.
//! 2. `-w` in FINAL position prompts instead, and asks for confirmation, so the
//!    value must be written twice. Send it once and the tool prints "passwords
//!    don't match", stores an empty item, and exits 0.
//! 3. That prompt reads through a 128-byte buffer. A longer secret is
//!    **silently truncated to 128 bytes** — exit 0, item created, value wrong.
//!    Measured: 300 bytes in, 128 bytes out.
//!
//! (3) is what settles it. An agent token is a JWT, comfortably past 128 bytes,
//! so the stdin path does not merely leak less — it corrupts the credential and
//! reports success. `security` itself warns that "use of the -p or -w options is
//! insecure", and it is right: the payload is visible in `ps` for the lifetime of
//! the child process. That cost is taken deliberately, because the alternative is
//! not "safer" but "broken", and because a same-user process reading argv for a
//! few milliseconds is already inside the threat model stated at the top of this
//! file — whereas a truncated token is a failure nobody would diagnose.
//!
//! Nothing here is persisted by the shell: the child is spawned directly, so
//! there is no shell and no history. The live round-trip test asserts the secret
//! comes back byte-exact, which is the assertion that caught (3) in the first
//! place — no fake could have.
//!
//! Windows has no built-in retrieval CLI (`cmdkey` stores but will not print),
//! so it resolves to no dialect and falls through to the file store. That is
//! stated rather than papered over; it is the same gap the open Windows-support
//! work covers.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};

use super::domain::{AuthScheme, StoredCredential};

/// Credential storage error.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no credential stored for '{0}' — connect it first")]
    Missing(String),
    #[error("stored credential for '{0}' could not be decrypted: {1}")]
    Undecryptable(String, String),
    #[error("{0}")]
    Invalid(String),
}

/// Where credentials are kept. A trait so the backend is swappable — see the
/// module docs for the three shipped backends and what each one protects.
pub trait CredentialStore: Send + Sync {
    fn load(&self, provider: &str) -> Result<Option<StoredCredential>, CredentialError>;
    fn save(&self, credential: &StoredCredential) -> Result<(), CredentialError>;
    /// Forget a credential. MUST be idempotent — deleting an absent credential
    /// is success, because revoke calls this after the provider may already
    /// have invalidated the token.
    fn delete(&self, provider: &str) -> Result<(), CredentialError>;
    /// Which providers have a credential, for a status surface. Never returns
    /// secrets.
    fn providers(&self) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// Env
// ---------------------------------------------------------------------------

/// The documented fallback: read a secret from the environment.
///
/// Not the product path — a wall of env vars is explicitly the thing to
/// avoid — but it is the one backend that works everywhere, including
/// the headless container where the keychain would not, and CI. Read-only by
/// nature: a process cannot durably set its own environment.
pub struct EnvCredentialStore;

impl EnvCredentialStore {
    /// `THINK_AND_SHIP_TRACKER_TOKEN_<PROVIDER>`, e.g. `…_LINEAR`.
    #[must_use]
    pub fn var_for(provider: &str) -> String {
        format!(
            "THINK_AND_SHIP_TRACKER_TOKEN_{}",
            provider.trim().to_ascii_uppercase().replace('-', "_")
        )
    }
}

impl CredentialStore for EnvCredentialStore {
    fn load(&self, provider: &str) -> Result<Option<StoredCredential>, CredentialError> {
        let Ok(secret) = std::env::var(Self::var_for(provider)) else {
            return Ok(None);
        };
        if secret.trim().is_empty() {
            return Ok(None);
        }
        // An env-supplied secret is always a pasted key: there is nowhere to put
        // refresh material, and nothing to refresh it with.
        let scheme = default_scheme_for(provider);
        Ok(Some(StoredCredential::personal_key(
            provider,
            secret.trim(),
            scheme,
            "1970-01-01T00:00:00+00:00",
        )))
    }

    fn save(&self, _credential: &StoredCredential) -> Result<(), CredentialError> {
        Err(CredentialError::Invalid(
            "credentials supplied by environment variable cannot be saved — \
             set the variable, or connect the provider to store one instead"
                .into(),
        ))
    }

    fn delete(&self, _provider: &str) -> Result<(), CredentialError> {
        Err(CredentialError::Invalid(
            "credentials supplied by environment variable cannot be revoked here — \
             unset the variable and revoke the token with the provider"
                .into(),
        ))
    }

    fn providers(&self) -> Vec<String> {
        // Enumerating the environment would be guesswork; the caller knows
        // which providers it cares about and can probe them.
        Vec::new()
    }
}

/// The scheme a provider uses for a pasted key.
///
/// Linear is the reason this function exists: its personal API key is sent RAW
/// while everyone else's pasted token is a Bearer. A store cannot infer this
/// from the secret's shape without guessing.
#[must_use]
pub fn default_scheme_for(provider: &str) -> AuthScheme {
    match provider.trim().to_ascii_lowercase().as_str() {
        "linear" => AuthScheme::Raw,
        _ => AuthScheme::Bearer,
    }
}

// ---------------------------------------------------------------------------
// Encrypted file
// ---------------------------------------------------------------------------

/// The on-disk shape: an opaque envelope, one file per provider.
#[derive(Debug, Serialize, Deserialize)]
struct SealedFile {
    /// Format marker, so a future backend change is detectable rather than a
    /// mystery decryption failure.
    v: u8,
    /// base64 nonce.
    n: String,
    /// base64 ciphertext.
    c: String,
}

/// Credentials in an encrypted file, one per provider.
///
/// Read the module docs for what the encryption does and does not defend
/// against before relying on it.
pub struct FileCredentialStore {
    dir: PathBuf,
}

impl FileCredentialStore {
    /// `<data_dir>/tracker/credentials/` — beside the tracker's other state and
    /// deliberately nowhere near the roadmap store.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("tracker").join("credentials"),
        }
    }

    fn secret_path(&self, provider: &str) -> PathBuf {
        self.dir
            .join(format!("{}.sealed", provider.trim().to_ascii_lowercase()))
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("key")
    }

    /// Load the file key, creating it on first use.
    ///
    /// Written at 0600 before any secret exists, so there is never a window
    /// where the key is world-readable.
    fn key(&self) -> Result<Key, CredentialError> {
        let path = self.key_path();
        if let Ok(existing) = std::fs::read(&path)
            && existing.len() == 32
        {
            return Ok(*Key::from_slice(&existing));
        }
        std::fs::create_dir_all(&self.dir)?;
        let key = ChaCha20Poly1305::generate_key(&mut OsRng);
        write_private(&path, &key)?;
        Ok(key)
    }

    fn cipher(&self) -> Result<ChaCha20Poly1305, CredentialError> {
        Ok(ChaCha20Poly1305::new(&self.key()?))
    }
}

/// Write a file readable only by its owner.
///
/// The permissions are set at CREATE time on Unix rather than after writing:
/// a chmod afterwards leaves a window in which the secret is world-readable.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

impl CredentialStore for FileCredentialStore {
    fn load(&self, provider: &str) -> Result<Option<StoredCredential>, CredentialError> {
        let path = self.secret_path(provider);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        let sealed: SealedFile = serde_json::from_str(&raw)
            .map_err(|e| CredentialError::Undecryptable(provider.into(), e.to_string()))?;

        let b64 = base64::engine::general_purpose::STANDARD;
        let nonce_bytes = b64
            .decode(&sealed.n)
            .map_err(|e| CredentialError::Undecryptable(provider.into(), e.to_string()))?;
        let ciphertext = b64
            .decode(&sealed.c)
            .map_err(|e| CredentialError::Undecryptable(provider.into(), e.to_string()))?;

        let plaintext = self
            .cipher()?
            .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
            .map_err(|_| {
                CredentialError::Undecryptable(
                    provider.into(),
                    "authentication failed — the file or its key was altered".into(),
                )
            })?;

        serde_json::from_slice(&plaintext)
            .map(Some)
            .map_err(|e| CredentialError::Undecryptable(provider.into(), e.to_string()))
    }

    fn save(&self, credential: &StoredCredential) -> Result<(), CredentialError> {
        let plaintext =
            serde_json::to_vec(credential).map_err(|e| CredentialError::Invalid(e.to_string()))?;
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher()?
            .encrypt(&nonce, plaintext.as_ref())
            .map_err(|e| CredentialError::Invalid(format!("encryption failed: {e}")))?;

        let b64 = base64::engine::general_purpose::STANDARD;
        let sealed = SealedFile {
            v: 1,
            n: b64.encode(nonce),
            c: b64.encode(ciphertext),
        };
        let body =
            serde_json::to_vec(&sealed).map_err(|e| CredentialError::Invalid(e.to_string()))?;
        write_private(&self.secret_path(&credential.provider), &body)?;
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<(), CredentialError> {
        match std::fs::remove_file(self.secret_path(provider)) {
            Ok(()) => Ok(()),
            // Idempotent: revoke deletes after the provider may already be done.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn providers(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".sealed").map(str::to_string)
            })
            .collect();
        out.sort();
        out
    }
}

// ---------------------------------------------------------------------------
// OS keychain
// ---------------------------------------------------------------------------

/// The default keychain service name — the label a human sees in Keychain
/// Access or Seahorse beside the item.
pub const KEYCHAIN_SERVICE: &str = "think-and-ship";

/// One subprocess invocation of a credential CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainCommand {
    /// Program name, resolved on `PATH`.
    pub program: &'static str,
    pub args: Vec<String>,
    /// Written to the child's stdin. This is how `secret-tool` takes a secret —
    /// and how `security` cannot, because its prompt truncates at 128 bytes. See
    /// the module docs.
    pub stdin: Option<String>,
}

/// What a credential CLI said back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainOutcome {
    /// `None` when the child was killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl KeychainOutcome {
    /// Convenience for tests and for the runner.
    #[must_use]
    pub fn new(code: i32, stdout: &str, stderr: &str) -> Self {
        Self {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }
}

/// How a [`KeychainCommand`] actually gets run.
///
/// A trait so tests drive the exit-code classification without touching the
/// developer's real keychain. There is exactly one production implementation
/// ([`ProcessRunner`]); everything else about this backend is pure mapping.
pub trait KeychainRunner: Send + Sync {
    fn run(&self, command: &KeychainCommand) -> std::io::Result<KeychainOutcome>;
}

/// Runs the command as a real subprocess, under a deadline.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl ProcessRunner {
    /// How long a credential CLI gets to answer before this machine is treated
    /// as having no usable keychain.
    ///
    /// A deadline exists because these tools CAN block on a human. A locked
    /// login keychain makes macOS put up an unlock dialog; libsecret can wait on
    /// a prompter. The first caller here is the MCP server at startup, so an
    /// unbounded wait means a locked machine gets a server that never finishes
    /// booting — and an agent host that reports nothing at all, because the
    /// server never got far enough to say anything.
    ///
    /// Generous relative to the work: the local round trips measured in single
    /// -digit milliseconds, so this only ever fires on a genuine block.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// How often to check whether the child is done.
    const POLL: std::time::Duration = std::time::Duration::from_millis(10);
}

impl KeychainRunner for ProcessRunner {
    fn run(&self, command: &KeychainCommand) -> std::io::Result<KeychainOutcome> {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let mut child = Command::new(command.program)
            .args(&command.args)
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(input) = &command.stdin {
            child
                .stdin
                .as_mut()
                .expect("stdin was piped")
                .write_all(input.as_bytes())?;
        }
        // Dropping our handle closes the pipe, so a tool that keeps reading
        // sees EOF instead of blocking us both forever.
        drop(child.stdin.take());

        // Poll rather than block. Output is a single secret either way, far
        // under the pipe buffer, so nothing can be waiting on us to drain it
        // while we wait on the exit.
        let deadline = std::time::Instant::now() + Self::TIMEOUT;
        while child.try_wait()?.is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(KeychainOutcome {
                    // No exit code: classification reads this as unusable, and
                    // the caller falls through to the encrypted file store.
                    code: None,
                    stdout: String::new(),
                    stderr: format!(
                        "`{}` did not answer within {}s — it may be waiting on a \
                         keychain unlock prompt",
                        command.program,
                        Self::TIMEOUT.as_secs(),
                    ),
                });
            }
            std::thread::sleep(Self::POLL);
        }

        let out = child.wait_with_output()?;
        Ok(KeychainOutcome {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Which platform CLI, and the verb spellings it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeychainDialect {
    /// macOS `security(1)`, against the user's login keychain.
    MacOsSecurity,
    /// freedesktop `secret-tool(1)`, over libsecret's Secret Service.
    SecretTool,
}

/// What a lookup found, before it becomes a `Result<Option<_>>`.
///
/// The three-way split is the whole point: "no such item" and "this machine has
/// no usable keychain" are different answers, and collapsing them is how a
/// headless container would silently look like a machine that was never
/// connected.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Lookup {
    Found(String),
    Absent,
    Unusable(String),
}

impl KeychainDialect {
    /// The dialect for the platform this binary was built for, if there is one.
    ///
    /// `None` on Windows: `cmdkey` stores a credential but will not print one
    /// back, and there is no other built-in retrieval CLI, so there is nothing
    /// to drive. Callers fall through to [`FileCredentialStore`].
    #[must_use]
    pub fn for_this_platform() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::MacOsSecurity)
        } else if cfg!(target_os = "windows") {
            None
        } else {
            Some(Self::SecretTool)
        }
    }

    fn program(self) -> &'static str {
        match self {
            Self::MacOsSecurity => "security",
            Self::SecretTool => "secret-tool",
        }
    }

    fn store(self, service: &str, account: &str, secret: &str) -> KeychainCommand {
        match self {
            // `-U` updates in place, so a reconnect is a plain re-store rather
            // than delete-then-add with a window where nothing is stored.
            //
            // `-w VALUE` on argv, NOT the stdin prompt, and the module docs
            // explain why at length: the prompt reads through a 128-byte buffer
            // and silently truncates anything longer, which for a JWT means a
            // corrupted credential reported as success. `-w` also comes LAST
            // apart from its own value, because it consumes whatever follows it
            // — `-w` then `-U` stores the string "-U" as the password.
            Self::MacOsSecurity => KeychainCommand {
                program: self.program(),
                args: vec![
                    "add-generic-password".into(),
                    "-s".into(),
                    service.into(),
                    "-a".into(),
                    account.into(),
                    "-U".into(),
                    "-w".into(),
                    secret.into(),
                ],
                stdin: None,
            },
            Self::SecretTool => KeychainCommand {
                program: self.program(),
                args: vec![
                    "store".into(),
                    "--label".into(),
                    format!("{service}: {account}"),
                    "service".into(),
                    service.into(),
                    "account".into(),
                    account.into(),
                ],
                stdin: Some(format!("{secret}\n")),
            },
        }
    }

    fn lookup(self, service: &str, account: &str) -> KeychainCommand {
        match self {
            Self::MacOsSecurity => KeychainCommand {
                program: self.program(),
                args: vec![
                    "find-generic-password".into(),
                    "-s".into(),
                    service.into(),
                    "-a".into(),
                    account.into(),
                    "-w".into(),
                ],
                stdin: None,
            },
            Self::SecretTool => KeychainCommand {
                program: self.program(),
                args: vec![
                    "lookup".into(),
                    "service".into(),
                    service.into(),
                    "account".into(),
                    account.into(),
                ],
                stdin: None,
            },
        }
    }

    fn erase(self, service: &str, account: &str) -> KeychainCommand {
        match self {
            Self::MacOsSecurity => KeychainCommand {
                program: self.program(),
                args: vec![
                    "delete-generic-password".into(),
                    "-s".into(),
                    service.into(),
                    "-a".into(),
                    account.into(),
                ],
                stdin: None,
            },
            Self::SecretTool => KeychainCommand {
                program: self.program(),
                args: vec![
                    "clear".into(),
                    "service".into(),
                    service.into(),
                    "account".into(),
                    account.into(),
                ],
                stdin: None,
            },
        }
    }

    /// Classify a lookup's result.
    ///
    /// Measured on macOS: exit 0 with the secret on stdout, exit 44 for a
    /// missing item. `secret-tool`'s missing-item code varies by build — 0 with
    /// empty stdout in some, 1 in others — so absent-vs-unusable is decided by
    /// STDERR, which is where libsecret's "Cannot autolaunch D-Bus without X11
    /// $DISPLAY" lands. Reading the code alone would classify a headless
    /// container as "not connected yet".
    fn classify(self, outcome: &KeychainOutcome) -> Lookup {
        match self {
            Self::MacOsSecurity => match outcome.code {
                Some(0) => Lookup::Found(outcome.stdout.trim_end_matches('\n').to_string()),
                Some(44) => Lookup::Absent,
                _ => Lookup::Unusable(diagnostic(outcome)),
            },
            Self::SecretTool => {
                let secret = outcome.stdout.trim_end_matches('\n');
                match outcome.code {
                    Some(0) if !secret.is_empty() => Lookup::Found(secret.to_string()),
                    Some(0 | 1) if outcome.stderr.trim().is_empty() => Lookup::Absent,
                    _ => Lookup::Unusable(diagnostic(outcome)),
                }
            }
        }
    }

    /// Whether an erase succeeded. Deleting an absent item is success — the
    /// trait requires idempotency, and macOS reports the same 44 it uses for a
    /// missing lookup.
    fn erased(self, outcome: &KeychainOutcome) -> Result<(), String> {
        match (self, outcome.code) {
            (Self::MacOsSecurity, Some(0 | 44)) | (Self::SecretTool, Some(0 | 1)) => Ok(()),
            _ => Err(diagnostic(outcome)),
        }
    }
}

/// The best one-line explanation available from a failed invocation.
fn diagnostic(outcome: &KeychainOutcome) -> String {
    let stderr = outcome.stderr.trim();
    if !stderr.is_empty() {
        return stderr.lines().next().unwrap_or(stderr).to_string();
    }
    match outcome.code {
        Some(code) => format!("exited {code} with no message"),
        None => "killed by a signal".to_string(),
    }
}

/// Credentials in the OS keychain, driven through the platform's own CLI.
///
/// Read the module docs before reaching for this: it is preferred over
/// [`FileCredentialStore`] where it works, and [`Self::available`] is how a
/// caller finds out whether that is here.
pub struct KeychainCredentialStore {
    service: String,
    dialect: KeychainDialect,
    runner: std::sync::Arc<dyn KeychainRunner>,
}

impl KeychainCredentialStore {
    /// The store for this platform, or `None` where there is no CLI to drive.
    ///
    /// `None` is not an error and callers must not treat it as one — it is the
    /// documented Windows answer, and the signal to use the file store.
    #[must_use]
    pub fn for_this_platform(service: &str) -> Option<Self> {
        Some(Self::with_runner(
            service,
            KeychainDialect::for_this_platform()?,
            std::sync::Arc::new(ProcessRunner),
        ))
    }

    /// A store over an explicit dialect and runner, so a test can drive every
    /// platform's spellings and exit codes without owning that platform or
    /// touching a real keychain.
    #[must_use]
    pub fn with_runner(
        service: &str,
        dialect: KeychainDialect,
        runner: std::sync::Arc<dyn KeychainRunner>,
    ) -> Self {
        Self {
            service: service.to_string(),
            dialect,
            runner,
        }
    }

    /// Whether this machine actually has a working keychain.
    ///
    /// Answered by looking up an item that will not exist: a clean "absent" is
    /// proof the tool ran and the keyring answered, while a missing binary or a
    /// D-Bus refusal shows up as unusable. Cheap, read-only, and it stores
    /// nothing.
    #[must_use]
    pub fn available(&self) -> bool {
        let probe = self
            .dialect
            .lookup(&self.service, "think-and-ship-availability-probe");
        match self.runner.run(&probe) {
            Ok(outcome) => !matches!(self.dialect.classify(&outcome), Lookup::Unusable(_)),
            // A missing binary lands here as NotFound.
            Err(_) => false,
        }
    }

    fn account(provider: &str) -> String {
        provider.trim().to_ascii_lowercase()
    }
}

impl CredentialStore for KeychainCredentialStore {
    fn load(&self, provider: &str) -> Result<Option<StoredCredential>, CredentialError> {
        let account = Self::account(provider);
        let outcome = self
            .runner
            .run(&self.dialect.lookup(&self.service, &account))?;
        match self.dialect.classify(&outcome) {
            Lookup::Absent => Ok(None),
            Lookup::Unusable(why) => Err(CredentialError::Invalid(format!(
                "the OS keychain could not be read for '{account}': {why}"
            ))),
            // The keychain holds the full StoredCredential as JSON, not the
            // bare secret: grant kind, scheme and expiry are what let a token
            // be renewed and revoked, and a backend that dropped them would
            // silently make every credential it stored unrenewable.
            Lookup::Found(payload) => serde_json::from_str(&payload)
                .map(Some)
                .map_err(|e| CredentialError::Undecryptable(account, e.to_string())),
        }
    }

    fn save(&self, credential: &StoredCredential) -> Result<(), CredentialError> {
        let account = Self::account(&credential.provider);
        let payload = serde_json::to_string(credential)
            .map_err(|e| CredentialError::Invalid(e.to_string()))?;
        let outcome = self
            .runner
            .run(&self.dialect.store(&self.service, &account, &payload))?;
        if outcome.code == Some(0) {
            return Ok(());
        }
        Err(CredentialError::Invalid(format!(
            "the OS keychain refused to store '{account}': {}",
            diagnostic(&outcome)
        )))
    }

    fn delete(&self, provider: &str) -> Result<(), CredentialError> {
        let account = Self::account(provider);
        let outcome = self
            .runner
            .run(&self.dialect.erase(&self.service, &account))?;
        self.dialect.erased(&outcome).map_err(|why| {
            CredentialError::Invalid(format!(
                "the OS keychain refused to forget '{account}': {why}"
            ))
        })
    }

    /// Always empty, deliberately.
    ///
    /// Neither CLI enumerates by service without cost: macOS would need
    /// `dump-keychain`, which prompts for the keychain password and is exactly
    /// the interactive failure this backend exists to avoid. A caller that needs
    /// to know which profiles exist reads them from the config that names them,
    /// which is where the profile name lives anyway. Returning an empty list is
    /// honest; inventing a side index would be a second copy of state.
    fn providers(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const NOW: &str = "2026-07-26T00:00:00Z";

    fn store(dir: &TempDir) -> FileCredentialStore {
        FileCredentialStore::new(dir.path())
    }

    #[test]
    fn a_saved_credential_round_trips() {
        let dir = TempDir::new().expect("tempdir");
        let s = store(&dir);
        let cred = StoredCredential::personal_key("linear", "lin_api_secret", AuthScheme::Raw, NOW);
        s.save(&cred).expect("save");

        let loaded = s.load("linear").expect("load").expect("present");
        assert_eq!(loaded, cred);
        assert_eq!(loaded.as_credential().header_value(), "lin_api_secret");
    }

    /// The point of the whole file: the secret must not be readable in it.
    #[test]
    fn the_secret_is_not_present_in_the_file_bytes() {
        let dir = TempDir::new().expect("tempdir");
        let s = store(&dir);
        s.save(&StoredCredential::personal_key(
            "github",
            "ghp_verysecretvalue",
            AuthScheme::Bearer,
            NOW,
        ))
        .expect("save");

        let raw = std::fs::read_to_string(s.secret_path("github")).expect("read");
        assert!(
            !raw.contains("ghp_verysecretvalue"),
            "the secret is readable on disk: {raw}"
        );
        // …and neither is the provider's own metadata leaking the shape.
        assert!(!raw.contains("personal_key"));
    }

    /// Tampering must fail loudly rather than yielding a wrong credential —
    /// that is what the AEAD's authentication tag is for.
    #[test]
    fn a_tampered_file_fails_authentication_rather_than_decoding() {
        let dir = TempDir::new().expect("tempdir");
        let s = store(&dir);
        s.save(&StoredCredential::personal_key(
            "github",
            "secret",
            AuthScheme::Bearer,
            NOW,
        ))
        .expect("save");

        let path = s.secret_path("github");
        let mut sealed: SealedFile =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        // Flip one byte of ciphertext.
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut bytes = b64.decode(&sealed.c).expect("decode");
        bytes[0] ^= 0xff;
        sealed.c = b64.encode(&bytes);
        std::fs::write(&path, serde_json::to_vec(&sealed).expect("ser")).expect("write");

        let err = s.load("github").expect_err("must refuse");
        assert!(matches!(err, CredentialError::Undecryptable(..)));
    }

    #[cfg(unix)]
    #[test]
    fn the_key_and_the_secret_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().expect("tempdir");
        let s = store(&dir);
        s.save(&StoredCredential::personal_key(
            "github",
            "secret",
            AuthScheme::Bearer,
            NOW,
        ))
        .expect("save");

        for path in [s.key_path(), s.secret_path("github")] {
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{} is not owner-only: {:o}",
                path.display(),
                mode & 0o777
            );
        }
    }

    /// Delete must be idempotent because revoke calls it after the provider may
    /// already have invalidated the token.
    #[test]
    fn delete_is_idempotent() {
        let dir = TempDir::new().expect("tempdir");
        let s = store(&dir);
        s.save(&StoredCredential::personal_key(
            "github",
            "secret",
            AuthScheme::Bearer,
            NOW,
        ))
        .expect("save");

        s.delete("github").expect("first delete");
        s.delete("github").expect("second delete must also succeed");
        assert!(s.load("github").expect("load").is_none());
    }

    #[test]
    fn providers_lists_what_is_stored_without_secrets() {
        let dir = TempDir::new().expect("tempdir");
        let s = store(&dir);
        for p in ["linear", "github"] {
            s.save(&StoredCredential::personal_key(
                p,
                "secret",
                AuthScheme::Bearer,
                NOW,
            ))
            .expect("save");
        }
        assert_eq!(s.providers(), vec!["github", "linear"]);
    }

    #[test]
    fn a_missing_credential_is_none_not_an_error() {
        let dir = TempDir::new().expect("tempdir");
        assert!(store(&dir).load("never-connected").expect("load").is_none());
    }

    /// Linear is the reason the scheme cannot be inferred from the secret.
    #[test]
    fn the_default_scheme_is_per_provider_not_guessed_from_the_token() {
        assert_eq!(default_scheme_for("linear"), AuthScheme::Raw);
        assert_eq!(default_scheme_for("github"), AuthScheme::Bearer);
        assert_eq!(default_scheme_for("jira"), AuthScheme::Bearer);
    }

    #[test]
    fn the_env_fallback_reads_a_provider_scoped_variable() {
        assert_eq!(
            EnvCredentialStore::var_for("linear"),
            "THINK_AND_SHIP_TRACKER_TOKEN_LINEAR"
        );
        // An absent variable is "nothing stored", not an error.
        assert!(
            EnvCredentialStore
                .load("provider-that-has-no-var-set")
                .expect("load")
                .is_none()
        );
    }

    /// The env backend is read-only by nature, and says so usefully rather than
    /// silently doing nothing.
    #[test]
    fn the_env_fallback_refuses_to_save_or_revoke_with_a_useful_message() {
        let cred = StoredCredential::personal_key("linear", "k", AuthScheme::Raw, NOW);
        let save = EnvCredentialStore.save(&cred).expect_err("must refuse");
        assert!(save.to_string().contains("environment variable"));
        let del = EnvCredentialStore
            .delete("linear")
            .expect_err("must refuse");
        assert!(
            del.to_string()
                .contains("revoke the token with the provider")
        );
    }

    // -----------------------------------------------------------------------
    // The real keychain
    // -----------------------------------------------------------------------

    /// The one test that touches a REAL keychain, and therefore the one that is
    /// `#[ignore]`d.
    ///
    /// Run it deliberately:
    ///
    /// ```text
    /// cargo test -p think-and-ship --lib the_real_os_keychain -- --ignored --nocapture
    /// ```
    ///
    /// It is opt-in rather than part of the gate for two reasons. It writes to
    /// the developer's login keychain, and a suite that does that without being
    /// asked is a suite nobody trusts. And it is a one-platform proof: on a
    /// machine with no keyring it would report unavailable and assert nothing,
    /// so as a gate it would pass by being skipped — the failure mode that makes
    /// an absence test worthless.
    ///
    /// What it proves that the fake cannot: that the argv and stdin spellings in
    /// [`KeychainDialect`] are the ones the real tool accepts. The fake can only
    /// confirm they are the ones this module builds.
    #[test]
    #[ignore = "writes to the real login keychain; run explicitly"]
    fn the_real_os_keychain_round_trips_a_credential() {
        let Some(store) = KeychainCredentialStore::for_this_platform("think-and-ship-selftest")
        else {
            eprintln!("no keychain CLI on this platform — nothing to prove here");
            return;
        };
        if !store.available() {
            eprintln!("no usable keyring on this machine — nothing to prove here");
            return;
        }

        // Namespaced by pid so a concurrent run cannot collide, and cleaned up
        // at the end whatever happens above it.
        let provider = format!("selftest-{}", std::process::id());
        let secret = "live-keychain-round-trip-secret";

        assert!(
            store.load(&provider).expect("a clean lookup").is_none(),
            "precondition: this provider is not already in the keychain",
        );

        let cred = StoredCredential::personal_key(&provider, secret, AuthScheme::Bearer, NOW);
        store
            .save(&cred)
            .expect("the real keychain accepts a store");

        let loaded = store
            .load(&provider)
            .expect("the real keychain accepts a lookup")
            .expect("what was stored is found");
        assert_eq!(
            loaded.access.expose(),
            secret,
            "the secret survives the round trip intact — this is what catches a \
             confirm-prompt mismatch, which stores an EMPTY item and still exits 0",
        );
        assert_eq!(loaded.scheme, AuthScheme::Bearer, "the scheme survives too");

        // Re-store: `-U` must update in place rather than fail on a duplicate.
        let updated = StoredCredential::personal_key(&provider, "second", AuthScheme::Bearer, NOW);
        store.save(&updated).expect("a re-store updates in place");
        assert_eq!(
            store.load(&provider).unwrap().unwrap().access.expose(),
            "second",
        );

        store.delete(&provider).expect("delete succeeds");
        assert!(
            store.load(&provider).unwrap().is_none(),
            "the item is gone after delete",
        );
        store
            .delete(&provider)
            .expect("deleting an absent item is success, as the trait requires");
    }
}
