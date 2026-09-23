//! SSH device transport (Zed/Orca pattern, local-first).
//!
//! The UI stays local; the remote runs `zeron rpc-stdio` and speaks the same
//! ndjson RPC envelopes over the ssh channel's stdio. Like Zed we shell out to
//! the system `ssh` binary so `~/.ssh/config`, `known_hosts`
//! (`StrictHostKeyChecking`, TOFU fingerprint), agent forwarding, and jump
//! hosts (`-J`) keep working without reimplementation. Like Orca, targets
//! persist device-locally (`{data_dir}/ssh-targets.json`) and are tested
//! before saving.
//!
//! Wire format: one JSON object per line, identical to [`crate::memory_client`]
//! and the WebSocket transport — [`crate::RpcClient`] / [`crate::serve_connection`]
//! stay transport-agnostic.
//!
//! `SshTarget::upload_binary_over_ssh` skips the PATH assumption entirely: the
//! remote platform is read from `uname`, a matching build is obtained (this
//! executable when the OS/arch lines up, otherwise the released headless
//! artifact for the remote), and it is streamed over the ssh channel into
//! `~/.zeron/remote/` before `zeron rpc-stdio` is invoked by full path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::{RpcClient, RpcError};

/// Connection timeout for `ssh -G` probes and `zeron --version` checks.
pub const SSH_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// One SSH device target. Device-local (never synced through Loro docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshTarget {
    /// Stable id (`ssh-<8 hex>`).
    pub id: String,
    /// Host as typed (`example.com`, `192.168.1.10`, or an `~/.ssh/config` alias).
    pub host: String,
    /// Defaults to the local username when `None` (ssh default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Defaults to 22.
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// Optional `-i` identity file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// Extra ssh args appended verbatim (`-J`, `-o`, `-L` …). Never includes
    /// secrets — passwords come from the interactive ssh prompt / agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    /// UI label. Defaults to `destination()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// When true, install a matching zeron build on the remote over ssh (into
    /// `~/.zeron/remote/`) instead of requiring `zeron` on the remote PATH.
    /// Same-OS remotes receive this exact executable; cross-OS remotes receive
    /// the released headless build for their OS/arch (restricted-egress hosts).
    #[serde(default)]
    pub upload_binary_over_ssh: bool,
}

fn default_ssh_port() -> u16 {
    22
}

impl SshTarget {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            id: format!("ssh-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            host: host.into(),
            username: None,
            port: 22,
            identity_file: None,
            extra_args: Vec::new(),
            nickname: None,
            upload_binary_over_ssh: false,
        }
    }

    /// `user@host:port` label (Orca edit-dialog style).
    pub fn destination(&self) -> String {
        let host = if self.port == 22 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        match self.username.as_deref().filter(|u| !u.is_empty()) {
            Some(user) => format!("{user}@{host}"),
            None => host,
        }
    }

    pub fn display_name(&self) -> String {
        self.nickname
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.destination())
    }

    fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("host is empty".into());
        }
        if self.host.contains(char::is_whitespace)
            || self.host.contains(';')
            || self.host.contains('&')
            || self.host.contains('|')
            || self.host.contains('`')
        {
            return Err(format!("unsafe host {:?}", self.host));
        }
        if let Some(user) = &self.username
            && (user.contains(char::is_whitespace) || user.contains('@') || user.contains(':'))
        {
            return Err(format!("unsafe username {user:?}"));
        }
        for arg in &self.extra_args {
            if arg.contains('\n') || arg.contains('\r') || arg.contains('\0') {
                return Err("ssh arg contains a line break".into());
            }
        }
        Ok(())
    }
}

/// Parse `ssh://[user@]host[:port][/path-ignored]` (Zed CLI style) or
/// `[user@]host[:port]`. Paths are ignored — the remote always runs
/// `zeron rpc-stdio`; workspaces are picked with the existing space picker.
pub fn parse_ssh_target(input: &str) -> Result<SshTarget, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty ssh target".into());
    }
    let without_scheme = input
        .strip_prefix("ssh://")
        .or_else(|| input.strip_prefix("zed://ssh/"))
        .unwrap_or(input);
    // Drop any path after the authority.
    let authority = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .trim();
    // Drop optional `password@` — we never persist passwords (Zed parity:
    // passwords may appear on a one-shot CLI URL, never in settings).
    let authority = authority.rsplit_once('@').map_or_else(
        || (None, authority),
        |(left, right)| {
            if right.contains(':') || !left.contains(':') {
                (Some(left), right)
            } else {
                // `user:password@host` — keep user, drop password.
                let user = left.split(':').next().unwrap_or(left);
                (Some(user), right)
            }
        },
    );
    let (username, hostport) = authority;
    // IPv6 `[::1]:22` or plain `host:port`.
    let (host, port) = if let Some(bracketed) = hostport.strip_prefix('[')
        && let Some((inner, rest)) = bracketed.split_once(']')
    {
        let port = rest
            .strip_prefix(':')
            .map(str::parse::<u16>)
            .transpose()
            .map_err(|_| format!("invalid port in {input:?}"))?
            .unwrap_or(22);
        (inner.to_string(), port)
    } else if let Some((h, p)) = hostport.rsplit_once(':')
        && !h.is_empty()
        && let Ok(port) = p.parse::<u16>()
    {
        (h.to_string(), port)
    } else {
        (hostport.to_string(), 22)
    };
    if host.is_empty() {
        return Err(format!("no host in {input:?}"));
    }
    if port == 0 {
        return Err(format!("invalid port in {input:?}"));
    }
    let mut target = SshTarget::new(host);
    target.port = port;
    target.username = username.filter(|u| !u.is_empty()).map(str::to_string);
    target.validate()?;
    Ok(target)
}

/// Base `ssh` argv (before the remote command): port, identity, extras, then
/// `destination`. Control characters are rejected — argv is never a shell string.
pub fn ssh_base_args(target: &SshTarget) -> Result<Vec<String>, String> {
    target.validate()?;
    let mut args = Vec::new();
    if target.port != 22 {
        args.push("-p".to_string());
        args.push(target.port.to_string());
    }
    if let Some(key) = target.identity_file.as_deref().filter(|k| !k.is_empty()) {
        if key.contains('\n') || key.contains('\r') || key.contains('\0') {
            return Err("identity file contains a line break".into());
        }
        args.push("-i".to_string());
        args.push(key.to_string());
    }
    args.extend(target.extra_args.iter().cloned());
    args.push(target.destination());
    Ok(args)
}

/// Full argv: `ssh <base> -- <remote command...>`. `--` keeps a hostile
/// hostname from being parsed as a flag.
pub fn ssh_command(target: &SshTarget, remote_cmd: &str) -> Result<(String, Vec<String>), String> {
    if remote_cmd.trim().is_empty() {
        return Err("remote command is empty".into());
    }
    let mut args = ssh_base_args(target)?;
    args.push("--".to_string());
    args.push(remote_cmd.to_string());
    Ok(("ssh".to_string(), args))
}

/// Multiplexing flags (Zed `ControlMaster` parity, Orca `Reuse SSH connection`
/// parity). Appended only for long-lived RPC sessions, not one-shot checks.
pub fn control_master_args(socket_path: &str) -> Vec<String> {
    vec![
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        format!("ControlPath={socket_path}"),
        "-o".to_string(),
        "ControlPersist=300s".to_string(),
    ]
}

/// Probe connectivity + remote `zeron` presence (`<zeron> --version`, where
/// `<zeron>` is `zeron` from PATH or the uploaded binary). Returns the version
/// line on success. Host-key verification, password, and key-passphrase prompts
/// all come from the user's own ssh (terminal-inherited), exactly like Zed.
pub async fn check_target(target: &SshTarget, edge_url: &str) -> Result<String, String> {
    let binary = remote_zeron_command(target, edge_url).await?;
    let output = ssh_capture(target, &format!("{binary} --version"), SSH_CHECK_TIMEOUT).await?;
    if !output.status.success() {
        let hint = first_stderr(&output);
        let hint = if output.status.code() == Some(127) || hint.contains("command not found") {
            format!(
                "{hint} (install zeron on {} or enable upload for this device)",
                target.host
            )
        } else {
            hint
        };
        return Err(format!("{}: {hint}", target.destination()));
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .next()
        .unwrap_or("zeron (version unknown)")
        .to_string();
    Ok(line)
}

/// One-shot ssh command capturing stdout/stderr (stdin closed) under a timeout.
async fn ssh_capture(
    target: &SshTarget,
    remote_cmd: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let (prog, args) = ssh_command(target, remote_cmd)?;
    tokio::time::timeout(timeout, async {
        tokio::process::Command::new(&prog)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("failed to spawn ssh (is it on PATH?): {e}"))
    })
    .await
    .map_err(|_| format!("timed out reaching {}", target.destination()))?
}

/// First stderr line (or the exit status when stderr is empty).
fn first_stderr(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        format!("ssh exited {}", output.status)
    } else {
        trimmed.lines().next().unwrap_or(trimmed).to_string()
    }
}

/// Open an RPC session: `ssh <base> -- <zeron> rpc-stdio`, piping ndjson frames
/// over the channel's stdio. Returns the client plus a handle that kills the
/// child on drop.
pub async fn connect_stdio(
    target: &SshTarget,
    edge_url: &str,
) -> Result<(RpcClient, SshHandle), RpcError> {
    let binary = remote_zeron_command(target, edge_url)
        .await
        .map_err(RpcError::Transport)?;
    connect_stdio_to(target, &format!("{binary} rpc-stdio")).await
}

/// [`connect_stdio`] with an explicit remote command (tests / custom binary paths).
pub async fn connect_stdio_to(
    target: &SshTarget,
    remote_cmd: &str,
) -> Result<(RpcClient, SshHandle), RpcError> {
    let (prog, args) = ssh_command(target, remote_cmd).map_err(RpcError::Transport)?;
    let mut child: Child = tokio::process::Command::new(&prog)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| RpcError::Transport(format!("spawn ssh: {e}")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| RpcError::Transport("ssh stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RpcError::Transport("ssh stdout unavailable".into()))?;
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<String>(256);
    let (in_tx, in_rx) = tokio::sync::mpsc::channel::<String>(256);
    // client -> remote stdin (one line per frame + newline)
    tokio::spawn(async move {
        let mut stdin = stdin;
        use tokio::io::AsyncWriteExt as _;
        let mut out_rx = out_rx;
        while let Some(frame) = out_rx.recv().await {
            if stdin.write_all(frame.as_bytes()).await.is_err() {
                break;
            }
            if stdin.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });
    // remote stdout -> client (line-split; a loud `.bashrc` corrupts the
    // protocol exactly like Zed's "Starting proxy…" hang — the remote must
    // keep non-interactive shells quiet).
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt as _, BufReader};
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            if in_tx.send(line).await.is_err() {
                break;
            }
        }
    });
    let client = RpcClient::new(out_tx, in_rx);
    Ok((client, SshHandle { child }))
}

/// Owns the `ssh` child for an RPC session; killing on drop mirrors Zed's
/// master-process teardown.
pub struct SshHandle {
    child: Child,
}

impl SshHandle {
    /// Wait for the remote to exit (yields the exit status).
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    pub fn kill(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Drop for SshHandle {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---------------------------------------------------------------------------
// Remote binary upload (`upload_binary_over_ssh`)
// ---------------------------------------------------------------------------

/// Remote directory uploaded binaries live in. Files are versioned, so two
/// clients on different versions (or a running remote) never clobber each other.
const REMOTE_BINARY_ROOT: &str = "$HOME/.zeron/remote";

/// Generous cap for pushing the (tens-of-MB) binary over a slow link.
const SSH_UPLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Remote machine identity from `uname`, mapped onto release artifact keys
/// (`linux`/`macos`/`windows`, `x86_64`/`aarch64`/`arm64`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePlatform {
    pub os: String,
    pub arch: String,
}

impl RemotePlatform {
    /// True when this build runs on the remote as-is — the primary case, where
    /// `current_exe` is uploaded instead of downloading a release.
    pub fn matches_local(&self) -> bool {
        let (os, arch) = zeron_update::platform_key();
        self.os == os && self.arch == arch
    }
}

/// Map `uname -s` / `uname -m` output to release platform keys.
pub fn parse_uname(os: &str, arch: &str) -> Result<RemotePlatform, String> {
    let os = os.trim();
    let arch = arch.trim();
    let os = if os.starts_with("Linux") {
        "linux"
    } else if os.starts_with("Darwin") {
        "macos"
    } else if os.starts_with("MINGW")
        || os.starts_with("MSYS")
        || os.starts_with("CYGWIN")
        || os.starts_with("Windows")
    {
        "windows"
    } else {
        return Err(format!("unsupported remote OS {os:?}"));
    };
    let arch = match arch {
        "x86_64" | "amd64" | "x64" => "x86_64",
        "aarch64" | "arm64" if os == "macos" => "arm64",
        "aarch64" | "arm64" => "aarch64",
        "armv7l" => "armv7l",
        other => return Err(format!("unsupported remote architecture {other:?}")),
    };
    Ok(RemotePlatform {
        os: os.to_string(),
        arch: arch.to_string(),
    })
}

/// `zeron-<ver>-<os>-<arch>.tar.gz` for the remote platform — the artifact the
/// curl|sh installer and the auto-updater fetch (see `scripts/package-linux.sh`).
pub fn remote_artifact(version: &str, platform: &RemotePlatform) -> String {
    format!(
        "zeron-{version}-{}-{}.tar.gz",
        platform.os, platform.arch
    )
}

/// Remote path of the uploaded binary for `version`.
pub fn remote_binary_path(version: &str, platform: &RemotePlatform) -> String {
    format!(
        "{REMOTE_BINARY_ROOT}/zeron-{version}-{}-{}",
        platform.os, platform.arch
    )
}

/// The command the remote runs to start zeron: bare `zeron` from PATH, or the
/// uploaded binary when the target opts into `upload_binary_over_ssh`.
pub async fn remote_zeron_command(target: &SshTarget, edge_url: &str) -> Result<String, String> {
    if !target.upload_binary_over_ssh {
        return Ok("zeron".to_string());
    }
    if edge_url.trim().is_empty() {
        return Err(
            "this device uploads a binary, but no release URL is configured".to_string(),
        );
    }
    ensure_remote_binary(target, edge_url).await
}

/// Resolve (and install once per target) the binary the remote should run.
/// Same-OS remotes get this build's own executable; cross-OS remotes get the
/// released headless artifact for their OS/arch, downloaded locally and streamed
/// over the ssh channel — so a restricted-egress host never contacts the edge.
pub async fn ensure_remote_binary(target: &SshTarget, edge_url: &str) -> Result<String, String> {
    let cache = remote_binary_cache();
    let mut installed = cache.lock().await;
    if let Some(path) = installed.get(&target.id) {
        return Ok(path.clone());
    }
    let platform = probe_remote_platform(target).await?;
    let (candidates, manifest) = remote_binary_candidates(edge_url, &platform).await?;
    // A previous process may have uploaded one already — probe the candidates
    // before pushing tens of MB again.
    for version in &candidates {
        let path = remote_binary_path(version, &platform);
        if remote_binary_present(target, &path).await {
            installed.insert(target.id.clone(), path.clone());
            return Ok(path);
        }
    }
    let staged =
        stage_binary_for_remote(edge_url, manifest.as_ref(), &candidates, &platform).await?;
    let remote_path = remote_binary_path(&staged.version, &platform);
    upload_binary(target, &staged.path, &remote_path).await?;
    installed.insert(target.id.clone(), remote_path.clone());
    Ok(remote_path)
}

fn remote_binary_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn probe_remote_platform(target: &SshTarget) -> Result<RemotePlatform, String> {
    let output = ssh_capture(target, "uname -s; uname -m", SSH_CHECK_TIMEOUT).await?;
    if !output.status.success() {
        return Err(format!(
            "{}: reading remote uname: {}",
            target.destination(),
            first_stderr(&output)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines().map(str::trim).filter(|line| !line.is_empty());
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    parse_uname(os, arch)
}

async fn remote_binary_present(target: &SshTarget, remote_path: &str) -> bool {
    matches!(
        ssh_capture(target, &format!("test -x \"{remote_path}\""), SSH_CHECK_TIMEOUT).await,
        Ok(output) if output.status.success()
    )
}

/// A binary staged locally, ready to stream to the remote.
struct StagedBinary {
    path: PathBuf,
    version: String,
    /// Present for downloads — cleans up the temp dir when dropped.
    _dir: Option<tempfile::TempDir>,
}

/// Ordered candidate versions for the remote binary. A same-OS remote has one
/// candidate (this build). A cross-OS remote fetches the release manifest; since
/// it carries checksums only for the newest release, a checksum-covered version
/// comes first — but the remote is never silently downgraded below this client,
/// and pre-manifest releases (no file list) still fall back to the pinned
/// version (unverified, which `download_release_file` warns about).
async fn remote_binary_candidates(
    edge_url: &str,
    platform: &RemotePlatform,
) -> Result<(Vec<String>, Option<zeron_update::Manifest>), String> {
    let current = zeron_update::current_version().to_string();
    if platform.matches_local() {
        return Ok((vec![current], None));
    }
    let manifest = zeron_update::fetch_latest(edge_url)
        .await
        .map_err(|err| format!("fetching release metadata from {edge_url}: {err}"))?;
    let latest = manifest.version.clone();
    let verified = |version: &str| -> bool {
        manifest
            .files
            .contains_key(&remote_artifact(version, platform))
    };
    Ok((ordered_candidates(&current, &latest, verified), Some(manifest)))
}

/// Order candidate versions: a checksum-covered one first, without ever
/// downgrading the remote below `current`; unverified candidates follow as
/// fallbacks (pre-manifest releases). Pure.
fn ordered_candidates(
    current: &str,
    latest: &str,
    verified: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut versions: Vec<String> = Vec::new();
    if verified(current) {
        versions.push(current.to_string());
    } else if verified(latest) && !zeron_update::version_newer(current, latest) {
        // Verified and not older than us — take the checksummed artifact.
        versions.push(latest.to_string());
    }
    for candidate in [current, latest] {
        if !versions.iter().any(|version| version == candidate) {
            versions.push(candidate.to_string());
        }
    }
    versions
}

async fn stage_binary_for_remote(
    edge_url: &str,
    manifest: Option<&zeron_update::Manifest>,
    candidates: &[String],
    platform: &RemotePlatform,
) -> Result<StagedBinary, String> {
    if platform.matches_local() {
        let path = std::env::current_exe()
            .map_err(|err| format!("resolving the local zeron binary: {err}"))?;
        return Ok(StagedBinary {
            path,
            version: zeron_update::current_version().to_string(),
            _dir: None,
        });
    }
    let manifest = manifest.ok_or_else(|| "missing release metadata".to_string())?;
    let mut last_err = String::new();
    for version in candidates {
        let file = remote_artifact(version, platform);
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => return Err(format!("creating a temp dir: {err}")),
        };
        let tarball = dir.path().join(&file);
        if let Err(err) =
            zeron_update::download_release_file(edge_url, &manifest, &file, &tarball).await
        {
            last_err = format!("{file}: {err}");
            continue;
        }
        let unpacked = dir.path().join("unpacked");
        if let Err(err) = std::fs::create_dir_all(&unpacked) {
            return Err(format!("creating {}: {err}", unpacked.display()));
        }
        if let Err(err) = extract_tar_gz(&tarball, &unpacked) {
            last_err = err;
            continue;
        }
        let binary = unpacked.join("zeron");
        if !binary.is_file() {
            last_err = format!("{file} did not contain a zeron binary");
            continue;
        }
        return Ok(StagedBinary {
            path: binary,
            version: version.clone(),
            _dir: Some(dir),
        });
    }
    Err(format!(
        "no {} {} build available for the remote ({last_err})",
        platform.os, platform.arch
    ))
}

fn extract_tar_gz(tarball: &Path, dest: &Path) -> Result<(), String> {
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(tarball)
        .arg("-C")
        .arg(dest)
        .arg("--strip-components=1")
        .status()
        .map_err(|err| format!("running tar to unpack {}: {err}", tarball.display()))?;
    if !status.success() {
        return Err(format!("tar failed unpacking {}", tarball.display()));
    }
    Ok(())
}

/// Stream `local` to `remote_path`: `cat` into a dotted temp name, `chmod`, then
/// `mv`, so a half-written upload is never executable.
async fn upload_binary(target: &SshTarget, local: &Path, remote_path: &str) -> Result<(), String> {
    let (dir, name) = remote_path
        .rsplit_once('/')
        .ok_or_else(|| format!("bad remote binary path {remote_path:?}"))?;
    let incoming = format!("{dir}/.{name}.incoming");
    // `dir`/`name` are ASCII version/os/arch (no single quotes), so the outer
    // single quotes safely hand this script to the remote `sh`.
    let script = format!(
        "set -e; umask 077; mkdir -p \"{dir}\"; \
         cat > \"{incoming}\"; chmod 755 \"{incoming}\"; mv -f \"{incoming}\" \"{remote_path}\""
    );
    let (prog, args) = ssh_command(target, &format!("sh -c '{script}'"))?;
    let mut child = tokio::process::Command::new(&prog)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to spawn ssh: {err}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ssh stdin unavailable".to_string())?;
    let mut file = tokio::fs::File::open(local)
        .await
        .map_err(|err| format!("reading {}: {err}", local.display()))?;
    tokio::io::copy(&mut file, &mut stdin)
        .await
        .map_err(|err| format!("uploading {}: {err}", local.display()))?;
    stdin.shutdown().await.ok();
    drop(stdin);
    let output = tokio::time::timeout(SSH_UPLOAD_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("timed out uploading to {}", target.destination()))?
        .map_err(|err| format!("waiting for ssh: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{}: upload failed: {}",
            target.destination(),
            first_stderr(&output)
        ));
    }
    Ok(())
}

// ---- device-local persistence (`{data_dir}/ssh-targets.json`) ----

const STORE_FILE: &str = "ssh-targets.json";

fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STORE_FILE)
}

/// Load persisted targets; missing/corrupt files yield an empty list (the
/// `ui-settings.json` corrupt-fallback pattern).
pub fn load_targets(data_dir: &Path) -> Vec<SshTarget> {
    let path = store_path(data_dir);
    let bytes = std::fs::read(&path).unwrap_or_default();
    if bytes.is_empty() {
        return Vec::new();
    }
    serde_json::from_slice::<Vec<SshTarget>>(&bytes).unwrap_or_default()
}

/// Best-effort atomic write (temp + rename).
pub fn save_targets(data_dir: &Path, targets: &[SshTarget]) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = store_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(targets)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Insert or replace by id.
pub fn upsert_target(data_dir: &Path, target: SshTarget) -> std::io::Result<Vec<SshTarget>> {
    let mut targets = load_targets(data_dir);
    if let Some(slot) = targets.iter_mut().find(|t| t.id == target.id) {
        *slot = target;
    } else {
        targets.push(target);
    }
    save_targets(data_dir, &targets)?;
    Ok(targets)
}

pub fn remove_target(data_dir: &Path, id: &str) -> std::io::Result<Vec<SshTarget>> {
    let targets: Vec<SshTarget> = load_targets(data_dir)
        .into_iter()
        .filter(|t| t.id != id)
        .collect();
    save_targets(data_dir, &targets)?;
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_host() {
        let t = parse_ssh_target("192.168.1.10").unwrap();
        assert_eq!(t.host, "192.168.1.10");
        assert_eq!(t.port, 22);
        assert_eq!(t.username, None);
        assert_eq!(t.destination(), "192.168.1.10");
    }

    #[test]
    fn parse_ssh_url_with_user_port() {
        let t = parse_ssh_target("ssh://deploy@gpu-box:2222").unwrap();
        assert_eq!(t.host, "gpu-box");
        assert_eq!(t.port, 2222);
        assert_eq!(t.username.as_deref(), Some("deploy"));
        assert_eq!(t.destination(), "deploy@gpu-box:2222");
    }

    #[test]
    fn parse_scp_like_and_ignore_path() {
        let t = parse_ssh_target("ssh://deploy@gpu-box:2222/~/code/app").unwrap();
        assert_eq!(t.host, "gpu-box");
        assert_eq!(t.destination(), "deploy@gpu-box:2222");
    }

    #[test]
    fn parse_drops_password_never_persist_it() {
        let t = parse_ssh_target("ssh://deploy:secret@gpu-box").unwrap();
        assert_eq!(t.username.as_deref(), Some("deploy"));
        assert_eq!(t.host, "gpu-box");
    }

    #[test]
    fn parse_rejects_empty_and_bad_port() {
        assert!(parse_ssh_target("").is_err());
        assert!(parse_ssh_target("ssh://").is_err());
        assert!(parse_ssh_target("ssh://host:0").is_err());
    }

    #[test]
    fn parse_rejects_shell_metachars() {
        assert!(parse_ssh_target("evil; rm -rf ~").is_err());
        assert!(parse_ssh_target("host | cat").is_err());
    }

    #[test]
    fn base_args_shape() {
        let mut t = SshTarget::new("gpu-box");
        t.port = 2222;
        t.username = Some("deploy".into());
        t.identity_file = Some("~/.ssh/gpu.pem".into());
        t.extra_args = vec!["-J".into(), "jump.example.com".into()];
        let args = ssh_base_args(&t).unwrap();
        assert_eq!(
            args,
            vec![
                "-p",
                "2222",
                "-i",
                "~/.ssh/gpu.pem",
                "-J",
                "jump.example.com",
                "deploy@gpu-box:2222",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn command_never_a_shell_string() {
        let t = SshTarget::new("gpu-box");
        let (prog, args) = ssh_command(&t, "zeron rpc-stdio").unwrap();
        assert_eq!(prog, "ssh");
        assert!(args.contains(&"--".to_string()));
        assert_eq!(args.last().unwrap(), "zeron rpc-stdio");
    }

    #[test]
    fn uname_maps_to_release_platforms() {
        let linux = parse_uname("Linux", "x86_64").unwrap();
        assert_eq!(linux.os, "linux");
        assert_eq!(linux.arch, "x86_64");
        assert_eq!(parse_uname("Linux", "aarch64").unwrap().arch, "aarch64");
        let mac = parse_uname("Darwin", "arm64").unwrap();
        assert_eq!(mac.os, "macos");
        assert_eq!(mac.arch, "arm64");
        assert_eq!(
            parse_uname("MINGW64_NT-10.0-19045", "x86_64").unwrap().os,
            "windows"
        );
        assert!(parse_uname("Plan9", "x86_64").is_err());
        assert!(parse_uname("Linux", "sparc").is_err());
    }

    #[test]
    fn remote_artifact_and_path_shape() {
        let plat = parse_uname("Linux", "x86_64").unwrap();
        assert_eq!(remote_artifact("1.2.3", &plat), "zeron-1.2.3-linux-x86_64.tar.gz");
        assert_eq!(
            remote_binary_path("1.2.3", &plat),
            "$HOME/.zeron/remote/zeron-1.2.3-linux-x86_64"
        );
    }

    #[test]
    fn candidate_order_prefers_verified_without_downgrade() {
        // Pre-manifest release (nothing verifiable): pinned version first.
        assert_eq!(
            ordered_candidates("1.0.0", "2.0.0", |_| false),
            ["1.0.0", "2.0.0"]
        );
        // Client version is checksum-covered: it wins.
        assert_eq!(
            ordered_candidates("1.0.0", "2.0.0", |v| v == "1.0.0"),
            ["1.0.0", "2.0.0"]
        );
        // Only the newer release is verifiable: take it, no downgrade.
        assert_eq!(
            ordered_candidates("1.0.0", "2.0.0", |v| v == "2.0.0"),
            ["2.0.0", "1.0.0"]
        );
        // An older verifiable release must not replace a newer client build.
        assert_eq!(
            ordered_candidates("3.0.0", "2.0.0", |v| v == "2.0.0"),
            ["3.0.0", "2.0.0"]
        );
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zeron-rpc-ssh-test-{}-{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_round_trip() {
        let dir = test_dir("round-trip");
        assert!(load_targets(&dir).is_empty());
        let t = SshTarget::new("gpu-box");
        let id = t.id.clone();
        let targets = upsert_target(&dir, t).unwrap();
        assert_eq!(targets.len(), 1);
        let remaining = remove_target(&dir, &id).unwrap();
        assert!(remaining.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_store_falls_back_to_empty() {
        let dir = test_dir("corrupt");
        std::fs::write(store_path(&dir), b"{not json").unwrap();
        assert!(load_targets(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
