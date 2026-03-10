use super::connection_handling_websocket::DEFAULT_READ_TIMEOUT;
use super::connection_handling_websocket::connect_websocket;
use super::connection_handling_websocket::create_config_toml;
use super::connection_handling_websocket::read_jsonrpc_message;
use super::connection_handling_websocket::read_response_for_id;
use super::connection_handling_websocket::reserve_local_addr;
use super::connection_handling_websocket::send_initialize_request;
use super::connection_handling_websocket::send_request;
use super::connection_handling_websocket::spawn_websocket_server;
use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallParams;
use codex_app_server_protocol::DynamicToolCallResponse;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ToolProviderRegisterResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_protocol::models::FunctionCallOutputPayload;
use core_test_support::responses;
use futures::SinkExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use wiremock::MockServer;

#[tokio::test]
async fn websocket_tool_provider_routes_dynamic_tool_calls_to_owner_connection() -> Result<()> {
    let call_id = "ws-provider-call-1";
    let tool_name = "provider_tool";
    let tool_args = json!({ "city": "Paris" });
    let tool_call_arguments = serde_json::to_string(&tool_args)?;
    let responses = vec![
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_function_call(call_id, tool_name, &tool_call_arguments),
            responses::ev_completed("resp-1"),
        ]),
        create_final_assistant_message_sse_response("Done")?,
    ];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri(), "never")?;

    let bind_addr = reserve_local_addr()?;
    let mut process = spawn_websocket_server(codex_home.path(), bind_addr).await?;

    let test_result = async {
        let mut provider = connect_websocket(bind_addr).await?;
        let mut thread_client = connect_websocket(bind_addr).await?;

        send_initialize_request(&mut provider, 1, "ws_provider").await?;
        let provider_init = read_response_for_id(&mut provider, 1).await?;
        assert_eq!(provider_init.id, RequestId::Integer(1));

        send_initialize_request(&mut thread_client, 2, "ws_thread_client").await?;
        let thread_init = read_response_for_id(&mut thread_client, 2).await?;
        assert_eq!(thread_init.id, RequestId::Integer(2));

        send_request(
            &mut thread_client,
            "thread/start",
            4,
            Some(serde_json::to_value(ThreadStartParams::default())?),
        )
        .await?;
        let thread_start_response = read_response_for_id(&mut thread_client, 4).await?;
        let ThreadStartResponse { thread, .. } = to_response(thread_start_response)?;

        send_request(
            &mut provider,
            "toolProvider/register",
            3,
            Some(json!({
                "tool": {
                    "name": tool_name,
                    "description": "Provider tool",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"],
                        "additionalProperties": false
                    },
                    "defaultTimeoutMs": 1_000,
                    "injectIntoContext": true
                }
            })),
        )
        .await?;
        let register_response = read_response_for_id(&mut provider, 3).await?;
        let _: ToolProviderRegisterResponse = to_response(register_response)?;

        send_request(
            &mut thread_client,
            "turn/start",
            5,
            Some(serde_json::to_value(TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![V2UserInput::Text {
                    text: "Run the provider tool".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })?),
        )
        .await?;
        let turn_start_response = read_response_for_id(&mut thread_client, 5).await?;
        let TurnStartResponse { turn } = to_response::<TurnStartResponse>(turn_start_response)?;

        let (request_id, params) = loop {
            match timeout(DEFAULT_READ_TIMEOUT, read_jsonrpc_message(&mut provider)).await?? {
                JSONRPCMessage::Request(request) => match ServerRequest::try_from(request)? {
                    ServerRequest::DynamicToolCall { request_id, params } => {
                        break (request_id, params);
                    }
                    other => bail!("expected provider DynamicToolCall request, got {other:?}"),
                },
                JSONRPCMessage::Notification(_) | JSONRPCMessage::Response(_) => {}
                JSONRPCMessage::Error(error) => {
                    bail!("unexpected websocket error on provider client: {error:?}");
                }
            }
        };
        assert_eq!(
            params,
            DynamicToolCallParams {
                thread_id: thread.id.clone(),
                turn_id: turn.id,
                call_id: call_id.to_string(),
                tool: tool_name.to_string(),
                arguments: tool_args,
            }
        );

        send_response(
            &mut thread_client,
            request_id.clone(),
            serde_json::to_value(DynamicToolCallResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "hijack".to_string(),
                }],
                success: true,
            })?,
        )
        .await?;
        sleep(Duration::from_millis(250)).await;
        assert_eq!(responses_bodies(&server).await?.len(), 1);

        send_response(
            &mut provider,
            request_id,
            serde_json::to_value(DynamicToolCallResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "dynamic-ok".to_string(),
                }],
                success: true,
            })?,
        )
        .await?;

        loop {
            match timeout(
                DEFAULT_READ_TIMEOUT,
                read_jsonrpc_message(&mut thread_client),
            )
            .await??
            {
                JSONRPCMessage::Request(request) => {
                    bail!("thread client unexpectedly received request: {request:?}");
                }
                JSONRPCMessage::Notification(notification)
                    if notification.method == "turn/completed" =>
                {
                    break;
                }
                JSONRPCMessage::Notification(_) | JSONRPCMessage::Response(_) => {}
                JSONRPCMessage::Error(error) => {
                    bail!("unexpected websocket error on thread client: {error:?}");
                }
            }
        }

        let bodies = responses_bodies(&server).await?;
        let payload = bodies
            .iter()
            .find_map(|body| function_call_output_payload(body, call_id))
            .context("expected function_call_output in follow-up request")?;
        assert_eq!(
            payload,
            FunctionCallOutputPayload::from_text("dynamic-ok".to_string())
        );

        Ok(())
    }
    .await;

    process
        .kill()
        .await
        .context("failed to stop websocket app-server process")?;
    test_result
}

#[tokio::test]
async fn websocket_tool_provider_disconnect_fails_in_flight_request_after_grace_period()
-> Result<()> {
    let call_id = "ws-provider-disconnect-call-1";
    let tool_name = "provider_tool";
    let tool_args = json!({ "city": "Paris" });
    let tool_call_arguments = serde_json::to_string(&tool_args)?;
    let responses = vec![
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_function_call(call_id, tool_name, &tool_call_arguments),
            responses::ev_completed("resp-1"),
        ]),
        create_final_assistant_message_sse_response("Done")?,
    ];
    let server = create_mock_responses_server_sequence_unchecked(responses).await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri(), "never")?;

    let bind_addr = reserve_local_addr()?;
    let mut process = spawn_websocket_server(codex_home.path(), bind_addr).await?;

    let test_result = async {
        let mut provider = connect_websocket(bind_addr).await?;
        let mut thread_client = connect_websocket(bind_addr).await?;

        send_initialize_request(&mut provider, 1, "ws_provider").await?;
        let provider_init = read_response_for_id(&mut provider, 1).await?;
        assert_eq!(provider_init.id, RequestId::Integer(1));

        send_initialize_request(&mut thread_client, 2, "ws_thread_client").await?;
        let thread_init = read_response_for_id(&mut thread_client, 2).await?;
        assert_eq!(thread_init.id, RequestId::Integer(2));

        send_request(
            &mut thread_client,
            "thread/start",
            4,
            Some(serde_json::to_value(ThreadStartParams::default())?),
        )
        .await?;
        let thread_start_response = read_response_for_id(&mut thread_client, 4).await?;
        let ThreadStartResponse { thread, .. } = to_response(thread_start_response)?;

        send_request(
            &mut provider,
            "toolProvider/register",
            3,
            Some(json!({
                "tool": {
                    "name": tool_name,
                    "description": "Provider tool",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"],
                        "additionalProperties": false
                    },
                    "defaultTimeoutMs": 30_000,
                    "injectIntoContext": true
                }
            })),
        )
        .await?;
        let register_response = read_response_for_id(&mut provider, 3).await?;
        let _: ToolProviderRegisterResponse = to_response(register_response)?;

        send_request(
            &mut thread_client,
            "turn/start",
            5,
            Some(serde_json::to_value(TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![V2UserInput::Text {
                    text: "Run the provider tool".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })?),
        )
        .await?;
        let turn_start_response = read_response_for_id(&mut thread_client, 5).await?;
        let _: TurnStartResponse = to_response::<TurnStartResponse>(turn_start_response)?;

        let request_id = loop {
            match timeout(DEFAULT_READ_TIMEOUT, read_jsonrpc_message(&mut provider)).await?? {
                JSONRPCMessage::Request(request) => match ServerRequest::try_from(request)? {
                    ServerRequest::DynamicToolCall { request_id, .. } => break request_id,
                    other => bail!("expected provider DynamicToolCall request, got {other:?}"),
                },
                JSONRPCMessage::Notification(_) | JSONRPCMessage::Response(_) => {}
                JSONRPCMessage::Error(error) => {
                    bail!("unexpected websocket error on provider client: {error:?}");
                }
            }
        };

        provider
            .close(None)
            .await
            .context("failed to close provider websocket")?;
        drop(provider);

        let (bodies, payload) = wait_for_function_call_output(&server, call_id).await?;
        assert_eq!(
            payload,
            FunctionCallOutputPayload::from_text(
                "dynamic tool provider is unavailable".to_string(),
            )
        );

        let response_count = bodies.len();
        send_response(
            &mut thread_client,
            request_id,
            serde_json::to_value(DynamicToolCallResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "late-response".to_string(),
                }],
                success: true,
            })?,
        )
        .await?;
        sleep(Duration::from_millis(250)).await;
        assert_eq!(responses_bodies(&server).await?.len(), response_count);

        Ok(())
    }
    .await;

    process
        .kill()
        .await
        .context("failed to stop websocket app-server process")?;
    test_result
}

async fn send_response(
    stream: &mut super::connection_handling_websocket::WsClient,
    id: RequestId,
    result: serde_json::Value,
) -> Result<()> {
    let payload = serde_json::to_string(&JSONRPCMessage::Response(JSONRPCResponse { id, result }))?;
    stream
        .send(WebSocketMessage::Text(payload.into()))
        .await
        .context("failed to send websocket response frame")?;
    Ok(())
}

async fn responses_bodies(server: &MockServer) -> Result<Vec<Value>> {
    let requests = server
        .received_requests()
        .await
        .context("failed to fetch received requests")?;

    requests
        .into_iter()
        .filter(|req| req.url.path().ends_with("/responses"))
        .map(|req| {
            req.body_json::<Value>()
                .context("request body should be JSON")
        })
        .collect()
}

fn function_call_output_payload(body: &Value, call_id: &str) -> Option<FunctionCallOutputPayload> {
    body.get("input")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(Value::as_str) == Some(call_id)
            })
        })
        .and_then(|item| item.get("output"))
        .cloned()
        .and_then(|output| serde_json::from_value(output).ok())
}

async fn wait_for_function_call_output(
    server: &MockServer,
    call_id: &str,
) -> Result<(Vec<Value>, FunctionCallOutputPayload)> {
    timeout(Duration::from_secs(15), async {
        loop {
            let bodies = responses_bodies(server).await?;
            if let Some(payload) = bodies
                .iter()
                .find_map(|body| function_call_output_payload(body, call_id))
            {
                return Ok((bodies, payload));
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await?
}
