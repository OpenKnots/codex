use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

async fn wait_for_path(path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if path.exists() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_path_absent(path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if !path.exists() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {} to be removed", path.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn remote_status_reports_stopped_before_start() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["remote", "status"])
        .assert()
        .success()
        .stdout(contains("status: stopped"));

    Ok(())
}

#[tokio::test]
async fn remote_start_creates_runtime_and_stop_removes_it() -> Result<()> {
    let codex_home = TempDir::new()?;
    let remote_dir = codex_home.path().join("remote");
    let pid_path = remote_dir.join("remote.pid");
    let socket_path = remote_dir.join("app-server.sock");

    let mut start = codex_command(codex_home.path())?;
    start
        .args(["remote", "start"])
        .assert()
        .success()
        .stdout(contains("Started remote host runtime."));

    wait_for_path(&pid_path).await?;
    wait_for_path(&socket_path).await?;

    let mut status = codex_command(codex_home.path())?;
    status
        .args(["remote", "status"])
        .assert()
        .success()
        .stdout(contains("status: running"))
        .stdout(contains("relay: disconnected"))
        .stdout(contains("paired devices: 0"));

    let mut stop = codex_command(codex_home.path())?;
    stop.args(["remote", "stop"])
        .assert()
        .success()
        .stdout(contains("Stopped remote host runtime."));

    wait_for_path_absent(&pid_path).await?;
    wait_for_path_absent(&socket_path).await?;

    Ok(())
}

#[tokio::test]
async fn remote_pair_writes_pairing_session_and_prints_deep_link() -> Result<()> {
    let codex_home = TempDir::new()?;
    let pairing_path = codex_home.path().join("remote").join("pairing.json");

    let mut start = codex_command(codex_home.path())?;
    start.args(["remote", "start"]).assert().success();

    let mut pair = codex_command(codex_home.path())?;
    pair.args(["remote", "pair"])
        .assert()
        .success()
        .stdout(contains("codex://remote/pair?"));

    wait_for_path(&pairing_path).await?;
    let pairing: Value = serde_json::from_str(&fs::read_to_string(&pairing_path)?)?;
    let sessions = pairing["sessions"]
        .as_array()
        .context("pairing.json should contain sessions")?;
    assert_eq!(sessions.len(), 1);

    let mut stop = codex_command(codex_home.path())?;
    stop.args(["remote", "stop"]).assert().success();

    Ok(())
}

#[tokio::test]
async fn remote_start_writes_structured_host_identity_metadata() -> Result<()> {
    let codex_home = TempDir::new()?;
    let host_path = codex_home.path().join("remote").join("host.json");

    let mut start = codex_command(codex_home.path())?;
    start.args(["remote", "start"]).assert().success();

    wait_for_path(&host_path).await?;
    let host: Value = serde_json::from_str(&fs::read_to_string(&host_path)?)?;

    assert_eq!(host["version"], Value::from(2));
    assert_eq!(host["identity"]["algorithm"], Value::from("x25519"));
    assert_eq!(host["relay"]["status"], Value::from("disconnected"));
    assert_eq!(host["platform"].as_str(), Some(std::env::consts::OS));
    assert!(host["hostName"].as_str().is_some());
    assert!(host["identity"]["publicKey"].as_str().is_some());
    assert!(host["identity"]["secretKey"].as_str().is_some());

    let mut stop = codex_command(codex_home.path())?;
    stop.args(["remote", "stop"]).assert().success();

    Ok(())
}

#[tokio::test]
async fn remote_pair_prunes_expired_sessions_before_issuing_a_new_one() -> Result<()> {
    let codex_home = TempDir::new()?;
    let remote_dir = codex_home.path().join("remote");
    fs::create_dir_all(&remote_dir)?;
    let pairing_path = remote_dir.join("pairing.json");
    fs::write(
        &pairing_path,
        serde_json::json!({
            "version": 2,
            "sessions": [
                {
                    "sessionId": "expired",
                    "code": "EXPIRED",
                    "deepLink": "codex://remote/pair?sessionId=expired",
                    "createdAt": 1,
                    "expiresAt": 2,
                    "usedAt": null,
                    "revokedAt": null
                }
            ]
        })
        .to_string(),
    )?;

    let mut start = codex_command(codex_home.path())?;
    start.args(["remote", "start"]).assert().success();

    let mut pair = codex_command(codex_home.path())?;
    pair.args(["remote", "pair"])
        .assert()
        .success()
        .stdout(contains("Session ID:"))
        .stdout(contains("Expires at:"));

    let pairing: Value = serde_json::from_str(&fs::read_to_string(&pairing_path)?)?;
    let sessions = pairing["sessions"]
        .as_array()
        .context("pairing.json should contain sessions")?;
    assert_eq!(sessions.len(), 1);
    assert_ne!(sessions[0]["sessionId"].as_str(), Some("expired"));

    let mut stop = codex_command(codex_home.path())?;
    stop.args(["remote", "stop"]).assert().success();

    Ok(())
}

#[tokio::test]
async fn remote_status_reports_host_identity_and_active_pairing_sessions() -> Result<()> {
    let codex_home = TempDir::new()?;

    let mut start = codex_command(codex_home.path())?;
    start.args(["remote", "start"]).assert().success();

    let mut pair = codex_command(codex_home.path())?;
    pair.args(["remote", "pair"]).assert().success();

    let mut status = codex_command(codex_home.path())?;
    status
        .args(["remote", "status"])
        .assert()
        .success()
        .stdout(contains("host id: host_"))
        .stdout(contains("host name: "))
        .stdout(contains("pending pairing sessions: 1"));

    let mut stop = codex_command(codex_home.path())?;
    stop.args(["remote", "stop"]).assert().success();

    Ok(())
}

#[tokio::test]
async fn remote_devices_revoke_marks_device_revoked() -> Result<()> {
    let codex_home = TempDir::new()?;
    let remote_dir = codex_home.path().join("remote");
    fs::create_dir_all(&remote_dir)?;
    let devices_path = remote_dir.join("devices.json");
    fs::write(
        &devices_path,
        serde_json::json!({
            "version": 1,
            "devices": [
                {
                    "id": "dev_123",
                    "name": "Test phone",
                    "pairedAt": 1,
                    "revokedAt": null
                }
            ]
        })
        .to_string(),
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["remote", "devices", "revoke", "dev_123"])
        .assert()
        .success()
        .stdout(contains("Revoked device `dev_123`."));

    let devices: Value = serde_json::from_str(&fs::read_to_string(&devices_path)?)?;
    let revoked_at = devices["devices"][0]["revokedAt"].as_i64();
    assert!(revoked_at.is_some());

    Ok(())
}
