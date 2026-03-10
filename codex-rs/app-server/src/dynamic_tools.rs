use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallResponse;
use codex_app_server_protocol::RequestId;
use codex_core::CodexThread;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem as CoreDynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolResponse as CoreDynamicToolResponse;
use codex_protocol::protocol::Op;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::error;

use crate::outgoing_message::ClientRequestResult;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::server_request_error::is_dynamic_tool_provider_disconnected_server_request_error;
use crate::server_request_error::is_turn_transition_server_request_error;

pub(crate) async fn on_call_response(
    call_id: String,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<CodexThread>,
    outgoing: ThreadScopedOutgoingMessageSender,
    response_timeout: Option<Duration>,
) {
    let response = match response_timeout {
        Some(response_timeout) => match tokio::time::timeout(response_timeout, receiver).await {
            Ok(response) => response,
            Err(_) => {
                let _canceled = outgoing.cancel_request(&pending_request_id).await;
                submit_response(
                    call_id,
                    fallback_response("dynamic tool request timed out").0,
                    conversation,
                )
                .await;
                return;
            }
        },
        None => receiver.await,
    };
    let (response, _error) = match response {
        Ok(Ok(value)) => decode_response(value),
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) if is_dynamic_tool_provider_disconnected_server_request_error(&err) => {
            fallback_response("dynamic tool provider is unavailable")
        }
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            fallback_response("dynamic tool request failed")
        }
        Err(err) => {
            error!("request failed: {err:?}");
            fallback_response("dynamic tool request failed")
        }
    };

    submit_response(call_id, response, conversation).await;
}

pub(crate) async fn submit_failed_tool_response(
    call_id: String,
    message: &str,
    conversation: Arc<CodexThread>,
) {
    submit_response(call_id, fallback_response(message).0, conversation).await;
}

fn decode_response(value: serde_json::Value) -> (DynamicToolCallResponse, Option<String>) {
    match serde_json::from_value::<DynamicToolCallResponse>(value) {
        Ok(response) => (response, None),
        Err(err) => {
            error!("failed to deserialize DynamicToolCallResponse: {err}");
            fallback_response("dynamic tool response was invalid")
        }
    }
}

fn fallback_response(message: &str) -> (DynamicToolCallResponse, Option<String>) {
    (
        DynamicToolCallResponse {
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: message.to_string(),
            }],
            success: false,
        },
        Some(message.to_string()),
    )
}

async fn submit_response(
    call_id: String,
    response: DynamicToolCallResponse,
    conversation: Arc<CodexThread>,
) {
    let DynamicToolCallResponse {
        content_items,
        success,
    } = response;
    let core_response = CoreDynamicToolResponse {
        content_items: content_items
            .into_iter()
            .map(CoreDynamicToolCallOutputContentItem::from)
            .collect(),
        success,
    };
    if let Err(err) = conversation
        .submit(Op::DynamicToolResponse {
            id: call_id,
            response: core_response,
        })
        .await
    {
        error!("failed to submit DynamicToolResponse: {err}");
    }
}
