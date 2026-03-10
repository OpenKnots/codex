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
use clap::Parser;
use codex_app_server::AppServerTransport;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::find_codex_home;
use codex_core::config_loader::LoaderOverrides;
use codex_utils_cli::CliConfigOverrides;
use serde_json::Value;
use serde_json::json;

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
    println!("relay: disconnected");
    println!("paired devices: {}", status.paired_devices);
    Ok(())
}

async fn pair_remote_device(paths: &RemotePaths) -> Result<()> {
    let status = load_remote_status(paths).await?;
    if !status.running {
        anyhow::bail!("remote host runtime is not running; start it with `codex remote start`");
    }

    let host = ensure_host_identity(paths)?;
    let mut pairing = read_json_file(&paths.pairing_path)?.unwrap_or_else(default_pairing_file);
    let now = now_unix_seconds()?;
    let session_id = generate_hex_token(16)?;
    let code = generate_pairing_code()?;
    let deep_link = format!(
        "codex://remote/pair?hostId={}&sessionId={session_id}&code={code}&expiresAt={}",
        host["hostId"]
            .as_str()
            .context("host.json missing hostId")?,
        now + PAIRING_TTL_SECS
    );

    let sessions = pairing
        .get_mut("sessions")
        .and_then(Value::as_array_mut)
        .context("pairing.json missing sessions array")?;
    sessions.push(json!({
        "sessionId": session_id,
        "code": code,
        "deepLink": deep_link,
        "createdAt": now,
        "expiresAt": now + PAIRING_TTL_SECS,
        "usedAt": Value::Null,
    }));
    write_json_file(&paths.pairing_path, &pairing)?;

    println!("Pairing link:");
    println!("{deep_link}");
    println!("Code: {code}");
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
    let Some(devices) = read_json_file(&paths.devices_path)? else {
        println!("No paired devices.");
        return Ok(());
    };
    let Some(entries) = devices.get("devices").and_then(Value::as_array) else {
        println!("No paired devices.");
        return Ok(());
    };

    let mut active_count = 0usize;
    for entry in entries {
        if entry.get("revokedAt").is_some() && !entry["revokedAt"].is_null() {
            continue;
        }
        active_count += 1;
        let id = entry["id"].as_str().unwrap_or("<unknown>");
        let name = entry["name"].as_str().unwrap_or("<unnamed>");
        println!("{id}\t{name}");
    }

    if active_count == 0 {
        println!("No paired devices.");
    }
    Ok(())
}

fn revoke_remote_device(paths: &RemotePaths, device_id: &str) -> Result<()> {
    let mut devices = read_json_file(&paths.devices_path)?.unwrap_or_else(default_devices_file);
    let entries = devices
        .get_mut("devices")
        .and_then(Value::as_array_mut)
        .context("devices.json missing devices array")?;
    let revoked_at = now_unix_seconds()?;
    let mut found = false;
    for entry in entries {
        if entry.get("id").and_then(Value::as_str) == Some(device_id) {
            entry["revokedAt"] = Value::from(revoked_at);
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
    let socket_connected = if running && paths.socket_path.exists() {
        socket_is_connected(&paths.socket_path).await
    } else {
        false
    };
    let paired_devices = read_json_file(&paths.devices_path)?
        .and_then(|value| value.get("devices").and_then(Value::as_array).cloned())
        .map(|devices| {
            devices
                .iter()
                .filter(|entry| entry.get("revokedAt").is_none() || entry["revokedAt"].is_null())
                .count()
        })
        .unwrap_or(0);
    Ok(RemoteStatus {
        running,
        pid,
        socket_connected,
        paired_devices,
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

fn write_json_file(path: &Path, value: &Value) -> Result<()> {
    let rendered = serde_json::to_string_pretty(value)?;
    fs::write(path, rendered).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn ensure_host_identity(paths: &RemotePaths) -> Result<Value> {
    if let Some(host) = read_json_file(&paths.host_path)? {
        return Ok(host);
    }

    let host = json!({
        "version": 1,
        "hostId": format!("host_{}", generate_hex_token(8)?),
        "publicKey": generate_hex_token(32)?,
        "secretKey": generate_hex_token(32)?,
        "createdAt": now_unix_seconds()?,
    });
    write_json_file(&paths.host_path, &host)?;
    Ok(host)
}

fn ensure_devices_file(paths: &RemotePaths) -> Result<()> {
    if !paths.devices_path.exists() {
        write_json_file(&paths.devices_path, &default_devices_file())?;
    }
    Ok(())
}

fn ensure_pairing_file(paths: &RemotePaths) -> Result<()> {
    if !paths.pairing_path.exists() {
        write_json_file(&paths.pairing_path, &default_pairing_file())?;
    }
    Ok(())
}

fn default_devices_file() -> Value {
    json!({
        "version": 1,
        "devices": [],
    })
}

fn default_pairing_file() -> Value {
    json!({
        "version": 1,
        "sessions": [],
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
