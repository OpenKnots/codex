//! Shared app-server client facade for CLI surfaces.
//!
//! This crate wraps both [`codex_app_server::in_process`] and the local unix
//! domain socket transport behind a single async API used by surfaces like TUI
//! and exec. It centralizes:
//!
//! - Runtime startup and initialize-capabilities handshake.
//! - Typed caller-provided startup identity (`SessionSource` + client name).
//! - Typed and raw request/notification dispatch.
//! - Server request resolution and rejection.
//! - Event consumption with backpressure signaling ([`InProcessServerEvent::Lagged`]).
//! - Bounded graceful shutdown with abort fallback.
//!
//! Each client interposes a worker task between the caller and the underlying
//! transport, bridging async `mpsc` channels on both sides. Queues are bounded
//! so overload surfaces as channel-full errors rather than unbounded memory
//! growth.

use std::error::Error;
use std::fmt;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Error as IoError;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use codex_app_server::in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
pub use codex_app_server::in_process::InProcessServerEvent;
use codex_app_server::in_process::InProcessStartArgs;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientNotification;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCRequest;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Result as JsonRpcResult;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_core::config_loader::CloudRequirementsLoader;
use codex_core::config_loader::LoaderOverrides;
use codex_feedback::CodexFeedback;
use codex_protocol::protocol::SessionSource;
use serde::de::DeserializeOwned;
use tokio::io::AsyncBufRead;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;
use toml::Value as TomlValue;
use tracing::warn;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Raw app-server request result for typed in-process requests.
///
/// Even on the in-process path, successful responses still travel back through
/// the same JSON-RPC result envelope used by socket/stdio transports because
/// `MessageProcessor` continues to produce that shape internally.
pub type RequestResult = std::result::Result<JsonRpcResult, JSONRPCErrorError>;

fn event_requires_delivery(event: &InProcessServerEvent) -> bool {
    // These terminal events drive surface shutdown/completion state. Dropping
    // them under backpressure can leave exec/TUI waiting forever even though
    // the underlying turn has already ended.
    match event {
        InProcessServerEvent::ServerNotification(
            codex_app_server_protocol::ServerNotification::TurnCompleted(_),
        ) => true,
        InProcessServerEvent::LegacyNotification(notification) => matches!(
            notification
                .method
                .strip_prefix("codex/event/")
                .unwrap_or(&notification.method),
            "task_complete" | "turn_aborted" | "shutdown_complete"
        ),
        _ => false,
    }
}

/// Layered error for [`InProcessAppServerClient::request_typed`].
///
/// This keeps transport failures, server-side JSON-RPC failures, and response
/// decode failures distinct so callers can decide whether to retry, surface a
/// server error, or treat the response as an internal request/response mismatch.
#[derive(Debug)]
pub enum TypedRequestError {
    Transport {
        method: String,
        source: IoError,
    },
    Server {
        method: String,
        source: JSONRPCErrorError,
    },
    Deserialize {
        method: String,
        source: serde_json::Error,
    },
}

impl fmt::Display for TypedRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { method, source } => {
                write!(f, "{method} transport error: {source}")
            }
            Self::Server { method, source } => {
                write!(f, "{method} failed: {}", source.message)
            }
            Self::Deserialize { method, source } => {
                write!(f, "{method} response decode error: {source}")
            }
        }
    }
}

impl Error for TypedRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport { source, .. } => Some(source),
            Self::Server { .. } => None,
            Self::Deserialize { source, .. } => Some(source),
        }
    }
}

#[derive(Clone)]
pub struct InProcessClientStartArgs {
    /// Resolved argv0 dispatch paths used by command execution internals.
    pub arg0_paths: Arg0DispatchPaths,
    /// Shared config used to initialize app-server runtime.
    pub config: Arc<Config>,
    /// CLI config overrides that are already parsed into TOML values.
    pub cli_overrides: Vec<(String, TomlValue)>,
    /// Loader override knobs used by config API paths.
    pub loader_overrides: LoaderOverrides,
    /// Preloaded cloud requirements provider.
    pub cloud_requirements: CloudRequirementsLoader,
    /// Feedback sink used by app-server/core telemetry and logs.
    pub feedback: CodexFeedback,
    /// Startup warnings emitted after initialize succeeds.
    pub config_warnings: Vec<ConfigWarningNotification>,
    /// Session source recorded in app-server thread metadata.
    pub session_source: SessionSource,
    /// Whether auth loading should honor the `CODEX_API_KEY` environment variable.
    pub enable_codex_api_key_env: bool,
    /// Client name reported during initialize.
    pub client_name: String,
    /// Client version reported during initialize.
    pub client_version: String,
    /// Whether experimental APIs are requested at initialize time.
    pub experimental_api: bool,
    /// Notification methods this client opts out of receiving.
    pub opt_out_notification_methods: Vec<String>,
    /// Queue capacity for command/event channels (clamped to at least 1).
    pub channel_capacity: usize,
}

impl InProcessClientStartArgs {
    /// Builds initialize params from caller-provided metadata.
    pub fn initialize_params(&self) -> InitializeParams {
        let capabilities = InitializeCapabilities {
            experimental_api: self.experimental_api,
            opt_out_notification_methods: if self.opt_out_notification_methods.is_empty() {
                None
            } else {
                Some(self.opt_out_notification_methods.clone())
            },
        };

        InitializeParams {
            client_info: ClientInfo {
                name: self.client_name.clone(),
                title: None,
                version: self.client_version.clone(),
            },
            capabilities: Some(capabilities),
        }
    }

    fn into_runtime_start_args(self) -> InProcessStartArgs {
        let initialize = self.initialize_params();
        InProcessStartArgs {
            arg0_paths: self.arg0_paths,
            config: self.config,
            cli_overrides: self.cli_overrides,
            loader_overrides: self.loader_overrides,
            cloud_requirements: self.cloud_requirements,
            feedback: self.feedback,
            config_warnings: self.config_warnings,
            session_source: self.session_source,
            enable_codex_api_key_env: self.enable_codex_api_key_env,
            initialize,
            channel_capacity: self.channel_capacity,
        }
    }
}

#[cfg(unix)]
#[derive(Clone)]
pub struct UnixDomainSocketClientStartArgs {
    /// Path to the app-server unix domain socket exposed by `codex remote start`.
    pub socket_path: PathBuf,
    /// Client name reported during initialize.
    pub client_name: String,
    /// Client version reported during initialize.
    pub client_version: String,
    /// Whether experimental APIs are requested at initialize time.
    pub experimental_api: bool,
    /// Notification methods this client opts out of receiving.
    pub opt_out_notification_methods: Vec<String>,
    /// Queue capacity for command/event channels (clamped to at least 1).
    pub channel_capacity: usize,
}

#[cfg(unix)]
impl UnixDomainSocketClientStartArgs {
    pub fn initialize_params(&self) -> InitializeParams {
        let capabilities = InitializeCapabilities {
            experimental_api: self.experimental_api,
            opt_out_notification_methods: if self.opt_out_notification_methods.is_empty() {
                None
            } else {
                Some(self.opt_out_notification_methods.clone())
            },
        };

        InitializeParams {
            client_info: ClientInfo {
                name: self.client_name.clone(),
                title: None,
                version: self.client_version.clone(),
            },
            capabilities: Some(capabilities),
        }
    }
}

/// Internal command sent from public facade methods to the worker task.
///
/// Each variant carries a oneshot sender so the caller can `await` the
/// result without holding a mutable reference to the client.
enum ClientCommand {
    Request {
        request: Box<ClientRequest>,
        response_tx: oneshot::Sender<IoResult<RequestResult>>,
    },
    Notify {
        notification: ClientNotification,
        response_tx: oneshot::Sender<IoResult<()>>,
    },
    ResolveServerRequest {
        request_id: RequestId,
        result: JsonRpcResult,
        response_tx: oneshot::Sender<IoResult<()>>,
    },
    RejectServerRequest {
        request_id: RequestId,
        error: JSONRPCErrorError,
        response_tx: oneshot::Sender<IoResult<()>>,
    },
    Shutdown {
        response_tx: oneshot::Sender<IoResult<()>>,
    },
}

/// Async facade over the in-process app-server runtime.
///
/// This type owns a worker task that bridges between:
/// - caller-facing async `mpsc` channels used by TUI/exec
/// - [`codex_app_server::in_process::InProcessClientHandle`], which speaks to
///   the embedded `MessageProcessor`
///
/// The facade intentionally preserves the server's request/notification/event
/// model instead of exposing direct core runtime handles. That keeps in-process
/// callers aligned with app-server behavior while still avoiding a process
/// boundary.
struct ClientFacade {
    command_tx: mpsc::Sender<ClientCommand>,
    event_rx: mpsc::Receiver<InProcessServerEvent>,
    worker_handle: tokio::task::JoinHandle<()>,
}

impl ClientFacade {
    async fn request(&self, request: ClientRequest) -> IoResult<RequestResult> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(ClientCommand::Request {
                request: Box::new(request),
                response_tx,
            })
            .await
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "app-server worker channel is closed"))?;
        response_rx
            .await
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "app-server request channel is closed"))?
    }

    async fn request_typed<T>(&self, request: ClientRequest) -> Result<T, TypedRequestError>
    where
        T: DeserializeOwned,
    {
        let method = request_method_name(&request);
        let response =
            self.request(request)
                .await
                .map_err(|source| TypedRequestError::Transport {
                    method: method.clone(),
                    source,
                })?;
        let result = response.map_err(|source| TypedRequestError::Server {
            method: method.clone(),
            source,
        })?;
        serde_json::from_value(result)
            .map_err(|source| TypedRequestError::Deserialize { method, source })
    }

    async fn notify(&self, notification: ClientNotification) -> IoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(ClientCommand::Notify {
                notification,
                response_tx,
            })
            .await
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "app-server worker channel is closed"))?;
        response_rx
            .await
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "app-server notify channel is closed"))?
    }

    async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(ClientCommand::ResolveServerRequest {
                request_id,
                result,
                response_tx,
            })
            .await
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "app-server worker channel is closed"))?;
        response_rx.await.map_err(|_| {
            IoError::new(ErrorKind::BrokenPipe, "app-server resolve channel is closed")
        })?
    }

    async fn reject_server_request(
        &self,
        request_id: RequestId,
        error: JSONRPCErrorError,
    ) -> IoResult<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(ClientCommand::RejectServerRequest {
                request_id,
                error,
                response_tx,
            })
            .await
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "app-server worker channel is closed"))?;
        response_rx.await.map_err(|_| {
            IoError::new(ErrorKind::BrokenPipe, "app-server reject channel is closed")
        })?
    }

    async fn next_event(&mut self) -> Option<InProcessServerEvent> {
        self.event_rx.recv().await
    }

    async fn shutdown(self) -> IoResult<()> {
        let Self {
            command_tx,
            event_rx,
            worker_handle,
        } = self;
        let mut worker_handle = worker_handle;
        drop(event_rx);
        let (response_tx, response_rx) = oneshot::channel();
        if command_tx
            .send(ClientCommand::Shutdown { response_tx })
            .await
            .is_ok()
            && let Ok(command_result) = timeout(SHUTDOWN_TIMEOUT, response_rx).await
        {
            command_result.map_err(|_| {
                IoError::new(ErrorKind::BrokenPipe, "app-server shutdown channel is closed")
            })??;
        }

        if let Err(_elapsed) = timeout(SHUTDOWN_TIMEOUT, &mut worker_handle).await {
            worker_handle.abort();
            let _ = worker_handle.await;
        }
        Ok(())
    }
}

pub struct InProcessAppServerClient {
    inner: ClientFacade,
}

impl InProcessAppServerClient {
    /// Starts the in-process runtime and facade worker task.
    ///
    /// The returned client is ready for requests and event consumption. If the
    /// internal event queue is saturated later, server requests are rejected
    /// with overload error instead of being silently dropped.
    pub async fn start(args: InProcessClientStartArgs) -> IoResult<Self> {
        let channel_capacity = args.channel_capacity.max(1);
        let mut handle =
            codex_app_server::in_process::start(args.into_runtime_start_args()).await?;
        let request_sender = handle.sender();
        let (command_tx, mut command_rx) = mpsc::channel::<ClientCommand>(channel_capacity);
        let (event_tx, event_rx) = mpsc::channel::<InProcessServerEvent>(channel_capacity);

        let worker_handle = tokio::spawn(async move {
            let mut event_stream_enabled = true;
            let mut skipped_events = 0usize;
            loop {
                tokio::select! {
                    command = command_rx.recv() => {
                        match command {
                            Some(ClientCommand::Request { request, response_tx }) => {
                                let request_sender = request_sender.clone();
                                // Request waits happen on a detached task so
                                // this loop can keep draining runtime events
                                // while the request is blocked on client input.
                                tokio::spawn(async move {
                                    let result = request_sender.request(*request).await;
                                    let _ = response_tx.send(result);
                                });
                            }
                            Some(ClientCommand::Notify {
                                notification,
                                response_tx,
                            }) => {
                                let result = request_sender.notify(notification);
                                let _ = response_tx.send(result);
                            }
                            Some(ClientCommand::ResolveServerRequest {
                                request_id,
                                result,
                                response_tx,
                            }) => {
                                let send_result =
                                    request_sender.respond_to_server_request(request_id, result);
                                let _ = response_tx.send(send_result);
                            }
                            Some(ClientCommand::RejectServerRequest {
                                request_id,
                                error,
                                response_tx,
                            }) => {
                                let send_result = request_sender.fail_server_request(request_id, error);
                                let _ = response_tx.send(send_result);
                            }
                            Some(ClientCommand::Shutdown { response_tx }) => {
                                let shutdown_result = handle.shutdown().await;
                                let _ = response_tx.send(shutdown_result);
                                break;
                            }
                            None => {
                                let _ = handle.shutdown().await;
                                break;
                            }
                        }
                    }
                    event = handle.next_event(), if event_stream_enabled => {
                        let Some(event) = event else {
                            break;
                        };

                        if skipped_events > 0 {
                            if event_requires_delivery(&event) {
                                // Surface lag before the terminal event, but
                                // do not let the lag marker itself cause us to
                                // drop the completion/abort notification that
                                // the caller is blocked on.
                                if event_tx
                                    .send(InProcessServerEvent::Lagged {
                                        skipped: skipped_events,
                                    })
                                    .await
                                    .is_err()
                                {
                                    event_stream_enabled = false;
                                    continue;
                                }
                                skipped_events = 0;
                            } else {
                                match event_tx.try_send(InProcessServerEvent::Lagged {
                                    skipped: skipped_events,
                                }) {
                                    Ok(()) => {
                                        skipped_events = 0;
                                    }
                                    Err(mpsc::error::TrySendError::Full(_)) => {
                                        skipped_events = skipped_events.saturating_add(1);
                                        warn!(
                                            "dropping in-process app-server event because consumer queue is full"
                                        );
                                        if let InProcessServerEvent::ServerRequest(request) = event {
                                            let _ = request_sender.fail_server_request(
                                                request.id().clone(),
                                                JSONRPCErrorError {
                                                    code: -32001,
                                                    message: "in-process app-server event queue is full".to_string(),
                                                    data: None,
                                                },
                                            );
                                        }
                                        continue;
                                    }
                                    Err(mpsc::error::TrySendError::Closed(_)) => {
                                        event_stream_enabled = false;
                                        continue;
                                    }
                                }
                            }
                        }

                        if event_requires_delivery(&event) {
                            // Block until the consumer catches up for
                            // terminal notifications; this preserves the
                            // completion signal even when the queue is
                            // otherwise saturated.
                            if event_tx.send(event).await.is_err() {
                                event_stream_enabled = false;
                            }
                            continue;
                        }

                        match event_tx.try_send(event) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(event)) => {
                                skipped_events = skipped_events.saturating_add(1);
                                warn!("dropping in-process app-server event because consumer queue is full");
                                if let InProcessServerEvent::ServerRequest(request) = event {
                                    let _ = request_sender.fail_server_request(
                                        request.id().clone(),
                                        JSONRPCErrorError {
                                            code: -32001,
                                            message: "in-process app-server event queue is full".to_string(),
                                            data: None,
                                        },
                                    );
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                event_stream_enabled = false;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            inner: ClientFacade {
                command_tx,
                event_rx,
                worker_handle,
            },
        })
    }

    /// Sends a typed client request and returns raw JSON-RPC result.
    ///
    /// Callers that expect a concrete response type should usually prefer
    /// [`request_typed`](Self::request_typed).
    pub async fn request(&self, request: ClientRequest) -> IoResult<RequestResult> {
        self.inner.request(request).await
    }

    /// Sends a typed client request and decodes the successful response body.
    ///
    /// This still deserializes from a JSON value produced by app-server's
    /// JSON-RPC result envelope. Because the caller chooses `T`, `Deserialize`
    /// failures indicate an internal request/response mismatch at the call site
    /// (or an in-process bug), not transport skew from an external client.
    pub async fn request_typed<T>(&self, request: ClientRequest) -> Result<T, TypedRequestError>
    where
        T: DeserializeOwned,
    {
        self.inner.request_typed(request).await
    }

    /// Sends a typed client notification.
    pub async fn notify(&self, notification: ClientNotification) -> IoResult<()> {
        self.inner.notify(notification).await
    }

    /// Resolves a pending server request.
    ///
    /// This should only be called with request IDs obtained from the current
    /// client's event stream.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()> {
        self.inner.resolve_server_request(request_id, result).await
    }

    /// Rejects a pending server request with JSON-RPC error payload.
    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        error: JSONRPCErrorError,
    ) -> IoResult<()> {
        self.inner.reject_server_request(request_id, error).await
    }

    /// Returns the next in-process event, or `None` when worker exits.
    ///
    /// Callers are expected to drain this stream promptly. If they fall behind,
    /// the worker emits [`InProcessServerEvent::Lagged`] markers and may reject
    /// pending server requests rather than letting approval flows hang.
    pub async fn next_event(&mut self) -> Option<InProcessServerEvent> {
        self.inner.next_event().await
    }

    /// Shuts down worker and in-process runtime with bounded wait.
    ///
    /// If graceful shutdown exceeds timeout, the worker task is aborted to
    /// avoid leaking background tasks in embedding callers.
    pub async fn shutdown(self) -> IoResult<()> {
        self.inner.shutdown().await
    }
}

#[cfg(unix)]
pub struct UnixDomainSocketAppServerClient {
    inner: ClientFacade,
}

#[cfg(unix)]
impl UnixDomainSocketAppServerClient {
    pub async fn start(args: UnixDomainSocketClientStartArgs) -> IoResult<Self> {
        let channel_capacity = args.channel_capacity.max(1);
        let stream = UnixStream::connect(&args.socket_path).await?;
        let (reader_half, mut writer_half) = stream.into_split();
        let mut lines = BufReader::new(reader_half).lines();
        let initialize_request_id = RequestId::Integer(0);
        let initialize_params = serde_json::to_value(args.initialize_params()).map_err(|err| {
            IoError::new(
                ErrorKind::InvalidData,
                format!("failed to encode app-server initialize params: {err}"),
            )
        })?;
        write_jsonrpc_message(
            &mut writer_half,
            JSONRPCMessage::Request(JSONRPCRequest {
                id: initialize_request_id.clone(),
                method: "initialize".to_string(),
                params: Some(initialize_params),
                trace: None,
            }),
        )
        .await?;

        let mut buffered_messages = VecDeque::new();
        loop {
            let Some(message) = read_jsonrpc_message(&mut lines).await? else {
                return Err(IoError::new(
                    ErrorKind::UnexpectedEof,
                    "unix domain socket app-server closed during initialize",
                ));
            };
            match message {
                JSONRPCMessage::Response(response) if response.id == initialize_request_id => break,
                JSONRPCMessage::Error(error) if error.id == initialize_request_id => {
                    return Err(IoError::new(
                        ErrorKind::PermissionDenied,
                        format!(
                            "unix domain socket app-server initialize failed: {}",
                            error.error.message
                        ),
                    ));
                }
                other => buffered_messages.push_back(other),
            }
        }

        let (command_tx, mut command_rx) = mpsc::channel::<ClientCommand>(channel_capacity);
        let (event_tx, event_rx) = mpsc::channel::<InProcessServerEvent>(channel_capacity);
        let worker_handle = tokio::spawn(async move {
            let mut pending_requests =
                HashMap::<RequestId, oneshot::Sender<IoResult<RequestResult>>>::new();
            let mut event_stream_enabled = true;
            let mut skipped_events = 0usize;
            loop {
                if let Some(message) = buffered_messages.pop_front() {
                    if let Err(err) = handle_socket_incoming_message(
                        &mut writer_half,
                        &event_tx,
                        &mut pending_requests,
                        &mut skipped_events,
                        &mut event_stream_enabled,
                        message,
                    )
                    .await
                    {
                        warn!("unix domain socket app-server message handling failed: {err}");
                        break;
                    }
                    continue;
                }

                tokio::select! {
                    command = command_rx.recv() => {
                        match command {
                            Some(ClientCommand::Request { request, response_tx }) => {
                                let request = match client_request_to_jsonrpc_request(*request) {
                                    Ok(request) => request,
                                    Err(err) => {
                                        let _ = response_tx.send(Err(err));
                                        continue;
                                    }
                                };
                                let request_id = request.id.clone();
                                if pending_requests.contains_key(&request_id) {
                                    let _ = response_tx.send(Err(IoError::new(
                                        ErrorKind::AlreadyExists,
                                        format!("duplicate app-server request id: {request_id:?}"),
                                    )));
                                    continue;
                                }
                                match write_jsonrpc_message(
                                    &mut writer_half,
                                    JSONRPCMessage::Request(request),
                                )
                                .await
                                {
                                    Ok(()) => {
                                        pending_requests.insert(request_id, response_tx);
                                    }
                                    Err(err) => {
                                        let _ = response_tx.send(Err(err));
                                        break;
                                    }
                                }
                            }
                            Some(ClientCommand::Notify {
                                notification,
                                response_tx,
                            }) => {
                                let notification =
                                    match client_notification_to_jsonrpc_notification(notification) {
                                        Ok(notification) => notification,
                                        Err(err) => {
                                            let _ = response_tx.send(Err(err));
                                            continue;
                                        }
                                    };
                                let send_result = write_jsonrpc_message(
                                    &mut writer_half,
                                    JSONRPCMessage::Notification(notification),
                                )
                                .await;
                                let should_break = send_result.is_err();
                                let _ = response_tx.send(send_result);
                                if should_break {
                                    break;
                                }
                            }
                            Some(ClientCommand::ResolveServerRequest {
                                request_id,
                                result,
                                response_tx,
                            }) => {
                                let send_result = write_jsonrpc_message(
                                    &mut writer_half,
                                    JSONRPCMessage::Response(codex_app_server_protocol::JSONRPCResponse {
                                        id: request_id,
                                        result,
                                    }),
                                )
                                .await;
                                let should_break = send_result.is_err();
                                let _ = response_tx.send(send_result);
                                if should_break {
                                    break;
                                }
                            }
                            Some(ClientCommand::RejectServerRequest {
                                request_id,
                                error,
                                response_tx,
                            }) => {
                                let send_result = write_jsonrpc_message(
                                    &mut writer_half,
                                    JSONRPCMessage::Error(codex_app_server_protocol::JSONRPCError {
                                        id: request_id,
                                        error,
                                    }),
                                )
                                .await;
                                let should_break = send_result.is_err();
                                let _ = response_tx.send(send_result);
                                if should_break {
                                    break;
                                }
                            }
                            Some(ClientCommand::Shutdown { response_tx }) => {
                                let _ = response_tx.send(Ok(()));
                                break;
                            }
                            None => break,
                        }
                    }
                    incoming = read_jsonrpc_message(&mut lines) => {
                        match incoming {
                            Ok(Some(message)) => {
                                if let Err(err) = handle_socket_incoming_message(
                                    &mut writer_half,
                                    &event_tx,
                                    &mut pending_requests,
                                    &mut skipped_events,
                                    &mut event_stream_enabled,
                                    message,
                                )
                                .await
                                {
                                    warn!("unix domain socket app-server message handling failed: {err}");
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(err) => {
                                warn!("unix domain socket app-server read failed: {err}");
                                break;
                            }
                        }
                    }
                }
            }

            for response_tx in pending_requests.into_values() {
                let _ = response_tx.send(Err(IoError::new(
                    ErrorKind::BrokenPipe,
                    "unix domain socket app-server connection is closed",
                )));
            }
        });

        Ok(Self {
            inner: ClientFacade {
                command_tx,
                event_rx,
                worker_handle,
            },
        })
    }

    pub async fn request(&self, request: ClientRequest) -> IoResult<RequestResult> {
        self.inner.request(request).await
    }

    pub async fn request_typed<T>(&self, request: ClientRequest) -> Result<T, TypedRequestError>
    where
        T: DeserializeOwned,
    {
        self.inner.request_typed(request).await
    }

    pub async fn notify(&self, notification: ClientNotification) -> IoResult<()> {
        self.inner.notify(notification).await
    }

    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> IoResult<()> {
        self.inner.resolve_server_request(request_id, result).await
    }

    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        error: JSONRPCErrorError,
    ) -> IoResult<()> {
        self.inner.reject_server_request(request_id, error).await
    }

    pub async fn next_event(&mut self) -> Option<InProcessServerEvent> {
        self.inner.next_event().await
    }

    pub async fn shutdown(self) -> IoResult<()> {
        self.inner.shutdown().await
    }
}

#[cfg(unix)]
fn client_request_to_jsonrpc_request(request: ClientRequest) -> IoResult<JSONRPCRequest> {
    serde_json::to_value(request)
        .map_err(|err| {
            IoError::new(
                ErrorKind::InvalidData,
                format!("failed to encode app-server request: {err}"),
            )
        })
        .and_then(|value| {
            serde_json::from_value(value).map_err(|err| {
                IoError::new(
                    ErrorKind::InvalidData,
                    format!("failed to shape app-server request as JSON-RPC: {err}"),
                )
            })
        })
}

#[cfg(unix)]
fn client_notification_to_jsonrpc_notification(
    notification: ClientNotification,
) -> IoResult<JSONRPCNotification> {
    serde_json::to_value(notification)
        .map_err(|err| {
            IoError::new(
                ErrorKind::InvalidData,
                format!("failed to encode app-server notification: {err}"),
            )
        })
        .and_then(|value| {
            serde_json::from_value(value).map_err(|err| {
                IoError::new(
                    ErrorKind::InvalidData,
                    format!("failed to shape app-server notification as JSON-RPC: {err}"),
                )
            })
        })
}

#[cfg(unix)]
async fn read_jsonrpc_message<R>(lines: &mut tokio::io::Lines<R>) -> IoResult<Option<JSONRPCMessage>>
where
    R: AsyncBufRead + Unpin,
{
    let Some(line) = lines.next_line().await? else {
        return Ok(None);
    };
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|err| {
            IoError::new(
                ErrorKind::InvalidData,
                format!("failed to decode JSON-RPC message: {err}"),
            )
        })
}

#[cfg(unix)]
async fn write_jsonrpc_message<W>(writer: &mut W, message: JSONRPCMessage) -> IoResult<()>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_string(&message).map_err(|err| {
        IoError::new(
            ErrorKind::InvalidData,
            format!("failed to encode JSON-RPC message: {err}"),
        )
    })?;
    encoded.push('\n');
    writer.write_all(encoded.as_bytes()).await
}

#[cfg(unix)]
async fn reject_socket_server_request<W>(
    writer: &mut W,
    request_id: RequestId,
    message: &str,
) -> IoResult<()>
where
    W: AsyncWrite + Unpin,
{
    write_jsonrpc_message(
        writer,
        JSONRPCMessage::Error(codex_app_server_protocol::JSONRPCError {
            id: request_id,
            error: JSONRPCErrorError {
                code: -32001,
                message: message.to_string(),
                data: None,
            },
        }),
    )
    .await
}

#[cfg(unix)]
async fn queue_socket_event<W>(
    writer: &mut W,
    event_tx: &mpsc::Sender<InProcessServerEvent>,
    event: InProcessServerEvent,
    skipped_events: &mut usize,
    event_stream_enabled: &mut bool,
) -> IoResult<()>
where
    W: AsyncWrite + Unpin,
{
    const SOCKET_QUEUE_FULL_MESSAGE: &str = "socket app-server event queue is full";

    if !*event_stream_enabled {
        if let InProcessServerEvent::ServerRequest(request) = event {
            reject_socket_server_request(writer, request.id().clone(), SOCKET_QUEUE_FULL_MESSAGE)
                .await?;
        }
        return Ok(());
    }

    if *skipped_events > 0 {
        if event_requires_delivery(&event) {
            if event_tx
                .send(InProcessServerEvent::Lagged {
                    skipped: *skipped_events,
                })
                .await
                .is_err()
            {
                *event_stream_enabled = false;
                return Ok(());
            }
            *skipped_events = 0;
        } else {
            match event_tx.try_send(InProcessServerEvent::Lagged {
                skipped: *skipped_events,
            }) {
                Ok(()) => {
                    *skipped_events = 0;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    *skipped_events = skipped_events.saturating_add(1);
                    warn!("dropping socket app-server event because consumer queue is full");
                    if let InProcessServerEvent::ServerRequest(request) = event {
                        reject_socket_server_request(
                            writer,
                            request.id().clone(),
                            SOCKET_QUEUE_FULL_MESSAGE,
                        )
                        .await?;
                    }
                    return Ok(());
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    *event_stream_enabled = false;
                    return Ok(());
                }
            }
        }
    }

    if event_requires_delivery(&event) {
        if event_tx.send(event).await.is_err() {
            *event_stream_enabled = false;
        }
        return Ok(());
    }

    match event_tx.try_send(event) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(event)) => {
            *skipped_events = skipped_events.saturating_add(1);
            warn!("dropping socket app-server event because consumer queue is full");
            if let InProcessServerEvent::ServerRequest(request) = event {
                reject_socket_server_request(writer, request.id().clone(), SOCKET_QUEUE_FULL_MESSAGE)
                    .await?;
            }
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            *event_stream_enabled = false;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn handle_socket_incoming_message<W>(
    writer: &mut W,
    event_tx: &mpsc::Sender<InProcessServerEvent>,
    pending_requests: &mut HashMap<RequestId, oneshot::Sender<IoResult<RequestResult>>>,
    skipped_events: &mut usize,
    event_stream_enabled: &mut bool,
    message: JSONRPCMessage,
) -> IoResult<()>
where
    W: AsyncWrite + Unpin,
{
    match message {
        JSONRPCMessage::Response(response) => {
            if let Some(response_tx) = pending_requests.remove(&response.id) {
                let _ = response_tx.send(Ok(Ok(response.result)));
            } else {
                warn!("dropping unexpected socket app-server response for {:?}", response.id);
            }
        }
        JSONRPCMessage::Error(error) => {
            if let Some(response_tx) = pending_requests.remove(&error.id) {
                let _ = response_tx.send(Ok(Err(error.error)));
            } else {
                warn!("dropping unexpected socket app-server error for {:?}", error.id);
            }
        }
        JSONRPCMessage::Notification(notification) => {
            let event = match ServerNotification::try_from(notification.clone()) {
                Ok(notification) => InProcessServerEvent::ServerNotification(notification),
                Err(_) => InProcessServerEvent::LegacyNotification(notification),
            };
            queue_socket_event(
                writer,
                event_tx,
                event,
                skipped_events,
                event_stream_enabled,
            )
            .await?;
        }
        JSONRPCMessage::Request(request) => match ServerRequest::try_from(request) {
            Ok(request) => {
                queue_socket_event(
                    writer,
                    event_tx,
                    InProcessServerEvent::ServerRequest(request),
                    skipped_events,
                    event_stream_enabled,
                )
                .await?;
            }
            Err(err) => {
                warn!("failed to decode socket app-server server request: {err}");
            }
        },
    }
    Ok(())
}

/// Extracts the JSON-RPC method name for diagnostics without extending the
/// protocol crate with in-process-only helpers.
fn request_method_name(request: &ClientRequest) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use codex_app_server_protocol::CommandExecutionApprovalDecision;
    #[cfg(unix)]
    use codex_app_server_protocol::JSONRPCMessage;
    #[cfg(unix)]
    use codex_app_server_protocol::JSONRPCNotification;
    #[cfg(unix)]
    use codex_app_server_protocol::JSONRPCRequest;
    #[cfg(unix)]
    use codex_app_server_protocol::JSONRPCResponse;
    use codex_app_server_protocol::ConfigRequirementsReadResponse;
    #[cfg(unix)]
    use codex_app_server_protocol::ServerNotification;
    use codex_app_server_protocol::SessionSource as ApiSessionSource;
    #[cfg(unix)]
    use codex_app_server_protocol::SkillsChangedNotification;
    use codex_app_server_protocol::ThreadStartParams;
    use codex_app_server_protocol::ThreadStartResponse;
    use codex_core::config::ConfigBuilder;
    use pretty_assertions::assert_eq;
    #[cfg(unix)]
    use serde_json::json;
    #[cfg(unix)]
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::time::SystemTime;
    #[cfg(unix)]
    use std::time::UNIX_EPOCH;
    #[cfg(unix)]
    use tokio::io::AsyncBufReadExt;
    #[cfg(unix)]
    use tokio::io::AsyncWriteExt;
    #[cfg(unix)]
    use tokio::io::BufReader;
    #[cfg(unix)]
    use tokio::net::UnixListener;
    use tokio::time::Duration;
    use tokio::time::timeout;

    async fn build_test_config() -> Config {
        match ConfigBuilder::default().build().await {
            Ok(config) => config,
            Err(_) => Config::load_default_with_cli_overrides(Vec::new())
                .expect("default config should load"),
        }
    }

    async fn start_test_client_with_capacity(
        session_source: SessionSource,
        channel_capacity: usize,
    ) -> InProcessAppServerClient {
        InProcessAppServerClient::start(InProcessClientStartArgs {
            arg0_paths: Arg0DispatchPaths::default(),
            config: Arc::new(build_test_config().await),
            cli_overrides: Vec::new(),
            loader_overrides: LoaderOverrides::default(),
            cloud_requirements: CloudRequirementsLoader::default(),
            feedback: CodexFeedback::new(),
            config_warnings: Vec::new(),
            session_source,
            enable_codex_api_key_env: false,
            client_name: "codex-app-server-client-test".to_string(),
            client_version: "0.0.0-test".to_string(),
            experimental_api: true,
            opt_out_notification_methods: Vec::new(),
            channel_capacity,
        })
        .await
        .expect("in-process app-server client should start")
    }

    async fn start_test_client(session_source: SessionSource) -> InProcessAppServerClient {
        start_test_client_with_capacity(session_source, DEFAULT_IN_PROCESS_CHANNEL_CAPACITY).await
    }

    #[cfg(unix)]
    fn unique_socket_path(name: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        PathBuf::from(format!(
            "/tmp/codex-asc-{name}-{}-{timestamp}.sock",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    async fn read_jsonrpc_message(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    ) -> JSONRPCMessage {
        let line = timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("socket frame should arrive before timeout")
            .expect("socket frame should be readable")
            .expect("socket should remain open");
        serde_json::from_str(&line).expect("socket frame should decode as JSON-RPC")
    }

    #[cfg(unix)]
    async fn write_jsonrpc_message(
        writer: &mut tokio::net::unix::OwnedWriteHalf,
        message: JSONRPCMessage,
    ) {
        let mut encoded =
            serde_json::to_string(&message).expect("JSON-RPC message should encode cleanly");
        encoded.push('\n');
        writer
            .write_all(encoded.as_bytes())
            .await
            .expect("socket frame should be writable");
    }

    #[tokio::test]
    async fn typed_request_roundtrip_works() {
        let client = start_test_client(SessionSource::Exec).await;
        let _response: ConfigRequirementsReadResponse = client
            .request_typed(ClientRequest::ConfigRequirementsRead {
                request_id: RequestId::Integer(1),
                params: None,
            })
            .await
            .expect("typed request should succeed");
        client.shutdown().await.expect("shutdown should complete");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_domain_socket_typed_request_roundtrip_works() {
        let socket_path = unique_socket_path("typed-request");
        let listener = UnixListener::bind(&socket_path)
            .expect("fake unix domain socket server should bind");
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener
                .accept()
                .await
                .expect("fake server should accept client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            let initialize = read_jsonrpc_message(&mut lines).await;
            let JSONRPCMessage::Request(initialize) = initialize else {
                panic!("expected initialize request");
            };
            assert_eq!(initialize.method, "initialize");
            write_jsonrpc_message(
                &mut writer,
                JSONRPCMessage::Response(JSONRPCResponse {
                    id: initialize.id,
                    result: json!({}),
                }),
            )
            .await;

            let request = read_jsonrpc_message(&mut lines).await;
            let JSONRPCMessage::Request(request) = request else {
                panic!("expected typed request");
            };
            assert_eq!(request.method, "configRequirements/read");
            write_jsonrpc_message(
                &mut writer,
                JSONRPCMessage::Response(JSONRPCResponse {
                    id: request.id,
                    result: json!({ "ok": true }),
                }),
            )
            .await;
        });

        let client = UnixDomainSocketAppServerClient::start(UnixDomainSocketClientStartArgs {
            socket_path: socket_path.clone(),
            client_name: "codex-app-server-client-test".to_string(),
            client_version: "0.0.0-test".to_string(),
            experimental_api: true,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        })
        .await
        .expect("socket app-server client should start");

        let response: serde_json::Value = client
            .request_typed(ClientRequest::ConfigRequirementsRead {
                request_id: RequestId::Integer(11),
                params: None,
            })
            .await
            .expect("typed request should succeed over socket");
        assert_eq!(response, json!({ "ok": true }));

        client.shutdown().await.expect("shutdown should complete");
        server_task
            .await
            .expect("fake unix domain socket server should finish cleanly");
        let _ = std::fs::remove_file(socket_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_domain_socket_surfaces_notifications_and_server_request_responses() {
        let socket_path = unique_socket_path("events");
        let listener =
            UnixListener::bind(&socket_path).expect("fake unix domain socket server should bind");
        let server_task = tokio::spawn(async move {
            let (stream, _) = listener
                .accept()
                .await
                .expect("fake server should accept client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();

            let initialize = read_jsonrpc_message(&mut lines).await;
            let JSONRPCMessage::Request(initialize) = initialize else {
                panic!("expected initialize request");
            };
            write_jsonrpc_message(
                &mut writer,
                JSONRPCMessage::Response(JSONRPCResponse {
                    id: initialize.id,
                    result: json!({}),
                }),
            )
            .await;

            write_jsonrpc_message(
                &mut writer,
                JSONRPCMessage::Notification(JSONRPCNotification {
                    method: "skills/changed".to_string(),
                    params: Some(json!({})),
                }),
            )
            .await;
            write_jsonrpc_message(
                &mut writer,
                JSONRPCMessage::Notification(JSONRPCNotification {
                    method: "codex/event/test".to_string(),
                    params: Some(json!({ "message": "hello" })),
                }),
            )
            .await;
            write_jsonrpc_message(
                &mut writer,
                JSONRPCMessage::Request(JSONRPCRequest {
                    id: RequestId::Integer(42),
                    method: "item/commandExecution/requestApproval".to_string(),
                    params: Some(json!({
                        "threadId": "thread-1",
                        "turnId": "turn-1",
                        "itemId": "item-1",
                        "command": "pwd",
                    })),
                    trace: None,
                }),
            )
            .await;

            let response = read_jsonrpc_message(&mut lines).await;
            let JSONRPCMessage::Response(response) = response else {
                panic!("expected server request response");
            };
            assert_eq!(response.id, RequestId::Integer(42));
            assert_eq!(response.result, json!({ "decision": "accept" }));
        });

        let mut client = UnixDomainSocketAppServerClient::start(UnixDomainSocketClientStartArgs {
            socket_path: socket_path.clone(),
            client_name: "codex-app-server-client-test".to_string(),
            client_version: "0.0.0-test".to_string(),
            experimental_api: true,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        })
        .await
        .expect("socket app-server client should start");

        let notification = timeout(Duration::from_secs(5), client.next_event())
            .await
            .expect("typed notification should arrive before timeout")
            .expect("typed notification should exist");
        assert!(matches!(
            notification,
            InProcessServerEvent::ServerNotification(ServerNotification::SkillsChanged(
                SkillsChangedNotification {}
            ))
        ));

        let legacy_notification = timeout(Duration::from_secs(5), client.next_event())
            .await
            .expect("legacy notification should arrive before timeout")
            .expect("legacy notification should exist");
        assert!(matches!(
            legacy_notification,
            InProcessServerEvent::LegacyNotification(JSONRPCNotification { method, .. })
                if method == "codex/event/test"
        ));

        let server_request = timeout(Duration::from_secs(5), client.next_event())
            .await
            .expect("server request should arrive before timeout")
            .expect("server request should exist");
        let request_id = match server_request {
            InProcessServerEvent::ServerRequest(
                codex_app_server_protocol::ServerRequest::CommandExecutionRequestApproval {
                    request_id,
                    params,
                },
            ) => {
                assert_eq!(params.command, Some("pwd".to_string()));
                request_id
            }
            other => panic!("expected command approval request, got {other:?}"),
        };

        client
            .resolve_server_request(
                request_id,
                json!({ "decision": CommandExecutionApprovalDecision::Accept }),
            )
            .await
            .expect("server request response should succeed");
        client.shutdown().await.expect("shutdown should complete");

        server_task
            .await
            .expect("fake unix domain socket server should finish cleanly");
        let _ = std::fs::remove_file(socket_path);
    }

    #[tokio::test]
    async fn typed_request_reports_json_rpc_errors() {
        let client = start_test_client(SessionSource::Exec).await;
        let err = client
            .request_typed::<ConfigRequirementsReadResponse>(ClientRequest::ThreadRead {
                request_id: RequestId::Integer(99),
                params: codex_app_server_protocol::ThreadReadParams {
                    thread_id: "missing-thread".to_string(),
                    include_turns: false,
                },
            })
            .await
            .expect_err("missing thread should return a JSON-RPC error");
        assert!(
            err.to_string().starts_with("thread/read failed:"),
            "expected method-qualified JSON-RPC failure message"
        );
        client.shutdown().await.expect("shutdown should complete");
    }

    #[tokio::test]
    async fn caller_provided_session_source_is_applied() {
        for (session_source, expected_source) in [
            (SessionSource::Exec, ApiSessionSource::Exec),
            (SessionSource::Cli, ApiSessionSource::Cli),
        ] {
            let client = start_test_client(session_source).await;
            let parsed: ThreadStartResponse = client
                .request_typed(ClientRequest::ThreadStart {
                    request_id: RequestId::Integer(2),
                    params: ThreadStartParams {
                        ephemeral: Some(true),
                        ..ThreadStartParams::default()
                    },
                })
                .await
                .expect("thread/start should succeed");
            assert_eq!(parsed.thread.source, expected_source);
            client.shutdown().await.expect("shutdown should complete");
        }
    }

    #[tokio::test]
    async fn tiny_channel_capacity_still_supports_request_roundtrip() {
        let client = start_test_client_with_capacity(SessionSource::Exec, 1).await;
        let _response: ConfigRequirementsReadResponse = client
            .request_typed(ClientRequest::ConfigRequirementsRead {
                request_id: RequestId::Integer(1),
                params: None,
            })
            .await
            .expect("typed request should succeed");
        client.shutdown().await.expect("shutdown should complete");
    }

    #[test]
    fn typed_request_error_exposes_sources() {
        let transport = TypedRequestError::Transport {
            method: "config/read".to_string(),
            source: IoError::new(ErrorKind::BrokenPipe, "closed"),
        };
        assert_eq!(std::error::Error::source(&transport).is_some(), true);

        let server = TypedRequestError::Server {
            method: "thread/read".to_string(),
            source: JSONRPCErrorError {
                code: -32603,
                data: None,
                message: "internal".to_string(),
            },
        };
        assert_eq!(std::error::Error::source(&server).is_some(), false);

        let deserialize = TypedRequestError::Deserialize {
            method: "thread/start".to_string(),
            source: serde_json::from_str::<u32>("\"nope\"")
                .expect_err("invalid integer should return deserialize error"),
        };
        assert_eq!(std::error::Error::source(&deserialize).is_some(), true);
    }

    #[tokio::test]
    async fn next_event_surfaces_lagged_markers() {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        let worker_handle = tokio::spawn(async {});
        event_tx
            .send(InProcessServerEvent::Lagged { skipped: 3 })
            .await
            .expect("lagged marker should enqueue");
        drop(event_tx);

        let mut client = InProcessAppServerClient {
            inner: ClientFacade {
                command_tx,
                event_rx,
                worker_handle,
            },
        };

        let event = timeout(Duration::from_secs(2), client.next_event())
            .await
            .expect("lagged marker should arrive before timeout");
        assert!(matches!(
            event,
            Some(InProcessServerEvent::Lagged { skipped: 3 })
        ));

        client.shutdown().await.expect("shutdown should complete");
    }

    #[test]
    fn event_requires_delivery_marks_terminal_events() {
        assert!(event_requires_delivery(
            &InProcessServerEvent::ServerNotification(
                codex_app_server_protocol::ServerNotification::TurnCompleted(
                    codex_app_server_protocol::TurnCompletedNotification {
                        thread_id: "thread".to_string(),
                        turn: codex_app_server_protocol::Turn {
                            id: "turn".to_string(),
                            items: Vec::new(),
                            status: codex_app_server_protocol::TurnStatus::Completed,
                            error: None,
                        },
                    }
                )
            )
        ));
        assert!(event_requires_delivery(
            &InProcessServerEvent::LegacyNotification(
                codex_app_server_protocol::JSONRPCNotification {
                    method: "codex/event/turn_aborted".to_string(),
                    params: None,
                }
            )
        ));
        assert!(!event_requires_delivery(&InProcessServerEvent::Lagged {
            skipped: 1
        }));
    }
}
