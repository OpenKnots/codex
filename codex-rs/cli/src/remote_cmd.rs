use std::fs;
use std::fs::File;
use std::io::ErrorKind;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::Parser;
use codex_app_server::AppServerTransport;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::find_codex_home;
use codex_core::config_loader::LoaderOverrides;
use codex_utils_cli::CliConfigOverrides;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use x25519_dalek::PublicKey;
use x25519_dalek::StaticSecret;

const REMOTE_DIR_NAME: &str = "remote";
const APP_SERVER_SOCKET_FILENAME: &str = "app-server.sock";
const DEVICES_FILENAME: &str = "devices.json";
const HOST_FILENAME: &str = "host.json";
const PAIRING_FILENAME: &str = "pairing.json";
const PID_FILENAME: &str = "remote.pid";
const REMOTE_LOG_FILENAME: &str = "remote.log";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const PAIRING_TTL_SECS: i64 = 300;
const REMOTE_STATE_VERSION: u32 = 2;
const RELAY_STATUS_DISCONNECTED: &str = "disconnected";
const X25519_ALGORITHM: &str = "x25519";

#[derive(Debug, Parser)]
pub(crate) struct RemoteCli {
    #[command(subcommand)]
    pub(crate) subcommand: RemoteSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum RemoteSubcommand {
    Start,
    Status,
    Pair,
    Stop,
    Devices(RemoteDevicesCommand),
    #[clap(hide = true)]
    Daemon,
}

#[derive(Debug, Parser)]
pub(crate) struct RemoteDevicesCommand {
    #[command(subcommand)]
    pub(crate) subcommand: RemoteDevicesSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum RemoteDevicesSubcommand {
    List,
    Revoke(RemoteDevicesRevokeCommand),
}

#[derive(Debug, Parser)]
pub(crate) struct RemoteDevicesRevokeCommand {
    pub(crate) device_id: String,
}

struct RemotePaths {
    dir: PathBuf,
    socket_path: PathBuf,
    devices_path: PathBuf,
    host_path: PathBuf,
    pairing_path: PathBuf,
    pid_path: PathBuf,
    log_path: PathBuf,
}

struct RemoteStatus {
    running: bool,
    pid: Option<i32>,
    socket_connected: bool,
    paired_devices: usize,
    pending_pairing_sessions: usize,
    host: Option<RemoteHostState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteHostState {
    version: u32,
    host_id: String,
    host_name: String,
    platform: String,
    relay: RemoteRelayState,
    identity: RemoteHostIdentity,
    created_at: i64,
    updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteRelayState {
    status: String,
    connected_at: Option<i64>,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteHostIdentity {
    algorithm: String,
    public_key: String,
    secret_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDevicesState {
    version: u32,
    devices: Vec<RemoteDeviceRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDeviceRecord {
    id: String,
    name: String,
    platform: Option<String>,
    public_key: Option<String>,
    paired_at: i64,
    last_seen_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePairingState {
    version: u32,
    sessions: Vec<RemotePairingSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePairingSession {
    session_id: String,
    code: String,
    deep_link: String,
    created_at: i64,
    expires_at: i64,
    used_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyRemoteHostStateV1 {
    host_id: String,
    created_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyRemoteDevicesStateV1 {
    devices: Vec<LegacyRemoteDeviceRecordV1>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyRemoteDeviceRecordV1 {
    id: String,
    name: String,
    paired_at: i64,
    revoked_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyRemotePairingStateV1 {
    sessions: Vec<LegacyRemotePairingSessionV1>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyRemotePairingSessionV1 {
    session_id: String,
    code: String,
    deep_link: String,
    created_at: i64,
    expires_at: i64,
    used_at: Option<i64>,
}

pub(crate) async fn run_remote_command(
    cli: RemoteCli,
    arg0_paths: Arg0DispatchPaths,
    root_config_overrides: CliConfigOverrides,
) -> Result<()> {
    let paths = remote_paths()?;
    match cli.subcommand {
        RemoteSubcommand::Start => start_remote_runtime(&paths, &root_config_overrides).await,
        RemoteSubcommand::Status => {
            print_remote_status(&paths).await?;
            Ok(())
        }
        RemoteSubcommand::Pair => pair_remote_device(&paths).await,
        RemoteSubcommand::Stop => stop_remote_runtime(&paths).await,
        RemoteSubcommand::Devices(remote_devices) => match remote_devices.subcommand {
            RemoteDevicesSubcommand::List => {
                list_remote_devices(&paths)?;
                Ok(())
            }
            RemoteDevicesSubcommand::Revoke(cmd) => revoke_remote_device(&paths, &cmd.device_id),
        },
        RemoteSubcommand::Daemon => {
            run_remote_daemon(paths, arg0_paths, root_config_overrides).await
        }
    }
}

fn remote_paths() -> Result<RemotePaths> {
    let dir = find_codex_home()
        .context("failed to resolve CODEX_HOME")?
        .join(REMOTE_DIR_NAME);
    Ok(RemotePaths {
        socket_path: dir.join(APP_SERVER_SOCKET_FILENAME),
        devices_path: dir.join(DEVICES_FILENAME),
        host_path: dir.join(HOST_FILENAME),
        pairing_path: dir.join(PAIRING_FILENAME),
        pid_path: dir.join(PID_FILENAME),
        log_path: dir.join(REMOTE_LOG_FILENAME),
        dir,
    })
}

async fn start_remote_runtime(
    paths: &RemotePaths,
    root_config_overrides: &CliConfigOverrides,
) -> Result<()> {
    fs::create_dir_all(&paths.dir)
        .with_context(|| format!("failed to create {}", paths.dir.display()))?;
    ensure_host_identity(paths)?;
    ensure_devices_file(paths)?;
    ensure_pairing_file(paths)?;

    let status = load_remote_status(paths).await?;
    if status.running {
        println!("Remote host runtime is already running.");
        return Ok(());
    }

    if paths.pid_path.exists() {
        let _ = fs::remove_file(&paths.pid_path);
    }

    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut command = Command::new(current_exe);
    for raw_override in &root_config_overrides.raw_overrides {
        command.arg("-c").arg(raw_override);
    }

    let log_file = File::options()
        .create(true)
        .append(true)
        .open(&paths.log_path)
        .with_context(|| format!("failed to open {}", paths.log_path.display()))?;
    let log_file_err = log_file
        .try_clone()
        .with_context(|| format!("failed to clone {}", paths.log_path.display()))?;

    command
        .arg("remote")
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));
    command
        .spawn()
        .context("failed to spawn remote host runtime")?;

    wait_for_remote_start(paths).await?;
    println!("Started remote host runtime.");
    Ok(())
}

async fn print_remote_status(paths: &RemotePaths) -> Result<()> {
    let status = load_remote_status(paths).await?;
    println!(
        "status: {}",
        if status.running { "running" } else { "stopped" }
    );
    if let Some(pid) = status.pid {
        println!("pid: {pid}");
    } else {
        println!("pid: none");
    }
    if let Some(host) = &status.host {
        println!("host id: {}", host.host_id);
        println!("host name: {}", host.host_name);
        println!("platform: {}", host.platform);
    }
    println!(
        "socket: {} ({})",
        paths.socket_path.display(),
        if status.socket_connected {
            "connected"
        } else if paths.socket_path.exists() {
            "present"
        } else {
            "missing"
        }
    );
    if let Some(host) = &status.host {
        println!("relay: {}", host.relay.status);
    } else {
        println!("relay: {RELAY_STATUS_DISCONNECTED}");
    }
    println!("paired devices: {}", status.paired_devices);
    println!(
        "pending pairing sessions: {}",
        status.pending_pairing_sessions
    );
    Ok(())
}

async fn pair_remote_device(paths: &RemotePaths) -> Result<()> {
    let status = load_remote_status(paths).await?;
    if !status.running {
        anyhow::bail!("remote host runtime is not running; start it with `codex remote start`");
    }

    let host = ensure_host_identity(paths)?;
    let now = now_unix_seconds()?;
    let mut pairing = ensure_pairing_file(paths)?;
    prune_pairing_sessions(&mut pairing, now);
    let session_id = generate_hex_token(16)?;
    let code = generate_pairing_code()?;
    let expires_at = now + PAIRING_TTL_SECS;
    let deep_link = format!(
        "codex://remote/pair?hostId={}&sessionId={session_id}&code={code}&expiresAt={expires_at}",
        host.host_id
    );

    pairing.sessions.push(RemotePairingSession {
        session_id: session_id.clone(),
        code: code.clone(),
        deep_link: deep_link.clone(),
        created_at: now,
        expires_at,
        used_at: None,
        revoked_at: None,
    });
    write_json_file(&paths.pairing_path, &pairing)?;

    println!("Pairing link:");
    println!("{deep_link}");
    println!("Session ID: {session_id}");
    println!("Code: {code}");
    println!("Expires at: {expires_at}");
    Ok(())
}

async fn stop_remote_runtime(paths: &RemotePaths) -> Result<()> {
    let Some(pid) = read_pid_file(&paths.pid_path)? else {
        println!("Remote host runtime is not running.");
        return Ok(());
    };

    if !process_is_alive(pid) {
        let _ = fs::remove_file(&paths.pid_path);
        println!("Remote host runtime is not running.");
        return Ok(());
    }

    terminate_process(pid)?;
    wait_for_remote_stop(paths, pid).await?;
    println!("Stopped remote host runtime.");
    Ok(())
}

fn list_remote_devices(paths: &RemotePaths) -> Result<()> {
    let Some(devices) = read_devices_state(paths)? else {
        println!("No paired devices.");
        return Ok(());
    };

    let mut active_count = 0usize;
    for entry in &devices.devices {
        if entry.revoked_at.is_some() {
            continue;
        }
        active_count += 1;
        println!("{}\t{}", entry.id, entry.name);
    }

    if active_count == 0 {
        println!("No paired devices.");
    }
    Ok(())
}

fn revoke_remote_device(paths: &RemotePaths, device_id: &str) -> Result<()> {
    let mut devices = ensure_devices_file(paths)?;
    let revoked_at = now_unix_seconds()?;
    let mut found = false;
    for entry in &mut devices.devices {
        if entry.id == device_id {
            entry.revoked_at = Some(revoked_at);
            found = true;
            break;
        }
    }

    if !found {
        anyhow::bail!("device `{device_id}` not found");
    }

    write_json_file(&paths.devices_path, &devices)?;
    println!("Revoked device `{device_id}`.");
    Ok(())
}

async fn run_remote_daemon(
    paths: RemotePaths,
    arg0_paths: Arg0DispatchPaths,
    root_config_overrides: CliConfigOverrides,
) -> Result<()> {
    fs::create_dir_all(&paths.dir)
        .with_context(|| format!("failed to create {}", paths.dir.display()))?;
    ensure_host_identity(&paths)?;
    ensure_devices_file(&paths)?;
    ensure_pairing_file(&paths)?;
    fs::write(&paths.pid_path, format!("{}", std::process::id()))
        .with_context(|| format!("failed to write {}", paths.pid_path.display()))?;

    let result = codex_app_server::run_main_with_transport(
        arg0_paths,
        root_config_overrides,
        LoaderOverrides::default(),
        false,
        AppServerTransport::UnixDomainSocket {
            socket_path: paths.socket_path.clone(),
        },
    )
    .await;

    if let Err(err) = fs::remove_file(&paths.pid_path)
        && err.kind() != ErrorKind::NotFound
    {
        eprintln!("failed to remove {}: {err}", paths.pid_path.display());
    }

    result.map_err(anyhow::Error::from)
}

async fn load_remote_status(paths: &RemotePaths) -> Result<RemoteStatus> {
    let pid = read_pid_file(&paths.pid_path)?;
    let running = pid.is_some_and(process_is_alive);
    let now = now_unix_seconds()?;
    let socket_connected = if running && paths.socket_path.exists() {
        socket_is_connected(&paths.socket_path).await
    } else {
        false
    };
    let paired_devices = read_devices_state(paths)?
        .map(|devices| {
            devices
                .devices
                .iter()
                .filter(|entry| entry.revoked_at.is_none())
                .count()
        })
        .unwrap_or(0);
    let pending_pairing_sessions = read_pairing_state(paths)?
        .map(|pairing| {
            pairing
                .sessions
                .iter()
                .filter(|session| pairing_session_is_active(session, now))
                .count()
        })
        .unwrap_or(0);
    Ok(RemoteStatus {
        running,
        pid,
        socket_connected,
        paired_devices,
        pending_pairing_sessions,
        host: read_host_state(paths)?,
    })
}

fn read_pid_file(path: &Path) -> Result<Option<i32>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let pid = contents
                .trim()
                .parse::<i32>()
                .with_context(|| format!("invalid pid file {}", path.display()))?;
            Ok(Some(pid))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn read_json_file(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let value = serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            Ok(Some(value))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_json_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let rendered = serde_json::to_string_pretty(value)?;
    fs::write(path, rendered).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn ensure_host_identity(paths: &RemotePaths) -> Result<RemoteHostState> {
    match read_json_file(&paths.host_path)? {
        Some(raw) if raw["version"].as_u64() == Some(REMOTE_STATE_VERSION.into()) => {
            serde_json::from_value(raw)
                .with_context(|| format!("failed to parse {}", paths.host_path.display()))
        }
        Some(raw) => {
            let host = migrate_host_state(raw)?;
            write_json_file(&paths.host_path, &host)?;
            Ok(host)
        }
        None => {
            let host = default_host_state()?;
            write_json_file(&paths.host_path, &host)?;
            Ok(host)
        }
    }
}

fn ensure_devices_file(paths: &RemotePaths) -> Result<RemoteDevicesState> {
    match read_json_file(&paths.devices_path)? {
        Some(raw) if raw["version"].as_u64() == Some(REMOTE_STATE_VERSION.into()) => {
            serde_json::from_value(raw)
                .with_context(|| format!("failed to parse {}", paths.devices_path.display()))
        }
        Some(raw) => {
            let devices = migrate_devices_state(raw)?;
            write_json_file(&paths.devices_path, &devices)?;
            Ok(devices)
        }
        None => {
            let devices = default_devices_file();
            write_json_file(&paths.devices_path, &devices)?;
            Ok(devices)
        }
    }
}

fn ensure_pairing_file(paths: &RemotePaths) -> Result<RemotePairingState> {
    match read_json_file(&paths.pairing_path)? {
        Some(raw) if raw["version"].as_u64() == Some(REMOTE_STATE_VERSION.into()) => {
            serde_json::from_value(raw)
                .with_context(|| format!("failed to parse {}", paths.pairing_path.display()))
        }
        Some(raw) => {
            let pairing = migrate_pairing_state(raw)?;
            write_json_file(&paths.pairing_path, &pairing)?;
            Ok(pairing)
        }
        None => {
            let pairing = default_pairing_file();
            write_json_file(&paths.pairing_path, &pairing)?;
            Ok(pairing)
        }
    }
}

fn read_host_state(paths: &RemotePaths) -> Result<Option<RemoteHostState>> {
    match read_json_file(&paths.host_path)? {
        Some(raw) if raw["version"].as_u64() == Some(REMOTE_STATE_VERSION.into()) => {
            let host = serde_json::from_value(raw)
                .with_context(|| format!("failed to parse {}", paths.host_path.display()))?;
            Ok(Some(host))
        }
        Some(raw) => Ok(Some(migrate_host_state(raw)?)),
        None => Ok(None),
    }
}

fn read_devices_state(paths: &RemotePaths) -> Result<Option<RemoteDevicesState>> {
    match read_json_file(&paths.devices_path)? {
        Some(raw) if raw["version"].as_u64() == Some(REMOTE_STATE_VERSION.into()) => {
            let devices = serde_json::from_value(raw)
                .with_context(|| format!("failed to parse {}", paths.devices_path.display()))?;
            Ok(Some(devices))
        }
        Some(raw) => Ok(Some(migrate_devices_state(raw)?)),
        None => Ok(None),
    }
}

fn read_pairing_state(paths: &RemotePaths) -> Result<Option<RemotePairingState>> {
    match read_json_file(&paths.pairing_path)? {
        Some(raw) if raw["version"].as_u64() == Some(REMOTE_STATE_VERSION.into()) => {
            let pairing = serde_json::from_value(raw)
                .with_context(|| format!("failed to parse {}", paths.pairing_path.display()))?;
            Ok(Some(pairing))
        }
        Some(raw) => Ok(Some(migrate_pairing_state(raw)?)),
        None => Ok(None),
    }
}

fn default_host_state() -> Result<RemoteHostState> {
    let created_at = now_unix_seconds()?;
    let identity = generate_x25519_host_identity()?;
    Ok(RemoteHostState {
        version: REMOTE_STATE_VERSION,
        host_id: format!("host_{}", generate_hex_token(8)?),
        host_name: current_host_name(),
        platform: current_platform(),
        relay: default_relay_state(),
        identity,
        created_at,
        updated_at: created_at,
    })
}

fn migrate_host_state(raw: Value) -> Result<RemoteHostState> {
    let legacy: LegacyRemoteHostStateV1 =
        serde_json::from_value(raw).context("failed to migrate legacy remote host state")?;
    Ok(RemoteHostState {
        version: REMOTE_STATE_VERSION,
        host_id: legacy.host_id,
        host_name: current_host_name(),
        platform: current_platform(),
        relay: default_relay_state(),
        identity: generate_x25519_host_identity()?,
        created_at: legacy.created_at,
        updated_at: now_unix_seconds()?,
    })
}

fn default_relay_state() -> RemoteRelayState {
    RemoteRelayState {
        status: RELAY_STATUS_DISCONNECTED.to_string(),
        connected_at: None,
        last_error: None,
    }
}

fn default_devices_file() -> RemoteDevicesState {
    RemoteDevicesState {
        version: REMOTE_STATE_VERSION,
        devices: Vec::new(),
    }
}

fn migrate_devices_state(raw: Value) -> Result<RemoteDevicesState> {
    let legacy: LegacyRemoteDevicesStateV1 =
        serde_json::from_value(raw).context("failed to migrate legacy remote devices state")?;
    Ok(RemoteDevicesState {
        version: REMOTE_STATE_VERSION,
        devices: legacy
            .devices
            .into_iter()
            .map(|device| RemoteDeviceRecord {
                id: device.id,
                name: device.name,
                platform: None,
                public_key: None,
                paired_at: device.paired_at,
                last_seen_at: None,
                revoked_at: device.revoked_at,
            })
            .collect(),
    })
}

fn default_pairing_file() -> RemotePairingState {
    RemotePairingState {
        version: REMOTE_STATE_VERSION,
        sessions: Vec::new(),
    }
}

fn migrate_pairing_state(raw: Value) -> Result<RemotePairingState> {
    let legacy: LegacyRemotePairingStateV1 =
        serde_json::from_value(raw).context("failed to migrate legacy remote pairing state")?;
    Ok(RemotePairingState {
        version: REMOTE_STATE_VERSION,
        sessions: legacy
            .sessions
            .into_iter()
            .map(|session| RemotePairingSession {
                session_id: session.session_id,
                code: session.code,
                deep_link: session.deep_link,
                created_at: session.created_at,
                expires_at: session.expires_at,
                used_at: session.used_at,
                revoked_at: None,
            })
            .collect(),
    })
}

fn current_host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("COMPUTERNAME")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| format!("{}-{}", current_platform(), std::process::id()))
}

fn current_platform() -> String {
    std::env::consts::OS.to_string()
}

fn prune_pairing_sessions(pairing: &mut RemotePairingState, now: i64) {
    pairing
        .sessions
        .retain(|session| pairing_session_is_active(session, now));
}

fn pairing_session_is_active(session: &RemotePairingSession, now: i64) -> bool {
    session.used_at.is_none() && session.revoked_at.is_none() && session.expires_at > now
}

fn generate_x25519_host_identity() -> Result<RemoteHostIdentity> {
    let mut file = File::open("/dev/urandom").context("failed to open /dev/urandom")?;
    let mut secret_key_bytes = [0u8; 32];
    file.read_exact(&mut secret_key_bytes)
        .context("failed to read x25519 secret key bytes")?;
    let secret_key = StaticSecret::from(secret_key_bytes);
    let public_key = PublicKey::from(&secret_key);
    Ok(RemoteHostIdentity {
        algorithm: X25519_ALGORITHM.to_string(),
        public_key: URL_SAFE_NO_PAD.encode(public_key.as_bytes()),
        secret_key: URL_SAFE_NO_PAD.encode(secret_key.to_bytes()),
    })
}

fn now_unix_seconds() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs() as i64)
}

fn process_is_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        // Safety: `kill(pid, 0)` does not deliver a signal; it probes liveness.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn terminate_process(pid: i32) -> Result<()> {
    #[cfg(unix)]
    {
        // Safety: sending SIGTERM to a pid obtained from our pid file.
        let result = unsafe { libc::kill(pid, libc::SIGTERM) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to signal process {pid}"));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        anyhow::bail!("remote host runtime is only supported on unix hosts")
    }
}

async fn wait_for_remote_start(paths: &RemotePaths) -> Result<()> {
    let deadline = std::time::Instant::now() + START_TIMEOUT;
    loop {
        let status = load_remote_status(paths).await?;
        if status.running && status.socket_connected {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for remote host runtime to start");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_remote_stop(paths: &RemotePaths, pid: i32) -> Result<()> {
    let deadline = std::time::Instant::now() + STOP_TIMEOUT;
    loop {
        let pid_file = read_pid_file(&paths.pid_path)?;
        let still_alive = process_is_alive(pid);
        if !still_alive {
            if pid_file.is_some() {
                let _ = fs::remove_file(&paths.pid_path);
            }
            if paths.socket_path.exists() {
                let _ = fs::remove_file(&paths.socket_path);
            }
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for remote host runtime to stop");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn socket_is_connected(socket_path: &Path) -> bool {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(socket_path).await.is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        false
    }
}

fn generate_pairing_code() -> Result<String> {
    let token = generate_hex_token(4)?;
    Ok(token.to_ascii_uppercase())
}

fn generate_hex_token(bytes_len: usize) -> Result<String> {
    let mut file = File::open("/dev/urandom").context("failed to open /dev/urandom")?;
    let mut bytes = vec![0u8; bytes_len];
    file.read_exact(&mut bytes)
        .context("failed to read random bytes")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
