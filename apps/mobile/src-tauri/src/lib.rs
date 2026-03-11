use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::ErrorKind;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::FileChangeRequestApprovalParams;
use codex_app_server_protocol::FileChangeRequestApprovalResponse;
use codex_app_server_protocol::GrantedMacOsPermissions;
use codex_app_server_protocol::GrantedPermissionProfile;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::PermissionGrantScope;
use codex_app_server_protocol::PermissionsRequestApprovalParams;
use codex_app_server_protocol::PermissionsRequestApprovalResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadActiveFlag;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::UserInput;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::State;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeCapabilities {
    secure_store: bool,
    qr_scanner: bool,
    file_import: bool,
    relay_sockets: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConnectorSnapshot {
    connector_mode: &'static str,
    session: RemoteSessionSnapshot,
    hosts: Vec<HostSummary>,
    device_groups: Vec<DeviceGroup>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteSessionSnapshot {
    signed_in: bool,
    account_label: String,
    workspace_label: String,
    pairing_code: String,
    pairing_url: String,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostSummary {
    id: String,
    name: String,
    platform: String,
    status: &'static str,
    relay_status: String,
    detail: String,
    paired_at: i64,
    last_seen_at: i64,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceGroup {
    host: HostSummary,
    devices: Vec<PairedDevice>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PairedDevice {
    id: String,
    name: String,
    trust: &'static str,
    paired_at: i64,
    last_seen_at: i64,
    transport: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteHostState {
    host_id: String,
    host_name: String,
    platform: String,
    relay: RemoteRelayState,
    created_at: i64,
    updated_at: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteRelayState {
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDevicesState {
    devices: Vec<RemoteDeviceRecord>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDeviceRecord {
    id: String,
    name: String,
    paired_at: i64,
    last_seen_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePairingState {
    sessions: Vec<RemotePairingSession>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePairingSession {
    code: String,
    deep_link: String,
    expires_at: i64,
    used_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Clone)]
struct RemotePaths {
    host_path: PathBuf,
    devices_path: PathBuf,
    pairing_path: PathBuf,
    socket_path: PathBuf,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ComposerMode {
    NewTurn,
    Steer,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteSendPromptInput {
    text: String,
    mode: ComposerMode,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteThreadRuntimeState {
    connection: &'static str,
    phase: &'static str,
    composer_mode: &'static str,
    status_copy: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteThreadRecord {
    host_id: String,
    thread: Thread,
    approvals: Vec<RemoteApproval>,
    runtime: RemoteThreadRuntimeState,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum RemoteApproval {
    Command {
        request_id: String,
        params: CommandExecutionRequestApprovalParams,
        decisions: Vec<CommandExecutionApprovalDecision>,
    },
    FileChange {
        request_id: String,
        params: FileChangeRequestApprovalParams,
        decisions: Vec<FileChangeApprovalDecision>,
    },
    Permissions {
        request_id: String,
        params: PermissionsRequestApprovalParams,
        default_scope: PermissionGrantScope,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteThreadRecordEvent {
    host_id: String,
    thread_id: String,
    record: RemoteThreadRecord,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteApprovalResolution {
    request_id: String,
    decision: RemoteApprovalDecision,
    scope: Option<PermissionGrantScope>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
enum RemoteApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Default)]
struct RemoteThreadStreamState {
    current: Mutex<Option<RemoteThreadStreamHandle>>,
}

#[derive(Default)]
struct RemoteConnectorStreamState {
    current: Mutex<Option<RemoteConnectorStreamHandle>>,
}

struct RemoteConnectorStreamHandle {
    sender: mpsc::Sender<ConnectorStreamCommand>,
    join_handle: thread::JoinHandle<()>,
}

enum ConnectorStreamCommand {
    Stop,
}

struct RemoteThreadStreamHandle {
    host_id: String,
    thread_id: String,
    sender: mpsc::Sender<ThreadStreamCommand>,
    join_handle: thread::JoinHandle<()>,
}

enum ThreadStreamCommand {
    ResolveApproval {
        resolution: RemoteApprovalResolution,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Stop,
}

#[derive(Clone)]
enum PendingApproval {
    Command {
        request_id: RequestId,
        params: CommandExecutionRequestApprovalParams,
        decisions: Vec<CommandExecutionApprovalDecision>,
    },
    FileChange {
        request_id: RequestId,
        params: FileChangeRequestApprovalParams,
        decisions: Vec<FileChangeApprovalDecision>,
    },
    Permissions {
        request_id: RequestId,
        params: PermissionsRequestApprovalParams,
        default_scope: PermissionGrantScope,
    },
}

#[cfg(unix)]
struct LocalPreviewAppServerClient {
    next_request_id: i64,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

#[cfg(unix)]
impl LocalPreviewAppServerClient {
    fn connect(socket_path: &PathBuf) -> Result<Self, String> {
        Self::connect_with_options(socket_path, None, None)
    }

    fn connect_stream(socket_path: &PathBuf) -> Result<Self, String> {
        Self::connect_with_options(
            socket_path,
            Some(Duration::from_millis(200)),
            Some(vec![
                "item/agentMessage/delta".to_string(),
                "item/plan/delta".to_string(),
                "item/reasoning/summaryTextDelta".to_string(),
                "item/reasoning/textDelta".to_string(),
                "item/commandExecution/outputDelta".to_string(),
                "item/fileChange/outputDelta".to_string(),
                "command/exec/outputDelta".to_string(),
            ]),
        )
    }

    fn connect_with_options(
        socket_path: &PathBuf,
        read_timeout: Option<Duration>,
        opt_out_notification_methods: Option<Vec<String>>,
    ) -> Result<Self, String> {
        let writer = UnixStream::connect(socket_path)
            .map_err(|err| format!("failed to connect to app-server socket: {err}"))?;
        writer
            .set_read_timeout(read_timeout)
            .map_err(|err| format!("failed to configure app-server socket timeout: {err}"))?;
        let reader = writer
            .try_clone()
            .map(BufReader::new)
            .map_err(|err| format!("failed to clone app-server socket: {err}"))?;
        let mut client = Self {
            next_request_id: 0,
            reader,
            writer,
        };
        let _: JsonValue = client.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "codex-remote-mobile-preview",
                    "title": "Codex Remote Local Preview",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": false,
                    "optOutNotificationMethods": opt_out_notification_methods,
                },
            }),
        )?;
        client.send_notification(json!({
            "method": "initialized",
        }))?;
        Ok(client)
    }

    fn request<P, T>(&mut self, method: &str, params: P) -> Result<T, String>
    where
        P: Serialize,
        T: for<'de> Deserialize<'de>,
    {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let params = serde_json::to_value(params)
            .map_err(|err| format!("failed to encode {method} params: {err}"))?;
        let message = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        self.write_message(&message, &format!("write {method} request"))?;

        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|err| format!("failed to read {method} response: {err}"))?;
            if read == 0 {
                return Err(format!(
                    "app-server socket closed before responding to {method}"
                ));
            }

            let message: JsonValue = serde_json::from_str(&line)
                .map_err(|err| format!("failed to decode {method} response: {err}"))?;
            if !matches_request_id(&message, request_id) {
                continue;
            }

            if let Some(error) = message.get("error") {
                let detail = error
                    .get("message")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("unknown app-server error");
                return Err(format!("{method} failed: {detail}"));
            }

            let result = message
                .get("result")
                .cloned()
                .ok_or_else(|| format!("{method} response was missing a result payload"))?;
            return serde_json::from_value(result)
                .map_err(|err| format!("failed to decode {method} result: {err}"));
        }
    }

    fn send_notification<T: Serialize>(&mut self, message: T) -> Result<(), String> {
        self.write_message(&message, "write notification")
    }

    fn send_response<T: Serialize>(
        &mut self,
        request_id: RequestId,
        result: T,
    ) -> Result<(), String> {
        self.write_message(
            &JSONRPCResponse {
                id: request_id,
                result: serde_json::to_value(result)
                    .map_err(|err| format!("failed to encode response: {err}"))?,
            },
            "write response",
        )
    }

    fn send_error(
        &mut self,
        request_id: RequestId,
        code: i64,
        message: &str,
    ) -> Result<(), String> {
        self.write_message(
            &JSONRPCError {
                error: JSONRPCErrorError {
                    code,
                    data: None,
                    message: message.to_string(),
                },
                id: request_id,
            },
            "write error response",
        )
    }

    fn write_message<T: Serialize>(&mut self, message: &T, context: &str) -> Result<(), String> {
        serde_json::to_writer(&mut self.writer, message)
            .map_err(|err| format!("failed to {context}: {err}"))?;
        self.writer
            .write_all(b"\n")
            .and_then(|()| self.writer.flush())
            .map_err(|err| format!("failed to {context}: {err}"))?;
        Ok(())
    }
}

impl PendingApproval {
    fn request_id(&self) -> &RequestId {
        match self {
            Self::Command { request_id, .. }
            | Self::FileChange { request_id, .. }
            | Self::Permissions { request_id, .. } => request_id,
        }
    }

    fn request_token(&self) -> Result<String, String> {
        serialize_request_id(self.request_id())
    }

    fn to_remote_approval(&self) -> Result<RemoteApproval, String> {
        let request_id = self.request_token()?;
        Ok(match self {
            Self::Command {
                params, decisions, ..
            } => RemoteApproval::Command {
                request_id,
                params: params.clone(),
                decisions: decisions.clone(),
            },
            Self::FileChange {
                params, decisions, ..
            } => RemoteApproval::FileChange {
                request_id,
                params: params.clone(),
                decisions: decisions.clone(),
            },
            Self::Permissions {
                params,
                default_scope,
                ..
            } => RemoteApproval::Permissions {
                request_id,
                params: params.clone(),
                default_scope: default_scope.clone(),
            },
        })
    }
}

fn serialize_request_id(request_id: &RequestId) -> Result<String, String> {
    serde_json::to_string(request_id)
        .map_err(|err| format!("failed to serialize app-server request id: {err}"))
}

fn deserialize_request_id(request_id: &str) -> Result<RequestId, String> {
    serde_json::from_str(request_id)
        .map_err(|err| format!("failed to decode app-server request id: {err}"))
}

fn default_command_decisions(
    params: &CommandExecutionRequestApprovalParams,
) -> Vec<CommandExecutionApprovalDecision> {
    params.available_decisions.clone().unwrap_or_else(|| {
        vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::AcceptForSession,
            CommandExecutionApprovalDecision::Decline,
            CommandExecutionApprovalDecision::Cancel,
        ]
    })
}

fn file_change_decisions() -> Vec<FileChangeApprovalDecision> {
    vec![
        FileChangeApprovalDecision::Accept,
        FileChangeApprovalDecision::AcceptForSession,
        FileChangeApprovalDecision::Decline,
        FileChangeApprovalDecision::Cancel,
    ]
}

fn granted_permissions_from_additional(
    permissions: &codex_app_server_protocol::AdditionalPermissionProfile,
) -> GrantedPermissionProfile {
    GrantedPermissionProfile {
        network: permissions.network.clone(),
        file_system: permissions.file_system.clone(),
        macos: permissions
            .macos
            .clone()
            .map(|macos| GrantedMacOsPermissions {
                preferences: Some(macos.preferences),
                automations: Some(macos.automations),
                accessibility: Some(macos.accessibility),
                calendar: Some(macos.calendar),
            }),
    }
}

#[tauri::command]
fn read_native_capabilities() -> NativeCapabilities {
    NativeCapabilities {
        secure_store: cfg!(target_os = "ios"),
        qr_scanner: cfg!(target_os = "ios"),
        file_import: true,
        relay_sockets: true,
    }
}

#[tauri::command]
fn pick_attachment_import() -> Option<String> {
    None
}

#[tauri::command]
fn read_remote_connector_snapshot() -> Option<RemoteConnectorSnapshot> {
    let paths = remote_paths()?;
    load_remote_connector_snapshot(&paths)
}

#[tauri::command]
fn start_remote_connector_stream(
    app: AppHandle,
    stream_state: State<'_, RemoteConnectorStreamState>,
) -> Result<(), String> {
    let Some(paths) = remote_paths() else {
        return Err("CODEX_HOME is unavailable for local preview.".to_string());
    };
    let previous_handle = {
        let mut current = stream_state
            .current
            .lock()
            .map_err(|_| "remote connector stream state is unavailable".to_string())?;
        if let Some(handle) = current.as_ref()
            && !handle.join_handle.is_finished()
        {
            return Ok(());
        }
        current.take()
    };
    if let Some(handle) = previous_handle {
        shutdown_connector_stream(handle)?;
    }

    let (sender, receiver) = mpsc::channel();
    let (ready_sender, ready_receiver) = mpsc::channel();
    let worker_app = app.clone();
    let worker_paths = paths.clone();
    let join_handle = thread::spawn(move || {
        run_remote_connector_stream(worker_app, worker_paths, receiver, ready_sender);
    });

    match ready_receiver.recv() {
        Ok(Ok(())) => {
            let mut current = stream_state
                .current
                .lock()
                .map_err(|_| "remote connector stream state is unavailable".to_string())?;
            *current = Some(RemoteConnectorStreamHandle {
                sender,
                join_handle,
            });
            Ok(())
        }
        Ok(Err(err)) => {
            let _ = join_handle.join();
            Err(err)
        }
        Err(err) => {
            let _ = join_handle.join();
            Err(format!("connector stream failed to initialize: {err}"))
        }
    }
}

#[tauri::command]
fn stop_remote_connector_stream(
    stream_state: State<'_, RemoteConnectorStreamState>,
) -> Result<(), String> {
    let handle = {
        let mut current = stream_state
            .current
            .lock()
            .map_err(|_| "remote connector stream state is unavailable".to_string())?;
        current.take()
    };
    if let Some(handle) = handle {
        shutdown_connector_stream(handle)?;
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn list_remote_threads(host_id: String) -> Result<Vec<Thread>, String> {
    let Some(paths) = remote_paths() else {
        return Ok(Vec::new());
    };
    if !paths.socket_path.exists() {
        return Ok(Vec::new());
    }
    let _host = ensure_known_host(&paths, &host_id)?;
    list_threads_from_app_server(&paths)
}

#[tauri::command(rename_all = "camelCase")]
fn read_remote_thread_record(
    host_id: String,
    thread_id: String,
) -> Result<Option<RemoteThreadRecord>, String> {
    let Some(paths) = remote_paths() else {
        return Ok(None);
    };
    if !paths.socket_path.exists() {
        return Ok(None);
    }
    let _host = ensure_known_host(&paths, &host_id)?;
    let thread = read_thread_from_app_server(&paths, &thread_id)?;
    Ok(Some(build_thread_record(
        host_id,
        thread,
        Vec::new(),
        "online",
    )))
}

#[tauri::command(rename_all = "camelCase")]
fn send_remote_prompt(
    host_id: String,
    thread_id: String,
    input: RemoteSendPromptInput,
) -> Result<(), String> {
    let Some(paths) = remote_paths() else {
        return Err("CODEX_HOME is unavailable for local preview.".to_string());
    };
    let _host = ensure_known_host(&paths, &host_id)?;
    let mut client = connect_local_preview_client(&paths)?;
    let thread: Thread = client
        .request(
            "thread/read",
            ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        )
        .map(|response: ThreadReadResponse| response.thread)?;
    let text_input = vec![UserInput::Text {
        text: input.text,
        text_elements: Vec::new(),
    }];

    let should_resume = matches!(thread.status, ThreadStatus::NotLoaded);
    if should_resume {
        let _: ThreadResumeResponse = client.request(
            "thread/resume",
            ThreadResumeParams {
                thread_id: thread_id.clone(),
                history: None,
                path: None,
                model: None,
                model_provider: None,
                service_tier: None,
                cwd: None,
                approval_policy: None,
                sandbox: None,
                config: None,
                base_instructions: None,
                developer_instructions: None,
                personality: None,
                persist_extended_history: false,
            },
        )?;
    }

    match (input.mode, active_turn_id(&thread)) {
        (ComposerMode::Steer, Some(turn_id)) => {
            let _: TurnSteerResponse = client.request(
                "turn/steer",
                TurnSteerParams {
                    thread_id,
                    input: text_input,
                    expected_turn_id: turn_id.to_string(),
                },
            )?;
        }
        _ => {
            let _: TurnStartResponse = client.request(
                "turn/start",
                TurnStartParams {
                    thread_id,
                    input: text_input,
                    ..TurnStartParams::default()
                },
            )?;
        }
    }

    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn interrupt_remote_turn(host_id: String, thread_id: String) -> Result<(), String> {
    let Some(paths) = remote_paths() else {
        return Err("CODEX_HOME is unavailable for local preview.".to_string());
    };
    let _host = ensure_known_host(&paths, &host_id)?;
    let mut client = connect_local_preview_client(&paths)?;
    let thread: Thread = client
        .request(
            "thread/read",
            ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        )
        .map(|response: ThreadReadResponse| response.thread)?;
    let Some(turn_id) = active_turn_id(&thread) else {
        return Ok(());
    };
    let _: TurnInterruptResponse = client.request(
        "turn/interrupt",
        TurnInterruptParams {
            thread_id,
            turn_id: turn_id.to_string(),
        },
    )?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn start_remote_thread_stream(
    app: AppHandle,
    stream_state: State<'_, RemoteThreadStreamState>,
    host_id: String,
    thread_id: String,
) -> Result<(), String> {
    #[cfg(not(unix))]
    {
        let _ = (app, stream_state, host_id, thread_id);
        Err("Local preview UDS transport is only available on unix hosts.".to_string())
    }

    #[cfg(unix)]
    {
        let Some(paths) = remote_paths() else {
            return Err("CODEX_HOME is unavailable for local preview.".to_string());
        };
        let _host = ensure_known_host(&paths, &host_id)?;
        let previous_handle = {
            let mut current = stream_state
                .current
                .lock()
                .map_err(|_| "remote thread stream state is unavailable".to_string())?;
            if let Some(handle) = current.as_ref()
                && handle.host_id == host_id
                && handle.thread_id == thread_id
                && !handle.join_handle.is_finished()
            {
                return Ok(());
            }
            current.take()
        };
        if let Some(handle) = previous_handle {
            shutdown_stream(handle)?;
        }

        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let worker_app = app.clone();
        let worker_paths = paths.clone();
        let worker_host_id = host_id.clone();
        let worker_thread_id = thread_id.clone();
        let join_handle = thread::spawn(move || {
            run_remote_thread_stream(
                worker_app,
                worker_paths,
                worker_host_id,
                worker_thread_id,
                receiver,
                ready_sender,
            );
        });

        match ready_receiver.recv() {
            Ok(Ok(())) => {
                let mut current = stream_state
                    .current
                    .lock()
                    .map_err(|_| "remote thread stream state is unavailable".to_string())?;
                *current = Some(RemoteThreadStreamHandle {
                    host_id,
                    thread_id,
                    sender,
                    join_handle,
                });
                Ok(())
            }
            Ok(Err(err)) => {
                let _ = join_handle.join();
                Err(err)
            }
            Err(err) => {
                let _ = join_handle.join();
                Err(format!("live thread stream failed to initialize: {err}"))
            }
        }
    }
}

#[tauri::command(rename_all = "camelCase")]
fn stop_remote_thread_stream(
    stream_state: State<'_, RemoteThreadStreamState>,
    host_id: String,
    thread_id: String,
) -> Result<(), String> {
    let handle = {
        let mut current = stream_state
            .current
            .lock()
            .map_err(|_| "remote thread stream state is unavailable".to_string())?;
        if let Some(handle) = current.as_ref()
            && handle.host_id == host_id
            && handle.thread_id == thread_id
        {
            current.take()
        } else {
            None
        }
    };
    if let Some(handle) = handle {
        shutdown_stream(handle)?;
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn resolve_remote_approval(
    stream_state: State<'_, RemoteThreadStreamState>,
    host_id: String,
    thread_id: String,
    resolution: RemoteApprovalResolution,
) -> Result<(), String> {
    let sender = {
        let current = stream_state
            .current
            .lock()
            .map_err(|_| "remote thread stream state is unavailable".to_string())?;
        let Some(handle) = current.as_ref() else {
            return Err("No live thread stream is active for local preview.".to_string());
        };
        if handle.host_id != host_id || handle.thread_id != thread_id {
            return Err("A different live thread stream is active.".to_string());
        }
        if handle.join_handle.is_finished() {
            return Err("The live thread stream is no longer connected.".to_string());
        }
        handle.sender.clone()
    };
    let (reply_sender, reply_receiver) = mpsc::channel();
    sender
        .send(ThreadStreamCommand::ResolveApproval {
            resolution,
            reply: reply_sender,
        })
        .map_err(|_| "The live thread stream is unavailable.".to_string())?;
    reply_receiver
        .recv()
        .map_err(|_| "The live thread stream did not acknowledge the approval.".to_string())?
}

fn shutdown_connector_stream(handle: RemoteConnectorStreamHandle) -> Result<(), String> {
    let _ = handle.sender.send(ConnectorStreamCommand::Stop);
    handle
        .join_handle
        .join()
        .map_err(|_| "connector stream panicked".to_string())
}

fn run_remote_connector_stream(
    app: AppHandle,
    paths: RemotePaths,
    receiver: mpsc::Receiver<ConnectorStreamCommand>,
    ready_sender: mpsc::Sender<Result<(), String>>,
) {
    let mut last_snapshot = load_remote_connector_snapshot(&paths);
    if let Some(snapshot) = last_snapshot.clone()
        && let Err(err) = emit_remote_connector_snapshot(&app, snapshot)
    {
        let _ = ready_sender.send(Err(err));
        return;
    }
    if ready_sender.send(Ok(())).is_err() {
        return;
    }

    loop {
        match receiver.recv_timeout(Duration::from_millis(750)) {
            Ok(ConnectorStreamCommand::Stop) => return,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let next_snapshot = load_remote_connector_snapshot(&paths);
        if next_snapshot == last_snapshot {
            continue;
        }
        last_snapshot = next_snapshot.clone();
        if let Some(snapshot) = next_snapshot
            && emit_remote_connector_snapshot(&app, snapshot).is_err()
        {
            return;
        }
    }
}

fn shutdown_stream(handle: RemoteThreadStreamHandle) -> Result<(), String> {
    let _ = handle.sender.send(ThreadStreamCommand::Stop);
    handle
        .join_handle
        .join()
        .map_err(|_| "live thread stream panicked".to_string())
}

#[cfg(unix)]
fn run_remote_thread_stream(
    app: AppHandle,
    paths: RemotePaths,
    host_id: String,
    thread_id: String,
    receiver: mpsc::Receiver<ThreadStreamCommand>,
    ready_sender: mpsc::Sender<Result<(), String>>,
) {
    let mut client = match connect_local_preview_stream_client(&paths) {
        Ok(client) => client,
        Err(err) => {
            let _ = ready_sender.send(Err(err));
            return;
        }
    };
    let resumed_thread = match client.request(
        "thread/resume",
        ThreadResumeParams {
            thread_id: thread_id.clone(),
            history: None,
            path: None,
            model: None,
            model_provider: None,
            service_tier: None,
            cwd: None,
            approval_policy: None,
            sandbox: None,
            config: None,
            base_instructions: None,
            developer_instructions: None,
            personality: None,
            persist_extended_history: false,
        },
    ) {
        Ok::<ThreadResumeResponse, String>(response) => response.thread,
        Err(err) => {
            let _ = ready_sender.send(Err(err));
            return;
        }
    };
    let mut last_thread = Some(resumed_thread);
    let mut pending_approvals = Vec::new();
    if let Err(err) = emit_latest_thread_record(
        &app,
        &paths,
        &host_id,
        &thread_id,
        &pending_approvals,
        &mut last_thread,
    ) {
        let _ = ready_sender.send(Err(err));
        return;
    }
    if ready_sender.send(Ok(())).is_err() {
        return;
    }

    loop {
        while let Ok(command) = receiver.try_recv() {
            match command {
                ThreadStreamCommand::ResolveApproval { resolution, reply } => {
                    let result = resolve_pending_approval(
                        &mut client,
                        &app,
                        &paths,
                        &host_id,
                        &thread_id,
                        &mut pending_approvals,
                        &mut last_thread,
                        resolution,
                    );
                    let _ = reply.send(result);
                }
                ThreadStreamCommand::Stop => return,
            }
        }

        let mut line = String::new();
        match client.reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => return,
        }
        if line.trim().is_empty() {
            continue;
        }

        let Ok(message) = serde_json::from_str::<JSONRPCMessage>(&line) else {
            continue;
        };
        match message {
            JSONRPCMessage::Request(request) => {
                let Ok(server_request) = ServerRequest::try_from(request) else {
                    continue;
                };
                match server_request {
                    ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
                        pending_approvals.push(PendingApproval::Command {
                            request_id,
                            decisions: default_command_decisions(&params),
                            params,
                        });
                        let _ = emit_latest_thread_record(
                            &app,
                            &paths,
                            &host_id,
                            &thread_id,
                            &pending_approvals,
                            &mut last_thread,
                        );
                    }
                    ServerRequest::FileChangeRequestApproval { request_id, params } => {
                        pending_approvals.push(PendingApproval::FileChange {
                            request_id,
                            decisions: file_change_decisions(),
                            params,
                        });
                        let _ = emit_latest_thread_record(
                            &app,
                            &paths,
                            &host_id,
                            &thread_id,
                            &pending_approvals,
                            &mut last_thread,
                        );
                    }
                    ServerRequest::PermissionsRequestApproval { request_id, params } => {
                        pending_approvals.push(PendingApproval::Permissions {
                            request_id,
                            params,
                            default_scope: PermissionGrantScope::Turn,
                        });
                        let _ = emit_latest_thread_record(
                            &app,
                            &paths,
                            &host_id,
                            &thread_id,
                            &pending_approvals,
                            &mut last_thread,
                        );
                    }
                    other => {
                        let _ = client.send_error(
                            other.id().clone(),
                            -32601,
                            "Codex Remote local preview does not support this server request.",
                        );
                    }
                }
            }
            JSONRPCMessage::Notification(notification) => {
                let Ok(server_notification) = ServerNotification::try_from(notification) else {
                    continue;
                };
                if let ServerNotification::ServerRequestResolved(params) = &server_notification {
                    if params.thread_id == thread_id {
                        pending_approvals
                            .retain(|approval| approval.request_id() != &params.request_id);
                    } else {
                        continue;
                    }
                }
                if should_refresh_thread_record(&server_notification, &thread_id) {
                    let _ = emit_latest_thread_record(
                        &app,
                        &paths,
                        &host_id,
                        &thread_id,
                        &pending_approvals,
                        &mut last_thread,
                    );
                }
            }
            JSONRPCMessage::Response(_) | JSONRPCMessage::Error(_) => {}
        }
    }
}

#[cfg(not(unix))]
fn run_remote_thread_stream(
    _app: AppHandle,
    _paths: RemotePaths,
    _host_id: String,
    _thread_id: String,
    _receiver: mpsc::Receiver<ThreadStreamCommand>,
    ready_sender: mpsc::Sender<Result<(), String>>,
) {
    let _ = ready_sender.send(Err(
        "Local preview UDS transport is only available on unix hosts.".to_string(),
    ));
}

fn should_refresh_thread_record(notification: &ServerNotification, thread_id: &str) -> bool {
    match notification {
        ServerNotification::ThreadStatusChanged(params) => params.thread_id == thread_id,
        ServerNotification::TurnStarted(params) => params.thread_id == thread_id,
        ServerNotification::TurnCompleted(params) => params.thread_id == thread_id,
        ServerNotification::ItemStarted(params) => params.thread_id == thread_id,
        ServerNotification::ItemCompleted(params) => params.thread_id == thread_id,
        ServerNotification::ServerRequestResolved(params) => params.thread_id == thread_id,
        _ => false,
    }
}

fn resolve_pending_approval(
    client: &mut LocalPreviewAppServerClient,
    app: &AppHandle,
    paths: &RemotePaths,
    host_id: &str,
    thread_id: &str,
    pending_approvals: &mut Vec<PendingApproval>,
    last_thread: &mut Option<Thread>,
    resolution: RemoteApprovalResolution,
) -> Result<(), String> {
    let request_id = deserialize_request_id(&resolution.request_id)?;
    let Some(index) = pending_approvals
        .iter()
        .position(|approval| approval.request_id() == &request_id)
    else {
        return Err("The requested approval is no longer pending.".to_string());
    };
    let approval = pending_approvals.remove(index);
    match approval {
        PendingApproval::Command { request_id, .. } => {
            client.send_response(
                request_id,
                CommandExecutionRequestApprovalResponse {
                    decision: match resolution.decision {
                        RemoteApprovalDecision::Accept => CommandExecutionApprovalDecision::Accept,
                        RemoteApprovalDecision::AcceptForSession => {
                            CommandExecutionApprovalDecision::AcceptForSession
                        }
                        RemoteApprovalDecision::Decline => {
                            CommandExecutionApprovalDecision::Decline
                        }
                        RemoteApprovalDecision::Cancel => CommandExecutionApprovalDecision::Cancel,
                    },
                },
            )?;
        }
        PendingApproval::FileChange { request_id, .. } => {
            client.send_response(
                request_id,
                FileChangeRequestApprovalResponse {
                    decision: match resolution.decision {
                        RemoteApprovalDecision::Accept => FileChangeApprovalDecision::Accept,
                        RemoteApprovalDecision::AcceptForSession => {
                            FileChangeApprovalDecision::AcceptForSession
                        }
                        RemoteApprovalDecision::Decline => FileChangeApprovalDecision::Decline,
                        RemoteApprovalDecision::Cancel => FileChangeApprovalDecision::Cancel,
                    },
                },
            )?;
        }
        PendingApproval::Permissions {
            request_id,
            params,
            default_scope,
        } => {
            let granted = match resolution.decision {
                RemoteApprovalDecision::Accept | RemoteApprovalDecision::AcceptForSession => {
                    granted_permissions_from_additional(&params.permissions)
                }
                RemoteApprovalDecision::Decline | RemoteApprovalDecision::Cancel => {
                    GrantedPermissionProfile::default()
                }
            };
            let scope = match resolution.decision {
                RemoteApprovalDecision::Accept => resolution.scope.unwrap_or(default_scope),
                RemoteApprovalDecision::AcceptForSession => PermissionGrantScope::Session,
                RemoteApprovalDecision::Decline | RemoteApprovalDecision::Cancel => {
                    PermissionGrantScope::Turn
                }
            };
            client.send_response(
                request_id,
                PermissionsRequestApprovalResponse {
                    permissions: granted,
                    scope,
                },
            )?;
        }
    }
    emit_latest_thread_record(
        app,
        paths,
        host_id,
        thread_id,
        pending_approvals,
        last_thread,
    )
}

fn remote_paths() -> Option<RemotePaths> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))?;
    let remote_dir = codex_home.join("remote");
    Some(RemotePaths {
        host_path: remote_dir.join("host.json"),
        devices_path: remote_dir.join("devices.json"),
        pairing_path: remote_dir.join("pairing.json"),
        socket_path: remote_dir.join("app-server.sock"),
    })
}

fn load_remote_connector_snapshot(paths: &RemotePaths) -> Option<RemoteConnectorSnapshot> {
    let host: RemoteHostState = read_json_file(&paths.host_path)?;
    let devices =
        read_json_file::<RemoteDevicesState>(&paths.devices_path).unwrap_or(RemoteDevicesState {
            devices: Vec::new(),
        });
    let pairing =
        read_json_file::<RemotePairingState>(&paths.pairing_path).unwrap_or(RemotePairingState {
            sessions: Vec::new(),
        });
    let now = now_unix_seconds()?;
    let active_pairing = pairing
        .sessions
        .into_iter()
        .filter(|session| {
            session.used_at.is_none() && session.revoked_at.is_none() && session.expires_at > now
        })
        .max_by_key(|session| session.expires_at);

    let host_summary = HostSummary {
        detail: if active_pairing.is_some() {
            "Local preview from CODEX_HOME/remote with an active pairing session.".to_string()
        } else {
            "Local preview from CODEX_HOME/remote without an active pairing session.".to_string()
        },
        id: host.host_id.clone(),
        last_seen_at: host.updated_at,
        name: host.host_name.clone(),
        paired_at: host.created_at,
        platform: platform_label(&host.platform),
        relay_status: host.relay.status,
        status: if paths.socket_path.exists() {
            "online"
        } else {
            "offline"
        },
    };

    Some(RemoteConnectorSnapshot {
        connector_mode: "localPreview",
        session: RemoteSessionSnapshot {
            signed_in: true,
            account_label: "Local preview".to_string(),
            pairing_code: active_pairing
                .as_ref()
                .map(|session| session.code.clone())
                .unwrap_or_else(|| "Run codex remote pair".to_string()),
            pairing_url: active_pairing
                .as_ref()
                .map(|session| session.deep_link.clone())
                .unwrap_or_default(),
            workspace_label: host.host_name,
        },
        hosts: vec![host_summary.clone()],
        device_groups: vec![DeviceGroup {
            host: host_summary,
            devices: devices
                .devices
                .into_iter()
                .map(|device| PairedDevice {
                    id: device.id,
                    last_seen_at: device.last_seen_at.unwrap_or(device.paired_at),
                    name: device.name,
                    paired_at: device.paired_at,
                    transport: "Local preview".to_string(),
                    trust: if device.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "trusted"
                    },
                })
                .collect(),
        }],
    })
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &PathBuf) -> Option<T> {
    let contents = fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn ensure_known_host(paths: &RemotePaths, host_id: &str) -> Result<RemoteHostState, String> {
    let host: RemoteHostState = read_json_file(&paths.host_path)
        .ok_or_else(|| "remote host state is unavailable".to_string())?;
    if host.host_id != host_id {
        return Err(format!("unknown local preview host: {host_id}"));
    }
    Ok(host)
}

#[cfg(unix)]
fn connect_local_preview_client(
    paths: &RemotePaths,
) -> Result<LocalPreviewAppServerClient, String> {
    if !paths.socket_path.exists() {
        return Err("Host daemon is offline.".to_string());
    }
    LocalPreviewAppServerClient::connect(&paths.socket_path)
}

#[cfg(not(unix))]
fn connect_local_preview_client(_paths: &RemotePaths) -> Result<(), String> {
    Err("Local preview UDS transport is only available on unix hosts.".to_string())
}

#[cfg(unix)]
fn connect_local_preview_stream_client(
    paths: &RemotePaths,
) -> Result<LocalPreviewAppServerClient, String> {
    if !paths.socket_path.exists() {
        return Err("Host daemon is offline.".to_string());
    }
    LocalPreviewAppServerClient::connect_stream(&paths.socket_path)
}

#[cfg(not(unix))]
fn connect_local_preview_stream_client(_paths: &RemotePaths) -> Result<(), String> {
    Err("Local preview UDS transport is only available on unix hosts.".to_string())
}

#[cfg(unix)]
fn list_threads_from_app_server(paths: &RemotePaths) -> Result<Vec<Thread>, String> {
    let mut client = connect_local_preview_client(paths)?;
    let mut cursor = None;
    let mut threads = Vec::new();

    loop {
        let response: ThreadListResponse = client.request(
            "thread/list",
            ThreadListParams {
                cursor: cursor.clone(),
                limit: Some(100),
                sort_key: Some(ThreadSortKey::UpdatedAt),
                model_providers: None,
                source_kinds: None,
                archived: None,
                cwd: None,
                search_term: None,
            },
        )?;
        threads.extend(response.data);
        if response.next_cursor.is_none() {
            return Ok(threads);
        }
        cursor = response.next_cursor;
    }
}

#[cfg(not(unix))]
fn list_threads_from_app_server(_paths: &RemotePaths) -> Result<Vec<Thread>, String> {
    Err("Local preview UDS transport is only available on unix hosts.".to_string())
}

fn read_thread_from_app_server(paths: &RemotePaths, thread_id: &str) -> Result<Thread, String> {
    let mut client = connect_local_preview_client(paths)?;
    client
        .request(
            "thread/read",
            ThreadReadParams {
                thread_id: thread_id.to_string(),
                include_turns: true,
            },
        )
        .map(|response: ThreadReadResponse| response.thread)
}

fn emit_latest_thread_record(
    app: &AppHandle,
    paths: &RemotePaths,
    host_id: &str,
    thread_id: &str,
    pending_approvals: &[PendingApproval],
    last_thread: &mut Option<Thread>,
) -> Result<(), String> {
    let (thread, connection) = match read_thread_from_app_server(paths, thread_id) {
        Ok(thread) => {
            *last_thread = Some(thread.clone());
            (thread, "online")
        }
        Err(err) => {
            let Some(thread) = last_thread.clone() else {
                return Err(err);
            };
            (thread, "offline")
        }
    };
    emit_thread_record(
        app,
        RemoteThreadRecordEvent {
            host_id: host_id.to_string(),
            thread_id: thread_id.to_string(),
            record: build_thread_record(
                host_id.to_string(),
                thread,
                remote_approvals_from_pending(pending_approvals)?,
                connection,
            ),
        },
    )
}

fn remote_approvals_from_pending(
    pending_approvals: &[PendingApproval],
) -> Result<Vec<RemoteApproval>, String> {
    pending_approvals
        .iter()
        .map(PendingApproval::to_remote_approval)
        .collect()
}

fn emit_thread_record(app: &AppHandle, event: RemoteThreadRecordEvent) -> Result<(), String> {
    app.emit("remote-thread-record", event)
        .map_err(|err| format!("failed to emit remote thread record: {err}"))
}

fn emit_remote_connector_snapshot(
    app: &AppHandle,
    snapshot: RemoteConnectorSnapshot,
) -> Result<(), String> {
    app.emit("remote-connector-snapshot", snapshot)
        .map_err(|err| format!("failed to emit remote connector snapshot: {err}"))
}

fn now_unix_seconds() -> Option<i64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

fn platform_label(platform: &str) -> String {
    match platform {
        "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        other => other.to_string(),
    }
}

fn matches_request_id(message: &JsonValue, request_id: i64) -> bool {
    message.get("id").and_then(JsonValue::as_i64) == Some(request_id)
}

fn build_thread_record(
    host_id: String,
    thread: Thread,
    approvals: Vec<RemoteApproval>,
    connection: &'static str,
) -> RemoteThreadRecord {
    let active_turn_id = active_turn_id(&thread);
    let waiting_on_approval = waiting_on_approval(&thread) || !approvals.is_empty();
    let runtime = RemoteThreadRuntimeState {
        composer_mode: if active_turn_id.is_some() {
            "steer"
        } else {
            "newTurn"
        },
        connection,
        phase: if waiting_on_approval {
            "waitingOnApproval"
        } else if active_turn_id.is_some() {
            "running"
        } else {
            "completed"
        },
        status_copy: status_copy(
            &thread,
            connection,
            active_turn_id.is_some(),
            waiting_on_approval,
        ),
    };

    RemoteThreadRecord {
        host_id,
        thread,
        approvals,
        runtime,
    }
}

fn active_turn_id(thread: &Thread) -> Option<&str> {
    thread
        .turns
        .iter()
        .rev()
        .find(|turn| matches!(turn.status, TurnStatus::InProgress))
        .map(|turn| turn.id.as_str())
}

fn waiting_on_approval(thread: &Thread) -> bool {
    match &thread.status {
        ThreadStatus::Active { active_flags } => active_flags
            .iter()
            .any(|flag| matches!(flag, ThreadActiveFlag::WaitingOnApproval)),
        ThreadStatus::NotLoaded | ThreadStatus::Idle | ThreadStatus::SystemError => false,
    }
}

fn status_copy(
    thread: &Thread,
    connection: &str,
    has_active_turn: bool,
    waiting_on_approval: bool,
) -> String {
    if connection == "offline" {
        return "Host daemon is offline; live updates are paused.".to_string();
    }
    if waiting_on_approval {
        return "Waiting on approval from a connected Codex client.".to_string();
    }
    if has_active_turn {
        return "Turn is active on the host.".to_string();
    }

    match &thread.status {
        ThreadStatus::NotLoaded => "Thread history loaded from the host daemon.".to_string(),
        ThreadStatus::Idle => "No active turn on this thread.".to_string(),
        ThreadStatus::SystemError => "Thread hit a system error on the host.".to_string(),
        ThreadStatus::Active { .. } => "Turn is active on the host.".to_string(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(RemoteConnectorStreamState::default())
        .manage(RemoteThreadStreamState::default())
        .invoke_handler(tauri::generate_handler![
            read_native_capabilities,
            pick_attachment_import,
            read_remote_connector_snapshot,
            start_remote_connector_stream,
            stop_remote_connector_stream,
            list_remote_threads,
            read_remote_thread_record,
            send_remote_prompt,
            interrupt_remote_turn,
            start_remote_thread_stream,
            stop_remote_thread_stream,
            resolve_remote_approval
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Codex Remote");
}
