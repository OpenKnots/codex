use crate::outgoing_message::ConnectionId;
use codex_app_server_protocol::ToolProviderToolSpec;
use codex_protocol::dynamic_tools::DynamicToolSpec as CoreDynamicToolSpec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub(crate) struct ToolProviderRegistry {
    state: Arc<Mutex<ToolProviderRegistryState>>,
}

#[derive(Default)]
struct ToolProviderRegistryState {
    tools_by_name: HashMap<String, RegisteredToolProviderTool>,
}

#[derive(Clone)]
struct RegisteredToolProviderTool {
    connection_id: ConnectionId,
    name: String,
    description: String,
    input_schema: serde_json::Value,
    default_timeout: Duration,
    inject_into_context: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ToolProviderTarget {
    pub(crate) connection_id: ConnectionId,
    pub(crate) default_timeout: Duration,
}

impl ToolProviderRegistry {
    pub(crate) async fn register(
        &self,
        connection_id: ConnectionId,
        tool: ToolProviderToolSpec,
    ) -> Result<(), String> {
        validate_tool_provider_tool(&tool)?;
        let mut state = self.state.lock().await;
        if state.tools_by_name.contains_key(tool.name.as_str()) {
            return Err(format!(
                "dynamic tool name is already registered: {}",
                tool.name
            ));
        }
        state.tools_by_name.insert(
            tool.name.clone(),
            RegisteredToolProviderTool {
                connection_id,
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                default_timeout: Duration::from_millis(tool.default_timeout_ms),
                inject_into_context: tool.inject_into_context,
            },
        );
        Ok(())
    }

    pub(crate) async fn unregister(
        &self,
        connection_id: ConnectionId,
        name: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let Some(existing) = state.tools_by_name.get(name) else {
            return Err(format!("tool provider tool not found: {name}"));
        };
        if existing.connection_id != connection_id {
            return Err(format!("tool provider does not own tool: {name}"));
        }
        state.tools_by_name.remove(name);
        Ok(())
    }

    pub(crate) async fn remove_connection(&self, connection_id: ConnectionId) -> bool {
        let mut state = self.state.lock().await;
        let before = state.tools_by_name.len();
        state
            .tools_by_name
            .retain(|_, tool| tool.connection_id != connection_id);
        before != state.tools_by_name.len()
    }

    pub(crate) async fn snapshot_dynamic_tools(&self) -> Vec<CoreDynamicToolSpec> {
        let state = self.state.lock().await;
        let mut tools = state
            .tools_by_name
            .values()
            .map(|tool| CoreDynamicToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
                inject_into_context: tool.inject_into_context,
                provider_owned: true,
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        tools
    }

    pub(crate) async fn lookup_target(&self, tool_name: &str) -> Option<ToolProviderTarget> {
        let state = self.state.lock().await;
        state
            .tools_by_name
            .get(tool_name)
            .map(|tool| ToolProviderTarget {
                connection_id: tool.connection_id,
                default_timeout: tool.default_timeout,
            })
    }
}

fn validate_tool_provider_tool(tool: &ToolProviderToolSpec) -> Result<(), String> {
    let name = tool.name.trim();
    if name.is_empty() {
        return Err("dynamic tool name must not be empty".to_string());
    }
    if name != tool.name {
        return Err(format!(
            "dynamic tool name has leading/trailing whitespace: {}",
            tool.name
        ));
    }
    if name == "mcp" || name.starts_with("mcp__") {
        return Err(format!("dynamic tool name is reserved: {name}"));
    }
    if tool.default_timeout_ms == 0 {
        return Err(format!(
            "tool provider default timeout must be greater than zero for {name}"
        ));
    }
    if let Err(err) = codex_core::parse_tool_input_schema(&tool.input_schema) {
        return Err(format!(
            "dynamic tool input schema is not supported for {name}: {err}"
        ));
    }
    Ok(())
}
