use std::time::{Duration, Instant};
use std::collections::HashMap;

use axum::response::IntoResponse;
use axum::{http::StatusCode, Extension, Json};
use lazy_static::lazy_static;
use log::info;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::core::mcp_core::MCPCore;
use crate::core::mcp_core_proxy_ext::McpCoreProxyExt;
use crate::models::types::{
    Distribution, ErrorResponse, InputSchema, RegistryToolsResponse, ServerConfiguration,
    ServerRegistrationRequest, ServerRegistrationResponse, ServerToolInfo, ServerToolsResponse,
    ToolExecutionRequest, InputSchemaProperty
};
use crate::types::{ConfigUpdateRequest, ServerConfigUpdateRequest};

use axum::{
    response::sse::{Event, Sse},
    extract::Query,
    debug_handler,
};
use futures::stream::{self, Stream};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use futures::StreamExt;
use uuid::Uuid;

use crate::mcp_server::handlers::SessionManager;
use mcp_sdk_server::Server;
use mcp_sdk_server::router::RouterService;
use crate::mcp_server::handlers::MCPRouter;
use crate::mcp_server::tools::{TOOL_REGISTER_SERVER, get_register_server_tool};

/// JSON-RPC request structure
#[derive(Deserialize, Debug)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Option<Value>,
}

/// JSON-RPC response structure
#[derive(Serialize, Debug)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error structure
#[derive(Serialize, Debug)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

/// JSON-RPC method enum
#[derive(Debug)]
pub enum JsonRpcMethod {
    ToolsList,
    ToolsHidden,
    ToolsCall,
    PromptsList,
    PromptsGet,
    ResourcesList,
    ResourcesRead,
    RegistryList,
    RegistryInstall,
    RegistryImport,
    ServerStart,
    ServerStop,
    ServerConfig,
    ServerDelete,
    Unknown(String),
}

// Cache structure to store registry data and timestamp
struct RegistryCache {
    data: Option<Value>,
    timestamp: Option<Instant>,
}

// Initialize the static cache with lazy_static
lazy_static! {
    static ref REGISTRY_CACHE: Mutex<RegistryCache> = Mutex::new(RegistryCache {
        data: None,
        timestamp: None,
    });
}

// Cache duration constant (1 minutes)
const CACHE_DURATION: Duration = Duration::from_secs(60);

pub async fn health_check() -> impl IntoResponse {
    (StatusCode::OK, "MCP Server is running!")
}

pub async fn handle_mcp_request(
    Extension(mcp_core): Extension<MCPCore>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    info!("Received MCP request: method={}", request.method);

    let result: Result<Value, Value> = match request.method.as_str() {
        "tools/list" => match handle_list_tools(mcp_core).await {
            Ok(response) => Ok(serde_json::to_value(response).unwrap()),
            Err(error) => Err(serde_json::to_value(error).unwrap()),
        },
        "tools/hidden" => handle_tools_hidden(mcp_core).await,
        "tools/call" => {
            if let Some(params) = request.params {
                handle_invoke_tool(mcp_core, params).await
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Missing parameters"
                }))
            }
        }
        "prompts/list" => handle_list_prompts().await,
        "resources/list" => handle_list_resources().await,
        "resources/read" => {
            if let Some(params) = request.params {
                handle_read_resource(params).await
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Invalid params - missing parameters for resource reading"
                }))
            }
        }
        "prompts/get" => {
            if let Some(params) = request.params {
                handle_get_prompt(params).await
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Invalid params - missing parameters for prompt retrieval"
                }))
            }
        }
        "registry/install" => {
            if let Some(params) = request.params {
                match handle_register_tool(mcp_core, params).await {
                    Ok(response) => Ok(serde_json::to_value(response).unwrap()),
                    Err(error) => Err(serde_json::to_value(error).unwrap()),
                }
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Missing parameters for tool installation"
                }))
            }
        }
        "registry/import" => {
            if let Some(params) = request.params {
                handle_import_server_from_url(mcp_core, params).await
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Missing parameters for server import"
                }))
            }
        }
        "registry/list" => handle_list_all_tools(mcp_core).await,
        "server/config" => {
            if let Some(params) = request.params {
                handle_get_server_config(mcp_core, params).await
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Invalid params - missing parameters for server config"
                }))
            }
        }
        _ => Err(json!({
            "code": -32601,
            "message": format!("Method '{}' not found", request.method)
        })),
    };

    match result {
        Ok(result) => Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(result),
            error: None,
        }),
        Err(error) => {
            let error_obj = error.as_object().unwrap();
            Json(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: error_obj
                        .get("code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(-32000) as i32,
                    message: error_obj
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error")
                        .to_string(),
                    data: None,
                }),
            })
        }
    }
}

async fn handle_list_tools(mcp_core: MCPCore) -> Result<ServerToolsResponse, ErrorResponse> {
    // Get the installed tools from MCPCore
    let result = mcp_core.list_all_server_tools().await;

    // Create a list of built-in tools converted to ServerToolInfo format
    let register_server_tool = get_register_server_tool();
    let built_in_tools = vec![
        ServerToolInfo {
            id: register_server_tool.name.clone(),
            name: TOOL_REGISTER_SERVER.to_string(),
            description: register_server_tool.description.clone(),
            server_id: "builtin".to_string(),
            proxy_id: None,
            is_active: true,
            input_schema: Some(InputSchema {
                r#type: "object".to_string(),
                properties: HashMap::from_iter(
                    serde_json::from_value::<HashMap<String, InputSchemaProperty>>(
                        register_server_tool.input_schema.get("properties")
                            .cloned()
                            .unwrap_or_else(|| json!({}))
                    ).unwrap_or_default()
                ),
                required: register_server_tool.input_schema.get("required")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .collect())
                    .unwrap_or_else(|| Vec::new()),
                ..Default::default()
            }),
        }
    ];

    match result {
        Ok(tools) => {
            // Add built-in tools first, then user-installed tools
            let mut all_tools = built_in_tools;
            
            // Add the user-installed tools
            let tools_with_defaults: Vec<ServerToolInfo> = tools
                .into_iter()
                .map(|tool| {
                    let mut tool = tool;
                    // Ensure input_schema has a default if not present
                    if tool.input_schema.is_none() {
                        let default_schema = InputSchema {
                            r#type: "object".to_string(),
                            ..Default::default()
                        };
                        tool.input_schema = Some(default_schema);
                    }
                    tool
                })
                .collect();
            
            all_tools.extend(tools_with_defaults);

            Ok(ServerToolsResponse {
                tools: all_tools,
            })
        }
        Err(e) => Err(ErrorResponse {
            code: -32000,
            message: format!("Failed to list tools: {}", e),
        }),
    }
}

#[derive(Deserialize, Debug)]

struct ToolRegistrationRequestByName {
    id: String,
    name: String,
    description: String,
    r#type: String,
    configuration: Option<ServerConfiguration>,
    distribution: Option<Distribution>,
}
#[derive(Deserialize, Debug)]

struct ToolRegistrationRequestById {
    tool_id: String,
}

#[derive(Deserialize, Debug)]
#[allow(clippy::large_enum_variant)]
#[serde(untagged)]
enum ToolRegistrationRequest {
    ByName(ToolRegistrationRequestByName),
    ById(ToolRegistrationRequestById),
}

async fn handle_register_tool(
    mcp_core: MCPCore,
    params: Value,
) -> Result<ServerRegistrationResponse, ErrorResponse> {
    println!("[INSTALLATION] handle_register_tool: params {:?}", params);
    let params = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            println!("[INSTALLATION] handle_register_tool: error {:?}", error);
            return Err(ErrorResponse {
                code: -32602,
                message: format!(
                    "Invalid params - missing parameters for tool registration: {}",
                    error
                ),
            });
        }
    };

    match params {
        ToolRegistrationRequest::ByName(request) => {
            println!(
                "[INSTALLATION] handle_register_tool: request (BY NAME) {:?}",
                request
            );
            let tool_id = request.id;
            let tool_name = request.name;
            let description = request.description;
            let tools_type = request.r#type;
            let configuration = request.configuration;
            let distribution = request.distribution;

            let tool = ServerRegistrationRequest {
                server_id: tool_id.clone(),
                server_name: tool_name,
                description,
                tools_type,
                configuration,
                distribution,
            };

            println!("[POST] handle_register_tool: tool {:?}", tool);
            let r = mcp_core.register_server(tool).await;
            println!("[INSTALLATION] handle_register_tool: r {:?}", r);
            Ok(ServerRegistrationResponse {
                success: true,
                message: "Tool installed successfully".to_string(),
                tool_id: Some(tool_id),
            })
        }
        ToolRegistrationRequest::ById(request) => {
            println!(
                "[INSTALLATION] handle_register_tool: request (BY ID) {:?}",
                request
            );
            let tool_id = request.tool_id;

            let registry = fetch_tool_from_registry().await?;

            let tool = registry
                .tools
                .iter()
                .find(|tool| tool.id.as_str() == tool_id);
            if tool.is_none() {
                return Err(ErrorResponse {
                    code: -32000,
                    message: format!("Tool {} not found", tool_id),
                });
            }
            let tool = tool.unwrap();
            println!("Building tool from registry: {:?}", tool);
            let r = mcp_core
                .register_server(ServerRegistrationRequest {
                    server_id: tool_id.clone(),
                    server_name: tool.name.clone(),
                    description: tool.description.clone(),
                    tools_type: tool.runtime.clone(),
                    configuration: Some(tool.config.clone()),
                    distribution: Some(tool.distribution.clone()),
                })
                .await;
            println!("[INSTALLATION] handle_register_tool: r {:?}", r);
            Ok(ServerRegistrationResponse {
                success: true,
                message: "Tool installed successfully".to_string(),
                tool_id: Some(tool_id.clone()),
            })
        }
    }
}

pub async fn fetch_tool_from_registry() -> Result<RegistryToolsResponse, ErrorResponse> {
    // Check if we have a valid cache
    let use_cache = {
        let cache = REGISTRY_CACHE.lock().await;
        if let (Some(data), Some(timestamp)) = (&cache.data, cache.timestamp) {
            if timestamp.elapsed() < CACHE_DURATION {
                // Cache is still valid
                match serde_json::from_value::<RegistryToolsResponse>(data.clone()) {
                    Ok(response) => Some(response),
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        }
    };

    // If we have valid cached data, return it
    if let Some(cached_data) = use_cache {
        return Ok(cached_data);
    }

    // Cache is invalid or doesn't exist, fetch fresh data
    // Fetch tools from remote URL
    // All Tools: Stable & Unstable
    let tools_url =
        "https://pub-5e2d77d67aac45ef811998185d312005.r2.dev/registry/registry.all.json";
    // Stable Only
    // let tools_url =
    //     "https://pub-5e2d77d67aac45ef811998185d312005.r2.dev/registry/registry.stable.json";
    // Unstable Only
    // let tools_url =
    //     "https://pub-5e2d77d67aac45ef811998185d312005.r2.dev/registry/registry.unstable.json";

    let client = reqwest::Client::builder().build().unwrap_or_default();

    let response = client
        .get(tools_url)
        .header("Accept-Encoding", "gzip")
        .header("User-Agent", "MCP-Core/1.0")
        .send()
        .await
        .map_err(|e| ErrorResponse {
            code: -32000,
            message: format!("Failed to fetch tools from registry: {}", e),
        })?;

    let raw = response.json().await.map_err(|e| ErrorResponse {
        code: -32000,
        message: format!("Failed to parse tools from registry: {}", e),
    })?;

    let tool_wrapper: RegistryToolsResponse =
        serde_json::from_value(raw).map_err(|e| ErrorResponse {
            code: -32000,
            message: format!("Failed to parse tools from registry: {}", e),
        })?;

    println!("[TOOLS] found # tools {:?}", tool_wrapper.tools.len());

    // let result = RegistryToolsResponse { tools };

    // Update the cache with new data
    {
        let mut cache = REGISTRY_CACHE.lock().await;
        cache.data = Some(serde_json::to_value(&tool_wrapper).unwrap_or_default());
        cache.timestamp = Some(Instant::now());
    }

    Ok(tool_wrapper)
}

async fn handle_list_all_tools(mcp_core: MCPCore) -> Result<Value, Value> {
    let mcp_state = mcp_core.mcp_state.read().await;
    let registry = mcp_state.tool_registry.read().await;
    let installed_tools = registry.get_all_servers()?;
    let registry_tools_result = fetch_tool_from_registry().await;

    let mut registry_tools = match registry_tools_result {
        Ok(response) => serde_json::to_value(response).unwrap_or(json!({"tools": []})),
        Err(error) => return Err(serde_json::to_value(error).unwrap()),
    };

    for tool in registry_tools
        .get_mut("tools")
        .unwrap()
        .as_array_mut()
        .unwrap()
    {
        let tool_name = tool.get("name").unwrap().as_str().unwrap();
        if installed_tools.contains_key(tool_name) {
            tool.as_object_mut()
                .unwrap()
                .insert("installed".to_string(), json!(true));
        } else {
            tool.as_object_mut()
                .unwrap()
                .insert("installed".to_string(), json!(false));
        }
    }

    Ok(registry_tools)
}

async fn handle_list_prompts() -> Result<Value, Value> {
    Ok(json!({
        "prompts": []
    }))
}

async fn handle_list_resources() -> Result<Value, Value> {
    Ok(json!({
        "resources": []
    }))
}

async fn handle_read_resource(_params: Value) -> Result<Value, Value> {
    Err(json!({
        "code": -32601,
        "message": "Resource reading not implemented yet"
    }))
}

async fn handle_get_prompt(_params: Value) -> Result<Value, Value> {
    Err(json!({
        "code": -32601,
        "message": "Prompt retrieval not implemented yet"
    }))
}

async fn handle_invoke_tool(mcp_core: MCPCore, params: Value) -> Result<Value, Value> {
    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(name) => name,
        None => {
            return Err(json!({
                "code": -32602,
                "message": "Missing name in parameters"
            }))
        }
    };

    let arguments = match params.get("arguments") {
        Some(args) => args.clone(),
        None => json!({}),
    };

    let mcp_state = mcp_core.mcp_state.read().await;
    let server_tools = mcp_state.server_tools.read().await;

    // Find which server has the requested tool
    let mut server_id = None;

    for (sid, tools) in &*server_tools {
        for tool in tools {
            if tool.id == tool_name {
                server_id = Some(sid.clone());
                break;
            }

            // Also check by name if id doesn't match
            if tool.name == tool_name {
                server_id = Some(sid.clone());
                break;
            }
        }

        if server_id.is_some() {
            break;
        }
    }

    // Drop the locks before proceeding
    drop(server_tools);
    drop(mcp_state);

    match server_id {
        Some(server_id) => {
            let request = ToolExecutionRequest {
                tool_id: format!("{}:{}", server_id, tool_name),
                parameters: arguments,
            };

            match mcp_core.execute_proxy_tool(request).await {
                Ok(response) => {
                    if response.success {
                        Ok(response.result.unwrap_or(json!(null)))
                    } else {
                        Err(json!({
                            "code": -32000,
                            "message": response.error.unwrap_or_else(|| "Unknown error".to_string())
                        }))
                    }
                }
                Err(e) => Err(json!({
                    "code": -32000,
                    "message": format!("Failed to execute tool: {}", e)
                })),
            }
        }
        None => Err(json!({
            "code": -32601,
            "message": format!("Tool '{}' not found", tool_name)
        })),
    }
}

async fn handle_import_server_from_url(mcp_core: MCPCore, params: Value) -> Result<Value, Value> {
    match params.get("url").and_then(|v| v.as_str()) {
        Some(url) => {
            info!("Importing server from URL: {}", url);

            match mcp_core.import_server_from_url(url.to_string()).await {
                Ok(response) => {
                    if response.success {
                        Ok(json!({
                            "success": true,
                            "message": response.message,
                            "server_id": response.tool_id
                        }))
                    } else {
                        Err(json!({
                            "code": -32000,
                            "message": response.message
                        }))
                    }
                }
                Err(e) => Err(json!({
                    "code": -32000,
                    "message": format!("Failed to import server: {}", e)
                })),
            }
        }
        None => Err(json!({
            "code": -32602,
            "message": "Missing URL parameter"
        })),
    }
}

async fn handle_get_server_config(mcp_core: MCPCore, params: Value) -> Result<Value, Value> {
    info!("handle_get_server_config: params {:?}", params);
    let config: ConfigUpdateRequest = match serde_json::from_value(params) {
        Ok(config) => config,
        Err(error) => {
            return Err(json!({
                "code": -32602,
                "message": format!("Invalid params - missing parameters for server config: {}", error)
            }));
        }
    };

    // Update the tool configuration
    match mcp_core
        .update_server_config(ServerConfigUpdateRequest {
            server_id: config.tool_id.to_string(),
            config: config.config,
        })
        .await
    {
        Ok(response) => {
            if !response.success {
                return Err(json!({
                    "code": -32000,
                    "message": response.message
                }));
            }

            // After successful config update, restart the tool
            match mcp_core
                .restart_server_command(config.tool_id.to_string())
                .await
            {
                Ok(restart_response) => {
                    if restart_response.success {
                        Ok(json!({
                            "message": format!("Configuration updated and tool restarted successfully: {}", restart_response.message)
                        }))
                    } else {
                        Err(json!({
                            "code": -32000,
                            "message": format!("Config updated but restart failed: {}", restart_response.message)
                        }))
                    }
                }
                Err(e) => Err(json!({
                    "code": -32000,
                    "message": format!("Config updated but restart error: {}", e)
                })),
            }
        }
        Err(e) => Err(json!({
            "code": -32000,
            "message": format!("Failed to update configuration: {}", e)
        })),
    }
}

async fn handle_tools_hidden(mcp_core: MCPCore) -> Result<Value, Value> {
    let hidden = mcp_core.are_tools_hidden().await;
    Ok(json!({ "hidden": hidden }))
}

/// Global session manager
static SESSION_MANAGER: once_cell::sync::Lazy<SessionManager> = once_cell::sync::Lazy::new(|| {
    SessionManager::new()
});

/// Global server instance
static MCP_SERVER: once_cell::sync::Lazy<std::sync::Mutex<Option<Server<RouterService<MCPRouter>>>>> = 
    once_cell::sync::Lazy::new(|| {
        std::sync::Mutex::new(None)
    });

/// Set the MCP server instance
pub fn set_mcp_server(server: Server<RouterService<MCPRouter>>) {
    let mut server_guard = MCP_SERVER.lock().unwrap();
    *server_guard = Some(server);
}

/// SSE endpoint handler with bidirectional communication
pub async fn sse_handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Create a unique session ID
    let session_id = Uuid::new_v4().to_string();
    
    // Log the connection
    info!("New SSE connection established: {}", session_id);
    
    // Create a channel for sending SSE events to the client
    let (tx, rx) = mpsc::channel::<Vec<u8>>(32);
    
    // Register the session with the standard channel
    SESSION_MANAGER.register_session(session_id.clone(), tx).await;
    
    // Create an initial event with the session ID
    let initial_event = futures::stream::once(futures::future::ok(
        Event::default()
            .event("endpoint")
            .data(format!("?sessionId={session_id}"))
    ));
    
    // Create a stream from the receiver that handles messages
    let message_stream = stream::unfold(rx, |mut rx| async move {
        if let Some(data) = rx.recv().await {
            let event = Event::default()
                .event("message")
                .data(String::from_utf8_lossy(&data).to_string());
            Some((Ok(event), rx))
        } else {
            None
        }
    });
    
    // Chain the initial event with the message stream
    let combined_stream = initial_event.chain(message_stream);
    
    // Return the SSE stream
    Sse::new(combined_stream)
}

/// Query parameter struct for session ID
#[derive(Debug, Deserialize)]
pub struct SessionIdParam {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// Handler for JSON-RPC requests via SSE
#[debug_handler]
pub async fn json_rpc_handler(
    Query(params): Query<SessionIdParam>,
    Extension(mcp_core): Extension<MCPCore>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let session_id = &params.session_id;
    info!("Processing JSON-RPC request for session {}: method={}", session_id, request.method);
    
    // Process the JSON-RPC request using the handle_mcp_request logic
    let result = match request.method.as_str() {
        "tools/list" => match handle_list_tools(mcp_core).await {
            Ok(response) => Ok(serde_json::to_value(response).unwrap()),
            Err(error) => Err(serde_json::to_value(error).unwrap()),
        },
        "tools/hidden" => handle_tools_hidden(mcp_core).await,
        "tools/call" => {
            if let Some(params) = request.params.clone() {
                handle_invoke_tool(mcp_core, params).await
            } else {
                Err(json!({
                    "code": -32602,
                    "message": "Missing parameters"
                }))
            }
        },
        // Add other methods as needed
        _ => Err(json!({
            "code": -32601,
            "message": format!("Method '{}' not found", request.method)
        })),
    };
    
    // Create the JSON-RPC response
    let response = match result {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(result),
            error: None,
        },
        Err(error) => {
            let error_obj = error.as_object().unwrap();
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: error_obj
                        .get("code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(-32000) as i32,
                    message: error_obj
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown error")
                        .to_string(),
                    data: None,
                }),
            }
        }
    };
    
    // Also send the response through the SSE channel
    if let Ok(response_json) = serde_json::to_vec(&response) {
        if let Err(e) = SESSION_MANAGER.send_to_session(session_id, response_json).await {
            log::error!("Failed to send response to SSE channel: {}", e);
        }
    }
    
    Json(response)
}

/// Handler for receiving messages from the client (binary data approach)
pub async fn message_handler(
    Query(params): Query<SessionIdParam>,
    Json(message): Json<Vec<u8>>,
) -> Json<serde_json::Value> {
    let session_id = &params.session_id;
    info!("Processing binary message for session {}: {} bytes", session_id, message.len());
    
    // Process the message using the MCP_SERVER and SESSION_MANAGER
    let result = process_client_message(session_id, message).await;
    
    match result {
        Ok(_) => Json(json!({ "success": true })),
        Err(e) => {
            log::error!("Failed to process message for session {}: {}", session_id, e);
            Json(json!({
                "success": false,
                "error": e
            }))
        }
    }
}

/// Process a client message using the MCP server (binary data approach)
async fn process_client_message(session_id: &str, message: Vec<u8>) -> Result<(), String> {
    // Create a buffer for the response
    const BUFFER_SIZE: usize = 1 << 12; // 4KB
    
    // Create bidirectional channels using simplex
    let (c2s_read, mut c2s_write) = io::simplex(BUFFER_SIZE);
    let (mut s2c_read, s2c_write) = io::simplex(BUFFER_SIZE);
    
    // Write the message to the client-to-server channel
    c2s_write.write_all(&message).await
        .map_err(|e| format!("Failed to write to c2s channel: {}", e))?;
    c2s_write.flush().await
        .map_err(|e| format!("Failed to flush c2s channel: {}", e))?;
    
    // Create a ByteTransport from the simplex channels
    let _bytes_transport = crate::ByteTransport::new(c2s_read, s2c_write);
    
    // Process the message with the server if available
    {
        let server_guard = MCP_SERVER.lock().unwrap();
        
        if let Some(_server) = &*server_guard {
            // We can't clone the server or hold the MutexGuard across an await point
            // Instead, we'll just process a single message and then drop the guard
            
            // TODO: Implement actual message processing with the server
            // This would be where we'd normally use server.process_message or similar
            // For now, we'll just log that we got the server
            log::info!("Got server for session {}, would process message here", session_id);
        } else {
            return Err(format!("No server instance available for session {}", session_id));
        }
        
        // MutexGuard is dropped here, before any await points
    }
    
    // Read the response from the server-to-client channel
    let mut response_buffer = vec![0u8; BUFFER_SIZE];
    let bytes_read = s2c_read.read(&mut response_buffer).await
        .map_err(|e| format!("Failed to read from s2c channel: {}", e))?;
    
    // If we got a response, forward it to the session
    if bytes_read > 0 {
        response_buffer.truncate(bytes_read);
        SESSION_MANAGER.send_to_session(session_id, response_buffer).await?;
    }
    
    Ok(())
}
