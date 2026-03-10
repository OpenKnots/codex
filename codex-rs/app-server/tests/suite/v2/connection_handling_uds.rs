use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCRequest;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;
use tokio::time::timeout;

use super::connection_handling_websocket::DEFAULT_READ_TIMEOUT;
use super::connection_handling_websocket::create_config_toml;

#[tokio::test]
async fn unix_domain_socket_transport_routes_per_connection_handshake_and_responses() -> Result<()>
{
    let server =
        app_test_support::create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri(), "never")?;

    let socket_path = codex_home.path().join("app-server.sock");
    let (mut process, socket_path) =
        spawn_unix_domain_socket_server(codex_home.path(), &socket_path).await?;

    let mut client_one = connect_unix_domain_socket(&socket_path).await?;
    let mut client_two = connect_unix_domain_socket(&socket_path).await?;

    send_initialize_request(&mut client_one, 1, "uds_client_one").await?;
    let first_init = read_response_for_id(&mut client_one, 1).await?;
    assert_eq!(first_init.id, RequestId::Integer(1));

    assert_no_message(&mut client_two, Duration::from_millis(250)).await?;

    send_config_read_request(&mut client_two, 2).await?;
    let not_initialized = read_error_for_id(&mut client_two, 2).await?;
    assert_eq!(not_initialized.error.message, "Not initialized");

    send_initialize_request(&mut client_two, 3, "uds_client_two").await?;
    let second_init = read_response_for_id(&mut client_two, 3).await?;
    assert_eq!(second_init.id, RequestId::Integer(3));

    send_config_read_request(&mut client_one, 77).await?;
    send_config_read_request(&mut client_two, 77).await?;
    let client_one_config = read_response_for_id(&mut client_one, 77).await?;
    let client_two_config = read_response_for_id(&mut client_two, 77).await?;

    assert_eq!(client_one_config.id, RequestId::Integer(77));
    assert_eq!(client_two_config.id, RequestId::Integer(77));
    assert!(client_one_config.result.get("config").is_some());
    assert!(client_two_config.result.get("config").is_some());

    process
        .kill()
        .await
        .context("failed to stop unix domain socket app-server")?;
    Ok(())
}

async fn spawn_unix_domain_socket_server(
    codex_home: &Path,
    socket_path: &Path,
) -> Result<(Child, String)> {
    let program = codex_utils_cargo_bin::cargo_bin("codex-app-server")
        .context("should find app-server binary")?;
    let mut cmd = Command::new(program);
    cmd.arg("--listen")
        .arg(format!("uds://{}", socket_path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("CODEX_HOME", codex_home)
        .env("RUST_LOG", "debug");
    let mut process = cmd
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn unix domain socket app-server")?;

    let stderr = process
        .stderr
        .take()
        .context("failed to capture unix domain socket app-server stderr")?;
    let mut stderr_reader = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(10);
    let socket_path = loop {
        let line = timeout(
            deadline.saturating_duration_since(Instant::now()),
            stderr_reader.next_line(),
        )
        .await
        .context("timed out waiting for unix domain socket app-server to report socket path")?
        .context("failed to read unix domain socket app-server stderr")?
        .context("unix domain socket app-server exited before reporting socket path")?;
        eprintln!("[unix domain socket app-server stderr] {line}");

        if let Some(path) = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("uds://"))
            .map(str::to_string)
        {
            break path;
        }
    };

    tokio::spawn(async move {
        while let Ok(Some(line)) = stderr_reader.next_line().await {
            eprintln!("[unix domain socket app-server stderr] {line}");
        }
    });

    Ok((process, socket_path))
}

async fn connect_unix_domain_socket(socket_path: &str) -> Result<UnixSocketClient> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                return Ok(UnixSocketClient {
                    reader: BufReader::new(reader).lines(),
                    writer,
                });
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    bail!("failed to connect unix domain socket to {socket_path}: {err}");
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

struct UnixSocketClient {
    reader: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

async fn send_initialize_request(
    stream: &mut UnixSocketClient,
    id: i64,
    client_name: &str,
) -> Result<()> {
    let params = InitializeParams {
        client_info: ClientInfo {
            name: client_name.to_string(),
            title: Some("Unix Socket Test Client".to_string()),
            version: "0.1.0".to_string(),
        },
        capabilities: None,
    };
    send_request(
        stream,
        "initialize",
        id,
        Some(serde_json::to_value(params)?),
    )
    .await
}

async fn send_config_read_request(stream: &mut UnixSocketClient, id: i64) -> Result<()> {
    send_request(
        stream,
        "config/read",
        id,
        Some(json!({ "includeLayers": false })),
    )
    .await
}

async fn send_request(
    stream: &mut UnixSocketClient,
    method: &str,
    id: i64,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let message = JSONRPCMessage::Request(JSONRPCRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params,
        trace: None,
    });
    let encoded = serde_json::to_string(&message)?;
    stream
        .writer
        .write_all(encoded.as_bytes())
        .await
        .context("failed to write unix domain socket message")?;
    stream
        .writer
        .write_all(b"\n")
        .await
        .context("failed to write unix domain socket newline")?;
    Ok(())
}

async fn read_response_for_id(stream: &mut UnixSocketClient, id: i64) -> Result<JSONRPCResponse> {
    loop {
        let Some(message) = read_message(stream).await? else {
            bail!("unix domain socket app-server closed connection");
        };
        match message {
            JSONRPCMessage::Response(response) if response.id == RequestId::Integer(id) => {
                return Ok(response);
            }
            _ => continue,
        }
    }
}

async fn read_error_for_id(stream: &mut UnixSocketClient, id: i64) -> Result<JSONRPCError> {
    loop {
        let Some(message) = read_message(stream).await? else {
            bail!("unix domain socket app-server closed connection");
        };
        if let JSONRPCMessage::Error(err) = message
            && err.id == RequestId::Integer(id)
        {
            return Ok(err);
        }
    }
}

async fn read_message(stream: &mut UnixSocketClient) -> Result<Option<JSONRPCMessage>> {
    let line = timeout(DEFAULT_READ_TIMEOUT, stream.reader.next_line())
        .await
        .context("timed out waiting for unix domain socket frame")?
        .context("failed to read unix domain socket frame")?;
    line.map(|text| serde_json::from_str(&text).context("failed to decode JSONRPCMessage"))
        .transpose()
}

async fn assert_no_message(stream: &mut UnixSocketClient, wait: Duration) -> Result<()> {
    match timeout(wait, stream.reader.next_line()).await {
        Ok(Ok(Some(line))) => bail!("unexpected unix domain socket message: {line}"),
        Ok(Ok(None)) => bail!("unix domain socket closed unexpectedly while waiting for silence"),
        Ok(Err(err)) => bail!("unexpected unix domain socket read error: {err}"),
        Err(_) => Ok(()),
    }
}
