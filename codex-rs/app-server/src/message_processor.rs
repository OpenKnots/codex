use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::codex_message_processor::CodexMessageProcessor;
use crate::codex_message_processor::CodexMessageProcessorArgs;
use crate::config_api::ConfigApi;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use crate::external_agent_config_api::ExternalAgentConfigApi;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::transport::AppServerTransport;
use async_trait::async_trait;
use codex_app_server_protocol::ChatgptAuthTokensRefreshParams;
use codex_app_server_protocol::ChatgptAuthTokensRefreshReason;
use codex_app_server_protocol::ChatgptAuthTokensRefreshResponse;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientNotification;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigReadParams;
use codex_app_server_protocol::ConfigValueWriteParams;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::ExperimentalApi;
use codex_app_server_protocol::ExternalAgentConfigDetectParams;
use codex_app_server_protocol::ExternalAgentConfigImportParams;
use codex_app_server_protocol::InitializeResponse;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCRequest;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequestPayload;
use codex_app_server_protocol::experimental_required_message;
use codex_arg0::Arg0DispatchPaths;
use codex_core::AuthManager;
use codex_core::ThreadManager;
use codex_core::auth::ExternalAuthRefreshContext;
use codex_core::auth::ExternalAuthRefreshReason;
use codex_core::auth::ExternalAuthRefresher;
use codex_core::auth::ExternalAuthTokens;
use codex_core::config::Config;
use codex_core::config_loader::CloudRequirementsLoader;
use codex_core::config_loader::LoaderOverrides;
use codex_core::default_client::SetOriginatorError;
use codex_core::default_client::USER_AGENT_SUFFIX;
use codex_core::default_client::get_codex_user_agent;
use codex_core::default_client::set_default_client_residency_requirement;
use codex_core::default_client::set_default_originator;
use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_feedback::CodexFeedback;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_state::log_db::LogDbLayer;
use futures::FutureExt;
use opentelemetry::context::FutureExt as OtelFutureExt;
use tokio::sync::broadcast;
use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::timeout;
use toml::Value as TomlValue;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

const EXTERNAL_AUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ExternalAuthRefreshBridge {
    outgoing: Arc<OutgoingMessageSender>,
}

impl ExternalAuthRefreshBridge {
    fn map_reason(reason: ExternalAuthRefreshReason) -> ChatgptAuthTokensRefreshReason {
        match reason {
            ExternalAuthRefreshReason::Unauthorized => ChatgptAuthTokensRefreshReason::Unauthorized,
        }
    }
}

#[async_trait]
impl ExternalAuthRefresher for ExternalAuthRefreshBridge {
    async fn refresh(
        &self,
        context: ExternalAuthRefreshContext,
    ) -> std::io::Result<ExternalAuthTokens> {
        let params = ChatgptAuthTokensRefreshParams {
            reason: Self::map_reason(context.reason),
            previous_account_id: context.previous_account_id,
        };

        let (request_id, rx) = self
            .outgoing
            .send_request(ServerRequestPayload::ChatgptAuthTokensRefresh(params))
            .await;

        let result = match timeout(EXTERNAL_AUTH_REFRESH_TIMEOUT, rx).await {
            Ok(result) => {
                // Two failure scenarios:
                // 1) `oneshot::Receiver` failed (sender dropped) => request canceled/channel closed.
                // 2) client answered with JSON-RPC error payload => propagate code/message.
                let result = result.map_err(|err| {
                    std::io::Error::other(format!("auth refresh request canceled: {err}"))
                })?;
                result.map_err(|err| {
                    std::io::Error::other(format!(
                        "auth refresh request failed: code={} message={}",
                        err.code, err.message
                    ))
                })?
            }
            Err(_) => {
                let _canceled = self.outgoing.cancel_request(&request_id).await;
                return Err(std::io::Error::other(format!(
                    "auth refresh request timed out after {}s",
                    EXTERNAL_AUTH_REFRESH_TIMEOUT.as_secs()
                )));
            }
        };

        let response: ChatgptAuthTokensRefreshResponse =
            serde_json::from_value(result).map_err(std::io::Error::other)?;

        Ok(ExternalAuthTokens {
            access_token: response.access_token,
            chatgpt_account_id: response.chatgpt_account_id,
            chatgpt_plan_type: response.chatgpt_plan_type,
        })
    }
}

pub(crate) struct MessageProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    codex_message_processor: CodexMessageProcessor,
    config_api: ConfigApi,
    external_agent_config_api: ExternalAgentConfigApi,
    config: Arc<Config>,
    config_warnings: Arc<Vec<ConfigWarningNotification>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConnectionSessionState {
    pub(crate) initialized: bool,
    pub(crate) experimental_api_enabled: bool,
    pub(crate) opted_out_notification_methods: HashSet<String>,
    pub(crate) app_server_client_name: Option<String>,
    pub(crate) client_version: Option<String>,
}

pub(crate) struct MessageProcessorArgs {
    pub(crate) outgoing: Arc<OutgoingMessageSender>,
    pub(crate) arg0_paths: Arg0DispatchPaths,
    pub(crate) config: Arc<Config>,
    pub(crate) cli_overrides: Vec<(String, TomlValue)>,
    pub(crate) loader_overrides: LoaderOverrides,
    pub(crate) cloud_requirements: CloudRequirementsLoader,
    pub(crate) feedback: CodexFeedback,
    pub(crate) log_db: Option<LogDbLayer>,
    pub(crate) config_warnings: Vec<ConfigWarningNotification>,
    pub(crate) session_source: SessionSource,
    pub(crate) enable_codex_api_key_env: bool,
}

impl MessageProcessor {
    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(args: MessageProcessorArgs) -> Self {
        let MessageProcessorArgs {
            outgoing,
            arg0_paths,
            config,
            cli_overrides,
            loader_overrides,
            cloud_requirements,
            feedback,
            log_db,
            config_warnings,
            session_source,
            enable_codex_api_key_env,
        } = args;
        let auth_manager = AuthManager::shared(
            config.codex_home.clone(),
            enable_codex_api_key_env,
            config.cli_auth_credentials_store_mode,
        );
        auth_manager.set_forced_chatgpt_workspace_id(config.forced_chatgpt_workspace_id.clone());
        auth_manager.set_external_auth_refresher(Arc::new(ExternalAuthRefreshBridge {
            outgoing: outgoing.clone(),
        }));
        let thread_manager = Arc::new(ThreadManager::new(
            config.codex_home.clone(),
            auth_manager.clone(),
            session_source,
            config.model_catalog.clone(),
            CollaborationModesConfig {
                default_mode_request_user_input: config
                    .features
                    .enabled(codex_core::features::Feature::DefaultModeRequestUserInput),
            },
        ));
        // TODO(xl): Move into PluginManager once this no longer depends on config feature gating.
        thread_manager
            .plugins_manager()
            .maybe_start_curated_repo_sync_for_config(&config);
        let cloud_requirements = Arc::new(RwLock::new(cloud_requirements));
        let codex_message_processor = CodexMessageProcessor::new(CodexMessageProcessorArgs {
            auth_manager,
            thread_manager: Arc::clone(&thread_manager),
            outgoing: outgoing.clone(),
            arg0_paths,
            config: Arc::clone(&config),
            cli_overrides: cli_overrides.clone(),
            cloud_requirements: cloud_requirements.clone(),
            feedback,
            log_db,
        });
        let config_api = ConfigApi::new(
            config.codex_home.clone(),
            cli_overrides,
            loader_overrides,
            cloud_requirements,
            thread_manager,
        );
        let external_agent_config_api = ExternalAgentConfigApi::new(config.codex_home.clone());

        Self {
            outgoing,
            codex_message_processor,
            config_api,
            external_agent_config_api,
            config,
            config_warnings: Arc::new(config_warnings),
        }
    }

    pub(crate) async fn process_request(
        &mut self,
        connection_id: ConnectionId,
        request: JSONRPCRequest,
        transport: AppServerTransport,
        session: &mut ConnectionSessionState,
    ) {
        let request_span =
            crate::app_server_tracing::request_span(&request, transport, connection_id, session);
        let request_method = request.method.as_str();
        tracing::trace!(
            ?connection_id,
            request_id = ?request.id,
            "app-server request: {request_method}"
        );
        let request_id = ConnectionRequestId {
            connection_id,
            request_id: request.id.clone(),
        };
        let request_json = match serde_json::to_value(&request) {
            Ok(request_json) => request_json,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("Invalid request: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let codex_request = match serde_json::from_value::<ClientRequest>(request_json) {
            Ok(codex_request) => codex_request,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("Invalid request: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        // Websocket callers finalize outbound readiness in lib.rs after mirroring
        // session state into outbound state and sending initialize notifications to
        // this specific connection. Passing `None` avoids marking the connection
        // ready too early from inside the shared request handler.
        if matches!(codex_request, ClientRequest::ThreadStart { .. }) {
            self.handle_client_request(connection_id, request_id, codex_request, session, None)
                .with_context(request_span.context())
                .await;
        } else {
            async {
                self.handle_client_request(connection_id, request_id, codex_request, session, None)
                    .await;
            }
            .instrument(request_span)
            .await;
        }
    }

    /// Handles a typed request path used by in-process embedders.
    ///
    /// This bypasses JSON request deserialization but keeps identical request
    /// semantics by delegating to `handle_client_request`.
    pub(crate) async fn process_client_request(
        &mut self,
        connection_id: ConnectionId,
        request: ClientRequest,
        session: &mut ConnectionSessionState,
        outbound_initialized: &AtomicBool,
    ) {
        let request_span =
            crate::app_server_tracing::typed_request_span(&request, connection_id, session);
        let request_id = ConnectionRequestId {
            connection_id,
            request_id: request.id().clone(),
        };
        tracing::trace!(
            ?connection_id,
            request_id = ?request_id.request_id,
            "app-server typed request"
        );
        if matches!(request, ClientRequest::ThreadStart { .. }) {
            self.handle_client_request(
                connection_id,
                request_id,
                request,
                session,
                Some(outbound_initialized),
            )
            .with_context(request_span.context())
            .await;
        } else {
            async {
                // In-process clients do not have the websocket transport loop that performs
                // post-initialize bookkeeping, so they still finalize outbound readiness in
                // the shared request handler.
                self.handle_client_request(
                    connection_id,
                    request_id,
                    request,
                    session,
                    Some(outbound_initialized),
                )
                .await;
            }
            .instrument(request_span)
            .await;
        }
    }

    pub(crate) async fn process_notification(&self, notification: JSONRPCNotification) {
        // Currently, we do not expect to receive any notifications from the
        // client, so we just log them.
        tracing::info!("<- notification: {:?}", notification);
    }

    /// Handles typed notifications from in-process clients.
    pub(crate) async fn process_client_notification(&self, notification: ClientNotification) {
        // Currently, we do not expect to receive any typed notifications from
        // in-process clients, so we just log them.
        tracing::info!("<- typed notification: {:?}", notification);
    }

    pub(crate) fn thread_created_receiver(&self) -> broadcast::Receiver<ThreadId> {
        self.codex_message_processor.thread_created_receiver()
    }

    pub(crate) async fn send_initialize_notifications_to_connection(
        &self,
        connection_id: ConnectionId,
    ) {
        for notification in self.config_warnings.iter().cloned() {
            self.outgoing
                .send_server_notification_to_connections(
                    &[connection_id],
                    ServerNotification::ConfigWarning(notification),
                )
                .await;
        }
    }

    pub(crate) async fn connection_initialized(&self, connection_id: ConnectionId) {
        self.codex_message_processor
            .connection_initialized(connection_id)
            .await;
    }

    pub(crate) async fn send_initialize_notifications(&self) {
        for notification in self.config_warnings.iter().cloned() {
            self.outgoing
                .send_server_notification(ServerNotification::ConfigWarning(notification))
                .await;
        }
    }

    pub(crate) async fn try_attach_thread_listener(
        &mut self,
        thread_id: ThreadId,
        connection_ids: Vec<ConnectionId>,
    ) {
        self.codex_message_processor
            .try_attach_thread_listener(thread_id, connection_ids)
            .await;
    }

    pub(crate) async fn connection_closed(&mut self, connection_id: ConnectionId) {
        self.codex_message_processor
            .connection_closed(connection_id)
            .await;
    }

    pub(crate) fn subscribe_running_assistant_turn_count(&self) -> watch::Receiver<usize> {
        self.codex_message_processor
            .subscribe_running_assistant_turn_count()
    }

    /// Handle a standalone JSON-RPC response originating from the peer.
    pub(crate) async fn process_response(&mut self, response: JSONRPCResponse) {
        tracing::info!("<- response: {:?}", response);
        let JSONRPCResponse { id, result, .. } = response;
        self.outgoing.notify_client_response(id, result).await
    }

    /// Handle an error object received from the peer.
    pub(crate) async fn process_error(&mut self, err: JSONRPCError) {
        tracing::error!("<- error: {:?}", err);
        self.outgoing.notify_client_error(err.id, err.error).await;
    }

    async fn handle_client_request(
        &mut self,
        connection_id: ConnectionId,
        request_id: ConnectionRequestId,
        codex_request: ClientRequest,
        session: &mut ConnectionSessionState,
        // `Some(...)` means the caller wants initialize to immediately mark the
        // connection outbound-ready. Websocket JSON-RPC calls pass `None` so
        // lib.rs can deliver connection-scoped initialize notifications first.
        outbound_initialized: Option<&AtomicBool>,
    ) {
        match codex_request {
            // Handle Initialize internally so CodexMessageProcessor does not have to concern
            // itself with the `initialized` bool.
            ClientRequest::Initialize { request_id, params } => {
                let request_id = ConnectionRequestId {
                    connection_id,
                    request_id,
                };
                if session.initialized {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "Already initialized".to_string(),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }

                // TODO(maxj): Revisit capability scoping for `experimental_api_enabled`.
                // Current behavior is per-connection. Reviewer feedback notes this can
                // create odd cross-client behavior (for example dynamic tool calls on a
                // shared thread when another connected client did not opt into
                // experimental API). Proposed direction is instance-global first-write-wins
                // with initialize-time mismatch rejection.
                let (experimental_api_enabled, opt_out_notification_methods) =
                    match params.capabilities {
                        Some(capabilities) => (
                            capabilities.experimental_api,
                            capabilities
                                .opt_out_notification_methods
                                .unwrap_or_default(),
                        ),
                        None => (false, Vec::new()),
                    };
                session.experimental_api_enabled = experimental_api_enabled;
                session.opted_out_notification_methods =
                    opt_out_notification_methods.into_iter().collect();
                let ClientInfo {
                    name,
                    title: _title,
                    version,
                } = params.client_info;
                session.app_server_client_name = Some(name.clone());
                session.client_version = Some(version.clone());
                if let Err(error) = set_default_originator(name.clone()) {
                    match error {
                        SetOriginatorError::InvalidHeaderValue => {
                            let error = JSONRPCErrorError {
                                code: INVALID_REQUEST_ERROR_CODE,
                                message: format!(
                                    "Invalid clientInfo.name: '{name}'. Must be a valid HTTP header value."
                                ),
                                data: None,
                            };
                            self.outgoing.send_error(request_id.clone(), error).await;
                            return;
                        }
                        SetOriginatorError::AlreadyInitialized => {
                            // No-op. This is expected to happen if the originator is already set via env var.
                            // TODO(owen): Once we remove support for CODEX_INTERNAL_ORIGINATOR_OVERRIDE,
                            // this will be an unexpected state and we can return a JSON-RPC error indicating
                            // internal server error.
                        }
                    }
                }
                set_default_client_residency_requirement(self.config.enforce_residency.value());
                let user_agent_suffix = format!("{name}; {version}");
                if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
                    *suffix = Some(user_agent_suffix);
                }

                let user_agent = get_codex_user_agent();
                let response = InitializeResponse { user_agent };
                self.outgoing.send_response(request_id, response).await;

                session.initialized = true;
                if let Some(outbound_initialized) = outbound_initialized {
                    // In-process clients can complete readiness immediately here. The
                    // websocket path defers this until lib.rs finishes transport-layer
                    // initialize handling for the specific connection.
                    outbound_initialized.store(true, Ordering::Release);
                    self.codex_message_processor
                        .connection_initialized(connection_id)
                        .await;
                }
                return;
            }
            _ => {
                if !session.initialized {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "Not initialized".to_string(),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            }
        }
        if let Some(reason) = codex_request.experimental_reason()
            && !session.experimental_api_enabled
        {
            let error = JSONRPCErrorError {
                code: INVALID_REQUEST_ERROR_CODE,
                message: experimental_required_message(reason),
                data: None,
            };
            self.outgoing.send_error(request_id, error).await;
            return;
        }

        match codex_request {
            ClientRequest::ConfigRead { request_id, params } => {
                self.handle_config_read(
                    ConnectionRequestId {
                        connection_id,
                        request_id,
                    },
                    params,
                )
                .await;
            }
            ClientRequest::ExternalAgentConfigDetect { request_id, params } => {
                self.handle_external_agent_config_detect(
                    ConnectionRequestId {
                        connection_id,
                        request_id,
                    },
                    params,
                )
                .await;
            }
            ClientRequest::ExternalAgentConfigImport { request_id, params } => {
                self.handle_external_agent_config_import(
                    ConnectionRequestId {
                        connection_id,
                        request_id,
                    },
                    params,
                )
                .await;
            }
            ClientRequest::ConfigValueWrite { request_id, params } => {
                self.handle_config_value_write(
                    ConnectionRequestId {
                        connection_id,
                        request_id,
                    },
                    params,
                )
                .await;
            }
            ClientRequest::ConfigBatchWrite { request_id, params } => {
                self.handle_config_batch_write(
                    ConnectionRequestId {
                        connection_id,
                        request_id,
                    },
                    params,
                )
                .await;
            }
            ClientRequest::ConfigRequirementsRead {
                request_id,
                params: _,
            } => {
                self.handle_config_requirements_read(ConnectionRequestId {
                    connection_id,
                    request_id,
                })
                .await;
            }
            other => {
                // Box the delegated future so this wrapper's async state machine does not
                // inline the full `CodexMessageProcessor::process_request` future, which
                // can otherwise push worker-thread stack usage over the edge.
                self.codex_message_processor
                    .process_request(connection_id, other, session.app_server_client_name.clone())
                    .boxed()
                    .await;
            }
        }
    }

    async fn handle_config_read(&self, request_id: ConnectionRequestId, params: ConfigReadParams) {
        match self.config_api.read(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_value_write(
        &self,
        request_id: ConnectionRequestId,
        params: ConfigValueWriteParams,
    ) {
        match self.config_api.write_value(params).await {
            Ok(response) => {
                self.codex_message_processor.clear_plugin_related_caches();
                self.codex_message_processor
                    .maybe_start_curated_repo_sync_for_latest_config()
                    .await;
                self.outgoing.send_response(request_id, response).await;
            }
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_batch_write(
        &self,
        request_id: ConnectionRequestId,
        params: ConfigBatchWriteParams,
    ) {
        match self.config_api.batch_write(params).await {
            Ok(response) => {
                self.codex_message_processor.clear_plugin_related_caches();
                self.codex_message_processor
                    .maybe_start_curated_repo_sync_for_latest_config()
                    .await;
                self.outgoing.send_response(request_id, response).await;
            }
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_requirements_read(&self, request_id: ConnectionRequestId) {
        match self.config_api.config_requirements_read().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_external_agent_config_detect(
        &self,
        request_id: ConnectionRequestId,
        params: ExternalAgentConfigDetectParams,
    ) {
        match self.external_agent_config_api.detect(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_external_agent_config_import(
        &self,
        request_id: ConnectionRequestId,
        params: ExternalAgentConfigImportParams,
    ) {
        match self.external_agent_config_api.import(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionSessionState;
    use super::MessageProcessor;
    use super::MessageProcessorArgs;
    use crate::outgoing_message::ConnectionId;
    use crate::outgoing_message::OutgoingMessageSender;
    use crate::transport::AppServerTransport;
    use anyhow::Result;
    use app_test_support::create_mock_responses_server_repeating_assistant;
    use app_test_support::write_mock_responses_config_toml;
    use codex_app_server_protocol::ClientInfo;
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::InitializeCapabilities;
    use codex_app_server_protocol::InitializeParams;
    use codex_app_server_protocol::JSONRPCRequest;
    use codex_app_server_protocol::RequestId;
    use codex_app_server_protocol::ThreadStartParams;
    use codex_arg0::Arg0DispatchPaths;
    use codex_core::config::Config;
    use codex_core::config::ConfigBuilder;
    use codex_core::config_loader::CloudRequirementsLoader;
    use codex_core::config_loader::LoaderOverrides;
    use codex_feedback::CodexFeedback;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::W3cTraceContext;
    use opentelemetry::global;
    use opentelemetry::trace::SpanId;
    use opentelemetry::trace::SpanKind;
    use opentelemetry::trace::TraceId;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use opentelemetry_sdk::trace::SpanData;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use tempfile::TempDir;
    use tokio::sync::mpsc;
    use tracing::Instrument;
    use tracing::Subscriber;
    use tracing::field::Visit;
    use tracing::span::Attributes;
    use tracing::span::Id;
    use tracing_opentelemetry::OtelData;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::layer::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::LookupSpan;

    const TEST_CONNECTION_ID: ConnectionId = ConnectionId(7);

    struct TestTracing {
        exporter: InMemorySpanExporter,
        provider: SdkTracerProvider,
        lifecycle: SpanLifecycleRecorder,
    }

    #[derive(Clone, Default)]
    struct SpanLifecycleRecorder {
        state: Arc<Mutex<SpanLifecycleState>>,
    }

    #[derive(Default)]
    struct SpanLifecycleState {
        open_spans: HashMap<Id, RecordedSpan>,
        closed_spans: Vec<RecordedSpan>,
    }

    #[derive(Clone, Debug)]
    struct RecordedSpan {
        name: String,
        rpc_method: Option<String>,
        otel_trace_id: Option<TraceId>,
        otel_span_id: Option<SpanId>,
        enter_count: usize,
        exit_count: usize,
    }

    #[derive(Default)]
    struct SpanFieldRecorder {
        rpc_method: Option<String>,
    }

    impl Visit for SpanFieldRecorder {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "rpc.method" {
                self.rpc_method = Some(value.to_string());
            }
        }

        fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
    }

    impl SpanLifecycleRecorder {
        fn reset(&self) {
            let mut state = self.state.lock().expect("lock span lifecycle state");
            state.open_spans.clear();
            state.closed_spans.clear();
        }

        fn closed_request_span_for_method(&self, method: &str) -> Option<RecordedSpan> {
            let state = self.state.lock().expect("lock span lifecycle state");
            state
                .closed_spans
                .iter()
                .find(|span| {
                    span.name == "app_server.request" && span.rpc_method.as_deref() == Some(method)
                })
                .cloned()
        }

        fn open_span_names(&self) -> Vec<String> {
            let state = self.state.lock().expect("lock span lifecycle state");
            state
                .open_spans
                .values()
                .map(|span| span.name.clone())
                .collect()
        }
    }

    impl<S> Layer<S> for SpanLifecycleRecorder
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            let mut fields = SpanFieldRecorder::default();
            attrs.record(&mut fields);

            let metadata = ctx
                .metadata(id)
                .expect("span metadata should be available while span exists");
            let recorded = RecordedSpan {
                name: metadata.name().to_string(),
                rpc_method: fields.rpc_method,
                otel_trace_id: None,
                otel_span_id: None,
                enter_count: 0,
                exit_count: 0,
            };

            let mut state = self.state.lock().expect("lock span lifecycle state");
            state.open_spans.insert(id.clone(), recorded);
        }

        fn on_close(&self, id: Id, ctx: Context<'_, S>) {
            let mut state = self.state.lock().expect("lock span lifecycle state");
            if let Some(span) = state.open_spans.remove(&id) {
                let mut recorded = span;
                if let Some(span_ref) = ctx.span(&id) {
                    let extensions = span_ref.extensions();
                    if let Some(otel_data) = extensions.get::<OtelData>() {
                        recorded.otel_trace_id = otel_data.trace_id();
                        recorded.otel_span_id = otel_data.span_id();
                    }
                }
                state.closed_spans.push(recorded);
            }
        }

        fn on_enter(&self, id: &Id, _ctx: Context<'_, S>) {
            let mut state = self.state.lock().expect("lock span lifecycle state");
            if let Some(span) = state.open_spans.get_mut(id) {
                span.enter_count += 1;
            }
        }

        fn on_exit(&self, id: &Id, _ctx: Context<'_, S>) {
            let mut state = self.state.lock().expect("lock span lifecycle state");
            if let Some(span) = state.open_spans.get_mut(id) {
                span.exit_count += 1;
            }
        }
    }

    fn init_test_tracing() -> &'static TestTracing {
        static TEST_TRACING: OnceLock<TestTracing> = OnceLock::new();
        TEST_TRACING.get_or_init(|| {
            let exporter = InMemorySpanExporter::default();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let tracer = provider.tracer("codex-app-server-message-processor-tests");
            let lifecycle = SpanLifecycleRecorder::default();
            global::set_text_map_propagator(TraceContextPropagator::new());
            let subscriber = tracing_subscriber::registry()
                .with(lifecycle.clone())
                .with(tracing_opentelemetry::layer().with_tracer(tracer));
            tracing::subscriber::set_global_default(subscriber)
                .expect("global tracing subscriber should only be installed once");
            TestTracing {
                exporter,
                provider,
                lifecycle,
            }
        })
    }

    fn request_from_client_request(request: ClientRequest) -> JSONRPCRequest {
        serde_json::from_value(serde_json::to_value(request).expect("serialize client request"))
            .expect("client request should convert to JSON-RPC")
    }

    async fn build_test_config(codex_home: &Path, server_uri: &str) -> Result<Config> {
        write_mock_responses_config_toml(
            codex_home,
            server_uri,
            &BTreeMap::new(),
            8_192,
            Some(false),
            "mock_provider",
            "compact",
        )?;

        Ok(ConfigBuilder::default()
            .codex_home(codex_home.to_path_buf())
            .build()
            .await?)
    }

    fn build_test_processor(
        config: Arc<Config>,
    ) -> (
        MessageProcessor,
        mpsc::Receiver<crate::outgoing_message::OutgoingEnvelope>,
    ) {
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let outgoing = Arc::new(OutgoingMessageSender::new(outgoing_tx));
        let processor = MessageProcessor::new(MessageProcessorArgs {
            outgoing,
            arg0_paths: Arg0DispatchPaths::default(),
            config,
            cli_overrides: Vec::new(),
            loader_overrides: LoaderOverrides::default(),
            cloud_requirements: CloudRequirementsLoader::default(),
            feedback: CodexFeedback::new(),
            log_db: None,
            config_warnings: Vec::new(),
            session_source: SessionSource::VSCode,
            enable_codex_api_key_env: false,
        });
        (processor, outgoing_rx)
    }

    fn span_attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a str> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .and_then(|kv| match &kv.value {
                opentelemetry::Value::String(value) => Some(value.as_str()),
                _ => None,
            })
    }

    fn find_rpc_span<'a>(spans: &'a [SpanData], kind: SpanKind, method: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| {
                span.span_kind == kind
                    && span_attr(span, "rpc.system") == Some("jsonrpc")
                    && span_attr(span, "rpc.method") == Some(method)
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing {kind:?} span for rpc.method={method}; exported spans:\n{}",
                    format_spans(spans)
                )
            })
    }

    fn find_span_by_name<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| span.name.as_ref() == name)
            .unwrap_or_else(|| {
                panic!(
                    "missing span named {name}; exported spans:\n{}",
                    format_spans(spans)
                )
            })
    }

    fn format_spans(spans: &[SpanData]) -> String {
        spans
            .iter()
            .map(|span| {
                let rpc_method = span_attr(span, "rpc.method").unwrap_or("-");
                format!(
                    "name={} span_id={} kind={:?} parent={} trace={} rpc.method={}",
                    span.name,
                    span.span_context.span_id(),
                    span.span_kind,
                    span.parent_span_id,
                    span.span_context.trace_id(),
                    rpc_method
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn run_thread_start_request(
        processor: &mut MessageProcessor,
        session: &mut ConnectionSessionState,
        request_id: i64,
        trace: Option<W3cTraceContext>,
    ) {
        let mut thread_start_request = request_from_client_request(ClientRequest::ThreadStart {
            request_id: RequestId::Integer(request_id),
            params: ThreadStartParams {
                ephemeral: Some(true),
                ..ThreadStartParams::default()
            },
        });
        thread_start_request.trace = trace;

        processor
            .process_request(
                TEST_CONNECTION_ID,
                thread_start_request,
                AppServerTransport::Stdio,
                session,
            )
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn thread_start_jsonrpc_span_exports_server_span_and_parents_children() -> Result<()> {
        let server = create_mock_responses_server_repeating_assistant("Done").await;
        let codex_home = TempDir::new()?;
        let config = Arc::new(build_test_config(codex_home.path(), &server.uri()).await?);
        let (mut processor, _outgoing_rx) = build_test_processor(config);

        let tracing = init_test_tracing();
        tracing.exporter.reset();
        tracing.lifecycle.reset();

        let mut session = ConnectionSessionState::default();
        tracing::callsite::rebuild_interest_cache();

        let initialize_request = request_from_client_request(ClientRequest::Initialize {
            request_id: RequestId::Integer(1),
            params: InitializeParams {
                client_info: ClientInfo {
                    name: "codex-app-server-tests".to_string(),
                    title: None,
                    version: "0.1.0".to_string(),
                },
                capabilities: Some(InitializeCapabilities {
                    experimental_api: true,
                    ..Default::default()
                }),
            },
        });
        processor
            .process_request(
                TEST_CONNECTION_ID,
                initialize_request,
                AppServerTransport::Stdio,
                &mut session,
            )
            .await;
        assert!(session.initialized);

        let remote_trace_id =
            TraceId::from_hex("00000000000000000000000000000011").expect("trace id");
        let remote_parent_span_id = SpanId::from_hex("0000000000000022").expect("parent span id");
        let remote_trace = W3cTraceContext {
            traceparent: Some(format!(
                "00-{remote_trace_id}-{remote_parent_span_id}-01"
            )),
            tracestate: Some("vendor=value".to_string()),
        };

        let noop_request = JSONRPCRequest {
            id: RequestId::Integer(99),
            method: "thread/start".to_string(),
            params: Some(serde_json::to_value(ThreadStartParams::default())?),
            trace: Some(remote_trace.clone()),
        };
        let noop_request_span = crate::app_server_tracing::request_span(
            &noop_request,
            AppServerTransport::Stdio,
            TEST_CONNECTION_ID,
            &session,
        );
        async {}.instrument(noop_request_span).await;
        tokio::task::yield_now().await;
        tracing.provider.force_flush()?;
        let noop_spans = tracing.exporter.get_finished_spans().expect("span export");
        let noop_server_span = find_rpc_span(&noop_spans, SpanKind::Server, "thread/start");
        assert_eq!(noop_server_span.parent_span_id, remote_parent_span_id);
        assert_eq!(noop_server_span.span_context.trace_id(), remote_trace_id);

        tracing.exporter.reset();

        let wrapped_request = JSONRPCRequest {
            id: RequestId::Integer(100),
            method: "thread/start".to_string(),
            params: Some(serde_json::to_value(ThreadStartParams::default())?),
            trace: Some(remote_trace.clone()),
        };
        let wrapped_request_span = crate::app_server_tracing::request_span(
            &wrapped_request,
            AppServerTransport::Stdio,
            TEST_CONNECTION_ID,
            &session,
        );
        async {
            let request_json = serde_json::to_value(&wrapped_request)?;
            let _codex_request: ClientRequest = serde_json::from_value(request_json)?;
            Ok::<(), anyhow::Error>(())
        }
        .instrument(wrapped_request_span)
        .await?;
        tokio::task::yield_now().await;
        tracing.provider.force_flush()?;
        let wrapped_spans = tracing.exporter.get_finished_spans().expect("span export");
        let wrapped_server_span = find_rpc_span(&wrapped_spans, SpanKind::Server, "thread/start");
        assert_eq!(wrapped_server_span.parent_span_id, remote_parent_span_id);
        assert_eq!(wrapped_server_span.span_context.trace_id(), remote_trace_id);

        tracing.exporter.reset();

        let child_request = JSONRPCRequest {
            id: RequestId::Integer(101),
            method: "thread/start".to_string(),
            params: Some(serde_json::to_value(ThreadStartParams::default())?),
            trace: Some(remote_trace.clone()),
        };
        let child_request_span = crate::app_server_tracing::request_span(
            &child_request,
            AppServerTransport::Stdio,
            TEST_CONNECTION_ID,
            &session,
        );
        async {
            async {}
                .instrument(tracing::info_span!("app_server.thread_start.child_control"))
                .await;
        }
        .instrument(child_request_span)
        .await;
        tokio::task::yield_now().await;
        tracing.provider.force_flush()?;
        let child_spans = tracing.exporter.get_finished_spans().expect("span export");
        let child_server_span = find_rpc_span(&child_spans, SpanKind::Server, "thread/start");
        assert_eq!(child_server_span.parent_span_id, remote_parent_span_id);
        assert_eq!(child_server_span.span_context.trace_id(), remote_trace_id);

        tracing.exporter.reset();

        run_thread_start_request(&mut processor, &mut session, 2, None).await;
        tokio::task::yield_now().await;

        tracing.provider.force_flush()?;
        let untraced_spans = tracing.exporter.get_finished_spans().expect("span export");
        let untraced_server_span = find_rpc_span(&untraced_spans, SpanKind::Server, "thread/start");
        assert_eq!(untraced_server_span.name.as_ref(), "thread/start");

        tracing.exporter.reset();
        tracing.lifecycle.reset();

        run_thread_start_request(&mut processor, &mut session, 3, Some(remote_trace)).await;
        tokio::task::yield_now().await;
        drop(processor);
        tokio::task::yield_now().await;

        tracing.provider.force_flush()?;
        let spans = tracing.exporter.get_finished_spans().expect("span export");
        let closed_request_span = tracing
            .lifecycle
            .closed_request_span_for_method("thread/start")
            .expect("thread/start request span never closed");
        assert!(
            !tracing
                .lifecycle
                .open_span_names()
                .iter()
                .any(|name| name == "app_server.request"),
            "thread/start request span remained open"
        );

        let derive_config_span = find_span_by_name(&spans, "app_server.thread_start.derive_config");
        assert_eq!(
            closed_request_span.otel_span_id,
            Some(derive_config_span.parent_span_id),
            "thread/start child spans were not parented under the closed request span"
        );
        assert_eq!(
            closed_request_span.otel_trace_id,
            Some(derive_config_span.span_context.trace_id())
        );
        assert_eq!(
            closed_request_span.enter_count, closed_request_span.exit_count,
            "thread/start request span entered/exited unevenly: {:?}",
            closed_request_span
        );

        let server_request_span = find_rpc_span(&spans, SpanKind::Server, "thread/start");

        assert_eq!(server_request_span.name.as_ref(), "thread/start");
        assert_eq!(server_request_span.parent_span_id, remote_parent_span_id);
        assert!(server_request_span.parent_span_is_remote);
        assert_eq!(server_request_span.span_context.trace_id(), remote_trace_id);
        assert_ne!(server_request_span.span_context.span_id(), SpanId::INVALID);

        assert_eq!(
            derive_config_span.parent_span_id,
            server_request_span.span_context.span_id()
        );
        assert!(!derive_config_span.parent_span_is_remote);
        assert_eq!(
            derive_config_span.span_context.trace_id(),
            server_request_span.span_context.trace_id()
        );

        Ok(())
    }
}
