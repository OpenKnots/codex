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
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
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

fn tracing_test_guard() -> &'static tokio::sync::Mutex<()> {
    static GUARD: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| tokio::sync::Mutex::new(()))
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

fn assert_span_descends_from(spans: &[SpanData], child: &SpanData, ancestor: &SpanData) {
    let ancestor_span_id = ancestor.span_context.span_id();
    let mut parent_span_id = child.parent_span_id;
    while parent_span_id != SpanId::INVALID {
        if parent_span_id == ancestor_span_id {
            return;
        }
        let Some(parent_span) = spans
            .iter()
            .find(|span| span.span_context.span_id() == parent_span_id)
        else {
            break;
        };
        parent_span_id = parent_span.parent_span_id;
    }

    panic!(
        "span {} does not descend from {}; exported spans:\n{}",
        child.name,
        ancestor.name,
        format_spans(spans)
    );
}

async fn read_response<T: serde::de::DeserializeOwned>(
    outgoing_rx: &mut mpsc::Receiver<crate::outgoing_message::OutgoingEnvelope>,
    request_id: i64,
) -> T {
    loop {
        let envelope = tokio::time::timeout(std::time::Duration::from_secs(5), outgoing_rx.recv())
            .await
            .expect("timed out waiting for response")
            .expect("outgoing channel closed");
        let crate::outgoing_message::OutgoingEnvelope::ToConnection {
            connection_id,
            message,
        } = envelope
        else {
            continue;
        };
        if connection_id != TEST_CONNECTION_ID {
            continue;
        }
        let crate::outgoing_message::OutgoingMessage::Response(response) = message else {
            continue;
        };
        if response.id != RequestId::Integer(request_id) {
            continue;
        }
        return serde_json::from_value(response.result)
            .expect("response payload should deserialize");
    }
}

async fn wait_for_exported_spans<F>(tracing: &TestTracing, predicate: F) -> Vec<SpanData>
where
    F: Fn(&[SpanData]) -> bool,
{
    for _ in 0..200 {
        tokio::task::yield_now().await;
        tracing
            .provider
            .force_flush()
            .expect("force flush should succeed");
        let spans = tracing.exporter.get_finished_spans().expect("span export");
        if predicate(&spans) {
            return spans;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    panic!("timed out waiting for expected exported spans");
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
    let _guard = tracing_test_guard().lock().await;
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

    let remote_trace_id = TraceId::from_hex("00000000000000000000000000000011").expect("trace id");
    let remote_parent_span_id = SpanId::from_hex("0000000000000022").expect("parent span id");
    let remote_trace = W3cTraceContext {
        traceparent: Some(format!("00-{remote_trace_id}-{remote_parent_span_id}-01")),
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

    let default_model_span = find_span_by_name(&spans, "thread_spawn.default_model");
    let session_init_rollout_span = find_span_by_name(&spans, "session_init.rollout");
    assert_span_descends_from(&spans, default_model_span, server_request_span);
    assert_span_descends_from(&spans, session_init_rollout_span, server_request_span);

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn turn_start_jsonrpc_span_parents_core_turn_spans() -> Result<()> {
    let _guard = tracing_test_guard().lock().await;
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let config = Arc::new(build_test_config(codex_home.path(), &server.uri()).await?);
    let (mut processor, mut outgoing_rx) = build_test_processor(config);

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

    let thread_start_request = request_from_client_request(ClientRequest::ThreadStart {
        request_id: RequestId::Integer(2),
        params: ThreadStartParams {
            ephemeral: Some(true),
            ..ThreadStartParams::default()
        },
    });
    processor
        .process_request(
            TEST_CONNECTION_ID,
            thread_start_request,
            AppServerTransport::Stdio,
            &mut session,
        )
        .await;
    let thread_start_response: ThreadStartResponse = read_response(&mut outgoing_rx, 2).await;
    let thread_id = thread_start_response.thread.id.clone();

    tracing.exporter.reset();
    tracing.lifecycle.reset();

    let remote_trace_id = TraceId::from_hex("00000000000000000000000000000077").expect("trace id");
    let remote_parent_span_id = SpanId::from_hex("0000000000000088").expect("parent span id");
    let remote_trace = W3cTraceContext {
        traceparent: Some(format!("00-{remote_trace_id}-{remote_parent_span_id}-01")),
        tracestate: Some("vendor=value".to_string()),
    };
    let turn_start_request = JSONRPCRequest {
        id: RequestId::Integer(3),
        method: "turn/start".to_string(),
        params: Some(serde_json::to_value(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            model: None,
            service_tier: None,
            effort: None,
            summary: None,
            personality: None,
            output_schema: None,
            collaboration_mode: None,
        })?),
        trace: Some(remote_trace),
    };
    processor
        .process_request(
            TEST_CONNECTION_ID,
            turn_start_request,
            AppServerTransport::Stdio,
            &mut session,
        )
        .await;
    let turn_start_response: TurnStartResponse = read_response(&mut outgoing_rx, 3).await;
    let spans = wait_for_exported_spans(tracing, |spans| {
        spans
            .iter()
            .any(|span| span.name.as_ref() == "submission_dispatch")
            && spans
                .iter()
                .any(|span| span.name.as_ref() == "session_task.turn")
            && spans.iter().any(|span| span.name.as_ref() == "run_turn")
    })
    .await;
    let _ = turn_start_response;
    drop(processor);
    tokio::task::yield_now().await;

    let server_request_span = find_rpc_span(&spans, SpanKind::Server, "turn/start");
    let submission_dispatch_span = find_span_by_name(&spans, "submission_dispatch");
    let session_task_turn_span = find_span_by_name(&spans, "session_task.turn");
    let run_turn_span = find_span_by_name(&spans, "run_turn");

    assert_eq!(server_request_span.parent_span_id, remote_parent_span_id);
    assert!(server_request_span.parent_span_is_remote);
    assert_eq!(server_request_span.span_context.trace_id(), remote_trace_id);
    assert_span_descends_from(&spans, submission_dispatch_span, server_request_span);
    assert_span_descends_from(&spans, session_task_turn_span, server_request_span);
    assert_span_descends_from(&spans, run_turn_span, server_request_span);
    assert_span_descends_from(&spans, session_task_turn_span, submission_dispatch_span);
    assert_span_descends_from(&spans, run_turn_span, session_task_turn_span);

    Ok(())
}
