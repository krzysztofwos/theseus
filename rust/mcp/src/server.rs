//! The MCP server: serves Theseus's tool catalog over the Model Context Protocol,
//! dispatching each tool call to a [`Session`].
//!
//! It holds the working model behind a lock. Each call reconstructs a session over
//! a copy of that model, runs the tool, and reads the model back, so an external
//! host's edits accumulate across calls exactly as they do for the agent loop.

use std::sync::{Arc, Mutex};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde_json::Value;
use theseus::{CargoToolchain, FsWorkspace, Session};
use theseus_modeling::Model;

/// A Model Context Protocol server over Theseus's [`Session`]. It holds the working
/// model and the write gate. An external host lists the tool catalog and calls
/// tools by name, driving the same surface as the agent loop.
pub struct TheseusMcp {
    model: Mutex<Model>,
    workspace: FsWorkspace,
    toolchain: CargoToolchain,
    allow_writes: bool,
}

impl TheseusMcp {
    /// Build a server over `model`, persisting writes through a filesystem
    /// workspace rooted at the repository.
    pub fn new(model: Model, allow_writes: bool) -> Self {
        Self {
            model: Mutex::new(model),
            workspace: FsWorkspace::at_repo_root(),
            toolchain: CargoToolchain,
            allow_writes,
        }
    }
}

impl ServerHandler for TheseusMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default()
            .with_server_info(Implementation::new(
                "theseus-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Theseus exposes its self-modeling operations as tools. Inspect the \
                 model with model, query, verify, and coverage, and edit it with patch.",
            );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = theseus::tool_catalog().iter().map(as_tool).collect();
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let input = Value::Object(request.arguments.unwrap_or_default());
        let mut model = self.model.lock().expect("the session lock is not poisoned");
        let mut session = Session::new(
            model.clone(),
            &self.workspace,
            &self.toolchain,
            self.allow_writes,
        );
        let outcome = session.call(request.name.as_ref(), &input);
        *model = session.into_model();
        drop(model);
        Ok(match outcome {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            // A failed tool returns its error as the result so the host can recover,
            // the way the agent loop feeds an error back into the conversation.
            Err(error) => {
                CallToolResult::error(vec![ContentBlock::text(format!("error: {error}"))])
            }
        })
    }
}

/// Render one catalog entry as an MCP [`Tool`]. The catalog carries the same
/// definitions the agent loop hands a model. The protocol names the schema field
/// `inputSchema`, which the `Tool` value supplies.
fn as_tool(entry: &Value) -> Tool {
    let text = |key| entry.get(key).and_then(Value::as_str).unwrap_or_default();
    let schema = entry
        .get("input_schema")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Tool::new(
        text("name").to_string(),
        text("description").to_string(),
        Arc::new(schema),
    )
}
