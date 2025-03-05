use crate::mcp_state::MCPState;
use crate::models::models::{
    DiscoverServerToolsRequest, DiscoverServerToolsResponse, Tool, ToolConfig,
    ToolConfigUpdateRequest, ToolConfigUpdateResponse, ToolConfiguration, ToolExecutionRequest,
    ToolExecutionResponse, ToolId, ToolRegistrationRequest, ToolRegistrationResponse, ToolType,
    ToolUninstallRequest, ToolUninstallResponse, ToolUpdateRequest, ToolUpdateResponse,
};
use crate::registry::ToolRegistry;
use crate::{database, dm_process::DMProcess, MCPError};
use log::{error, info, warn};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    process::{Child, Command},
    time::Duration,
};

/// Discover tools available from an MCP server
pub async fn discover_server_tools(
    server_id: &str,
    registry: &mut ToolRegistry,
) -> Result<Vec<Value>, String> {
    // Get the stdin/stdout handles for the server
    let (stdin, stdout) = match registry.process_ios.get_mut(server_id) {
        Some(io) => io,
        None => return Err(format!("Server {} not found or not running", server_id)),
    };

    info!("Discovering tools from server {}", server_id);

    // According to MCP specification, the correct method is "tools/list"
    // https://github.com/modelcontextprotocol/specification/blob/main/docs/specification/2024-11-05/server/tools.md
    let discover_cmd = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });

    // Send the command to the process
    let cmd_str = serde_json::to_string(&discover_cmd)
        .map_err(|e| format!("Failed to serialize command: {}", e))?
        + "\n";

    info!("Command: {}", cmd_str.trim());

    // Write command to stdin
    stdin
        .write_all(cmd_str.as_bytes())
        .await
        .map_err(|e| format!("Failed to write to process stdin: {}", e))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("Failed to flush stdin: {}", e))?;

    // Read the response with a timeout
    let mut reader = tokio::io::BufReader::new(&mut *stdout);
    let mut response_line = String::new();

    let read_result = tokio::time::timeout(
        Duration::from_secs(10),
        reader.read_line(&mut response_line),
    )
    .await;

    match read_result {
        Ok(Ok(0)) => return Err("Server process closed connection".to_string()),
        Ok(Ok(_)) => info!(
            "Received response from server {}: {}",
            server_id,
            response_line.trim()
        ),
        Ok(Err(e)) => return Err(format!("Failed to read from process stdout: {}", e)),
        Err(_) => {
            return Err(format!(
                "Timeout waiting for response from server {}",
                server_id
            ))
        }
    }

    if response_line.is_empty() {
        return Err("No response from process".to_string());
    }

    // Parse the response
    let response: Value = match serde_json::from_str(&response_line) {
        Ok(json) => json,
        Err(e) => return Err(format!("Failed to parse response as JSON: {}", e)),
    };

    // Check for error in the response
    if let Some(error) = response.get("error") {
        return Err(format!("Server returned error: {:?}", error));
    }

    // According to MCP spec, tools should be in the result field
    if let Some(result) = response.get("result") {
        // MCP returns tools directly in the result field as array
        if let Some(tools_array) = result.as_array() {
            info!("Found {} tools in result array", tools_array.len());
            return Ok(tools_array.clone());
        }

        // Some implementations might nest it under a tools field
        if let Some(tools) = result.get("tools") {
            if let Some(tools_array) = tools.as_array() {
                info!("Found {} tools in result.tools array", tools_array.len());
                return Ok(tools_array.clone());
            }
        }

        // If there's a result but we couldn't find tools array, try to use the entire result
        info!("No tools array found, using entire result as fallback");
        return Ok(vec![result.clone()]);
    }

    // If the server doesn't fully comply with MCP but has a tools field at root
    if let Some(tools) = response.get("tools") {
        if let Some(tools_array) = tools.as_array() {
            info!("Found {} tools in root tools array", tools_array.len());
            return Ok(tools_array.clone());
        }
    }

    // If initialization hasn't completed yet or tools are not supported,
    // return an empty array as fallback
    info!("No tools found in response: {}", response_line.trim());
    Ok(Vec::new())
}

/// Execute a tool on an MCP server
pub async fn execute_server_tool(
    server_id: &str,
    tool_name: &str,
    parameters: Value,
    registry: &mut ToolRegistry,
) -> Result<Value, MCPError> {
    let (stdin, stdout) = registry
        .process_ios
        .get_mut(server_id)
        .ok_or_else(|| MCPError::ServerNotFound(server_id.to_string()))?;

    let execute_cmd = json!({
        "jsonrpc": "2.0",
        "id": format!("execute_{}_{}", server_id, tool_name),
        "method": "tools/call",
        "params": { "name": tool_name, "arguments": parameters }
    });

    let cmd_str = serde_json::to_string(&execute_cmd)
        .map_err(|e| MCPError::SerializationError(e.to_string()))?
        + "\n";

    stdin
        .write_all(cmd_str.as_bytes())
        .await
        .map_err(|e| MCPError::StdinWriteError(e.to_string()))?;
    stdin
        .flush()
        .await
        .map_err(|e| MCPError::StdinFlushError(e.to_string()))?;

    let mut reader = tokio::io::BufReader::new(&mut *stdout);
    let mut response_line = String::new();

    let read_result = tokio::time::timeout(
        Duration::from_secs(30),
        reader.read_line(&mut response_line),
    )
    .await;

    match read_result {
        Ok(Ok(0)) => return Err(MCPError::ServerClosedConnection),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(MCPError::StdoutReadError(e.to_string())),
        Err(_) => return Err(MCPError::TimeoutError(server_id.to_string())),
    }

    if response_line.is_empty() {
        return Err(MCPError::NoResponse);
    }

    let response: Value = serde_json::from_str(&response_line)
        .map_err(|e| MCPError::JsonParseError(e.to_string()))?;

    if let Some(error) = response.get("error") {
        return Err(MCPError::ToolExecutionError(error.to_string()));
    }

    response
        .get("result")
        .cloned()
        .ok_or(MCPError::NoResultField)
}

/// Spawn an MCP server process using DMProcess
pub async fn spawn_process(
    configuration: &Value,
    tool_id: &str,
    tool_type: &str,
    env_vars: Option<&HashMap<String, String>>,
) -> Result<
    (
        Child,
        tokio::process::ChildStdin,
        tokio::process::ChildStdout,
    ),
    String,
> {
    let command = configuration
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Configuration missing 'command' field or not a string".to_string())?;

    let args = configuration
        .get("args")
        .and_then(|v| v.as_array())
        .map(|args| {
            args.iter()
                .filter_map(|arg| arg.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let config = ToolConfiguration {
        command: command.to_string(),
        args: Some(args),
    };

    let tool_type = match tool_type {
        "node" => ToolType::Node,
        "python" => ToolType::Python,
        "docker" => ToolType::Docker,
        _ => return Err(format!("Unsupported tool type: {}", tool_type)),
    };

    let tool_id = ToolId::new(tool_id.to_string());
    let dm_process = DMProcess::new(&tool_id, &tool_type, &config, env_vars).await?;
    Ok((dm_process.child, dm_process.stdin, dm_process.stdout))
}

/// Kill a running process
pub async fn kill_process(process: &mut Child) -> Result<(), String> {
    match process.kill().await {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("Failed to kill process: {}", e)),
    }
}

/// Register a new tool with the MCP server
pub async fn register_tool(
    mcp_state: &MCPState,
    request: ToolRegistrationRequest,
) -> Result<ToolRegistrationResponse, String> {
    info!("Starting registration of tool: {}", request.tool_name);

    // Safely access the command field if configuration exists
    if let Some(config) = &request.configuration {
        if let Some(cmd) = config.get("command") {
            info!("Command: {}", cmd);
        } else {
            info!("Command: Not specified in configuration");
        }
    } else {
        info!("Configuration not provided");
    }

    let mut registry = mcp_state.tool_registry.write().await;

    // Generate a simple tool ID (in production, use UUIDs)
    let tool_id = format!("tool_{}", registry.tools.len() + 1);
    info!("Generated tool ID: {}", tool_id);

    // Create the tool configuration if provided
    let configuration = request
        .configuration
        .as_ref()
        .map(|config| ToolConfiguration {
            command: config
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            args: config.get("args").and_then(|v| v.as_array()).map(|args| {
                args.iter()
                    .filter_map(|arg| arg.as_str().map(|s| s.to_string()))
                    .collect()
            }),
        });

    // Create the tool config with env variables if provided
    let mut tool_config = None;
    if let Some(auth) = &request.authentication {
        if let Some(env) = auth.get("env") {
            if let Some(env_obj) = env.as_object() {
                let mut env_map = HashMap::new();
                for (key, value) in env_obj {
                    // Extract the value as a string
                    let value_str = match value {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => {
                            // For objects, check if it has a description field (which means it's a template)
                            if let Value::Object(obj) = value {
                                if obj.contains_key("description") {
                                    // This is a template, so we don't have a value yet
                                    continue;
                                }
                            }
                            // For other types, convert to JSON string
                            value.to_string()
                        }
                    };
                    env_map.insert(key.clone(), value_str);
                }
                tool_config = Some(ToolConfig {
                    env: Some(env_map),
                    command: None,
                    args: None,
                });
            }
        }
    }

    // Create the Tool struct
    let tool = Tool {
        name: request.tool_name.clone(),
        description: request.description.clone(),
        enabled: true, // Default to enabled
        tool_type: request.tool_type.clone(),
        entry_point: None,
        configuration,
        distribution: request.distribution.clone(),
        config: tool_config,
        authentication: request.authentication.clone(),
    };

    registry.tools.insert(tool_id.clone(), tool.clone());

    // Create a default empty tools list
    registry.server_tools.insert(tool_id.clone(), Vec::new());

    // Extract environment variables from the tool config
    let env_vars = if let Some(config) = &tool.config {
        config.env.clone()
    } else {
        None
    };

    // Create the config_value for the spawn functions
    let config_value = if let Some(configuration) = &tool.configuration {
        json!({
            "command": configuration.command,
            "args": configuration.args
        })
    } else if let Some(config) = &tool.config {
        if let Some(command) = &config.command {
            json!({
                "command": command,
                "args": config.args.clone().unwrap_or_default()
            })
        } else {
            return Err("Configuration is required for tools".to_string());
        }
    } else {
        return Err("Configuration is required for tools".to_string());
    };

    // Spawn process based on tool type
    let spawn_result = spawn_process(
        &config_value,
        &tool_id,
        &request.tool_type,
        env_vars.as_ref(),
    )
    .await;

    match spawn_result {
        Ok((process, stdin, stdout)) => {
            info!("Process spawned successfully for tool ID: {}", tool_id);
            registry.processes.insert(tool_id.clone(), Some(process));
            registry
                .process_ios
                .insert(tool_id.clone(), (stdin, stdout));

            // Wait a moment for the server to start
            info!("Waiting for server to initialize...");
            drop(registry); // Release the lock during sleep
            tokio::time::sleep(Duration::from_secs(3)).await;

            // Try to discover tools from this server with a timeout to avoid hanging
            info!("Attempting to discover tools from server {}", tool_id);
            let discover_result = tokio::time::timeout(Duration::from_secs(15), async {
                let mut registry = mcp_state.tool_registry.write().await;
                discover_server_tools(&tool_id, &mut registry).await
            })
            .await;

            // Handle the result of the discovery attempt
            match discover_result {
                Ok(Ok(tools)) => {
                    info!(
                        "Successfully discovered {} tools from {}",
                        tools.len(),
                        tool_id
                    );
                    let mut registry = mcp_state.tool_registry.write().await;
                    // Clone tools before inserting to avoid the "moved value" error
                    let tools_clone = tools.clone();
                    registry.server_tools.insert(tool_id.clone(), tools);

                    // If empty tools, add a default "main" tool
                    if tools_clone.is_empty() {
                        info!("No tools discovered, adding a default main tool");
                        let default_tool = json!({
                            "id": "main",
                            "name": request.tool_name,
                            "description": request.description
                        });
                        registry
                            .server_tools
                            .insert(tool_id.clone(), vec![default_tool]);
                    }
                }
                Ok(Err(e)) => {
                    error!("Error discovering tools from server {}: {}", tool_id, e);
                    // Add a default tool since discovery failed
                    let mut registry = mcp_state.tool_registry.write().await;
                    let default_tool = json!({
                        "id": "main",
                        "name": request.tool_name,
                        "description": request.description
                    });
                    registry
                        .server_tools
                        .insert(tool_id.clone(), vec![default_tool]);
                    info!("Added default tool for server {}", tool_id);
                }
                Err(_) => {
                    error!("Timeout while discovering tools from server {}", tool_id);
                    // Add a default tool since discovery timed out
                    let mut registry = mcp_state.tool_registry.write().await;
                    let default_tool = json!({
                        "id": "main",
                        "name": request.tool_name,
                        "description": request.description
                    });
                    registry
                        .server_tools
                        .insert(tool_id.clone(), vec![default_tool]);
                    info!("Added default tool for server {} after timeout", tool_id);
                }
            }
        }
        Err(e) => {
            error!("Failed to spawn process for {}: {}", tool_id, e);
            return Ok(ToolRegistrationResponse {
                success: false,
                message: format!("Tool registered but failed to start process: {}", e),
                tool_id: Some(tool_id),
            });
        }
    }

    info!("Tool registration completed for: {}", request.tool_name);
    Ok(ToolRegistrationResponse {
        success: true,
        message: format!("Tool '{}' registered successfully", request.tool_name),
        tool_id: Some(tool_id),
    })
}

/// List all registered tools
pub async fn list_tools(mcp_state: &MCPState) -> Result<Vec<Value>, String> {
    let registry = mcp_state.tool_registry.read().await;
    let mut tools = Vec::new();

    for (id, tool_struct) in registry.tools.iter() {
        // Convert the Tool struct to a Value
        let mut tool_value = json!({
            "name": tool_struct.name,
            "description": tool_struct.description,
            "enabled": tool_struct.enabled,
            "tool_type": tool_struct.tool_type,
            "id": id,
        });

        // Add optional fields if they exist
        if let Some(entry_point) = &tool_struct.entry_point {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert("entry_point".to_string(), json!(entry_point));
            }
        }

        if let Some(configuration) = &tool_struct.configuration {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert(
                    "configuration".to_string(),
                    json!({
                        "command": configuration.command,
                        "args": configuration.args
                    }),
                );
            }
        }

        if let Some(distribution) = &tool_struct.distribution {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert("distribution".to_string(), distribution.clone());
            }
        }

        if let Some(config) = &tool_struct.config {
            if let Some(obj) = tool_value.as_object_mut() {
                let mut config_json = json!({});
                if let Some(env) = &config.env {
                    if let Some(config_obj) = config_json.as_object_mut() {
                        config_obj.insert("env".to_string(), json!(env));
                    }
                }
                if let Some(command) = &config.command {
                    if let Some(config_obj) = config_json.as_object_mut() {
                        config_obj.insert("command".to_string(), json!(command));
                    }
                }
                if let Some(args) = &config.args {
                    if let Some(config_obj) = config_json.as_object_mut() {
                        config_obj.insert("args".to_string(), json!(args));
                    }
                }
                obj.insert("config".to_string(), config_json);
            }
        }

        if let Some(authentication) = &tool_struct.authentication {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert("authentication".to_string(), authentication.clone());
            }
        }

        // Add process status - check the processes map
        if let Some(obj) = tool_value.as_object_mut() {
            let process_running = registry.processes.contains_key(id);
            obj.insert("process_running".to_string(), json!(process_running));

            // Add number of available tools from this server
            let server_tool_count = registry
                .server_tools
                .get(id)
                .map_or_else(|| 0, |tools| tools.len());
            obj.insert("tool_count".to_string(), json!(server_tool_count));
        }

        tools.push(tool_value);
    }
    Ok(tools)
}

/// List all available tools from all running MCP servers
pub async fn list_all_server_tools(mcp_state: &MCPState) -> Result<Vec<Value>, String> {
    let registry = mcp_state.tool_registry.read().await;
    let mut all_tools = Vec::new();

    for (server_id, tools) in &registry.server_tools {
        for tool in tools {
            // Extract the basic tool information
            let mut tool_info = serde_json::Map::new();

            // Copy the original tool properties
            if let Some(obj) = tool.as_object() {
                for (key, value) in obj {
                    tool_info.insert(key.clone(), value.clone());
                }
            }

            // Add server_id
            tool_info.insert("server_id".to_string(), json!(server_id));

            // Create a proxy ID
            let tool_id = tool.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let proxy_id = format!("{}:{}", server_id, tool_id);
            tool_info.insert("proxy_id".to_string(), json!(proxy_id));

            all_tools.push(json!(tool_info));
        }
    }

    Ok(all_tools)
}

/// Discover tools from a specific MCP server
pub async fn discover_tools(
    mcp_state: &MCPState,
    request: DiscoverServerToolsRequest,
) -> Result<DiscoverServerToolsResponse, String> {
    // Check if the server exists and is running
    let server_running = {
        let registry = mcp_state.tool_registry.read().await;
        registry
            .processes
            .get(&request.server_id)
            .is_some_and(|p| p.is_some())
    };

    if !server_running {
        return Ok(DiscoverServerToolsResponse {
            success: false,
            tools: None,
            error: Some(format!(
                "Server with ID '{}' is not running",
                request.server_id
            )),
        });
    }

    // Discover tools from the server
    let mut registry = mcp_state.tool_registry.write().await;
    match discover_server_tools(&request.server_id, &mut registry).await {
        Ok(tools) => {
            // Store the discovered tools
            registry
                .server_tools
                .insert(request.server_id.clone(), tools.clone());

            Ok(DiscoverServerToolsResponse {
                success: true,
                tools: Some(tools),
                error: None,
            })
        }
        Err(e) => Ok(DiscoverServerToolsResponse {
            success: false,
            tools: None,
            error: Some(format!("Failed to discover tools: {}", e)),
        }),
    }
}

/// Execute a tool from an MCP server
pub async fn execute_proxy_tool(
    mcp_state: &MCPState,
    request: ToolExecutionRequest,
) -> Result<ToolExecutionResponse, String> {
    // Extract server_id and tool_id from the proxy_id
    let parts: Vec<&str> = request.tool_id.split(':').collect();
    println!("parts: {:?}", parts);
    if parts.len() != 2 {
        return Err("Invalid tool_id format. Expected 'server_id:tool_id'".to_string());
    }

    let server_id = parts[0];
    println!("server_id: {}", server_id);
    let tool_id = parts[1];
    println!("tool_id: {}", tool_id);

    // Execute the tool on the server
    let mut registry = mcp_state.tool_registry.write().await;
    match execute_server_tool(
        server_id,
        tool_id,
        request.parameters.clone(),
        &mut registry,
    )
    .await
    {
        Ok(result) => Ok(ToolExecutionResponse {
            success: true,
            result: Some(result),
            error: None,
        }),
        Err(e) => Ok(ToolExecutionResponse {
            success: false,
            result: None,
            error: Some(e.to_string()),
        }),
    }
}

/// Update a tool's status (enabled/disabled)
pub async fn update_tool_status(
    mcp_state: &MCPState,
    request: ToolUpdateRequest,
) -> Result<ToolUpdateResponse, String> {
    // First, check if the tool exists and get the necessary information
    let tool_info = {
        let registry = mcp_state.tool_registry.read().await;

        if let Some(tool) = registry.tools.get(&request.tool_id) {
            // Extract and clone the necessary values
            let tool_type = tool.tool_type.clone();
            let entry_point = tool.entry_point.clone().unwrap_or_default();
            let process_running = registry
                .processes
                .get(&request.tool_id)
                .is_some_and(|p| p.is_some());

            Some((tool_type, entry_point, process_running))
        } else {
            None
        }
    };

    // If the tool doesn't exist, return an error
    if tool_info.is_none() {
        return Ok(ToolUpdateResponse {
            success: false,
            message: format!("Tool with ID '{}' not found", request.tool_id),
        });
    }

    // Now handle the process based on the enabled status
    let result = {
        let mut registry = mcp_state.tool_registry.write().await;

        // Update the enabled status in the tool definition
        if let Some(tool) = registry.tools.get_mut(&request.tool_id) {
            tool.enabled = request.enabled;
        }

        // Drop the write lock before trying to restart the tool
        drop(registry);

        if request.enabled {
            let mut registry = mcp_state.tool_registry.write().await;
            registry.restart_tool(&request.tool_id).await
        } else {
            Ok(())
        }
    };

    // Handle any errors from the process management
    if let Err(e) = result {
        return Ok(ToolUpdateResponse {
            success: false,
            message: e,
        });
    }

    // Save the updated state to the database
    if let Err(e) = ToolRegistry::save_mcp_state(mcp_state).await {
        error!("Failed to save MCP state after updating tool status: {}", e);
        // Continue even if saving fails
    } else {
        info!("Successfully saved MCP state after updating tool status");
    }

    // Return success
    Ok(ToolUpdateResponse {
        success: true,
        message: format!(
            "Tool '{}' status updated to {}",
            request.tool_id,
            if request.enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
    })
}

/// Update a tool's configuration (environment variables)
pub async fn update_tool_config(
    mcp_state: &MCPState,
    request: ToolConfigUpdateRequest,
) -> Result<ToolConfigUpdateResponse, String> {
    info!("Updating configuration for tool: {}", request.tool_id);

    // First, check if the tool exists
    let (tool_exists, is_enabled) = {
        let registry = mcp_state.tool_registry.read().await;
        let tool = registry.tools.get(&request.tool_id);
        let enabled = tool.map(|t| t.enabled).unwrap_or(false);
        (tool.is_some(), enabled)
    };

    // If the tool doesn't exist, return an error
    if !tool_exists {
        error!("Tool with ID '{}' not found", request.tool_id);
        return Ok(ToolConfigUpdateResponse {
            success: false,
            message: format!("Tool with ID '{}' not found", request.tool_id),
        });
    }

    info!("Tool '{}' found, enabled: {}", request.tool_id, is_enabled);

    // Update the tool configuration
    let mut registry = mcp_state.tool_registry.write().await;

    // Update the configuration in the tool definition
    if let Some(tool) = registry.tools.get_mut(&request.tool_id) {
        // Create or update the config object
        if tool.config.is_none() {
            tool.config = Some(ToolConfig {
                env: Some(HashMap::new()),
                command: None,
                args: None,
            });
        }

        if let Some(config) = &mut tool.config {
            // Create or update the env object
            if config.env.is_none() {
                config.env = Some(HashMap::new());
            }

            if let Some(env_map) = &mut config.env {
                // Update each environment variable
                if let Some(req_env) = &request.config.env {
                    for (key, value) in req_env {
                        info!(
                            "Setting environment variable for tool {}: {}={}",
                            request.tool_id, key, value
                        );
                        env_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }

    // Release the registry lock before saving state
    drop(registry);

    // Save the updated state to the database
    if let Err(e) = ToolRegistry::save_mcp_state(mcp_state).await {
        error!("Failed to save MCP state after updating tool config: {}", e);
        // Continue even if saving fails
    } else {
        info!(
            "Successfully saved MCP state after updating tool config for tool: {}",
            request.tool_id
        );
    }

    // Return success
    Ok(ToolConfigUpdateResponse {
        success: true,
        message: format!("Tool '{}' configuration updated", request.tool_id),
    })
}

/// Uninstall a registered tool
pub async fn uninstall_tool(
    mcp_state: &MCPState,
    request: ToolUninstallRequest,
) -> Result<ToolUninstallResponse, String> {
    let mut registry = mcp_state.tool_registry.write().await;

    // Kill the process if it's running
    if let Some(Some(process)) = registry.processes.get_mut(&request.tool_id) {
        if let Err(e) = kill_process(process).await {
            return Ok(ToolUninstallResponse {
                success: false,
                message: format!("Failed to kill process: {}", e),
            });
        }
    }

    // Remove the tool and process from the registry
    if registry.tools.remove(&request.tool_id).is_some() {
        registry.processes.remove(&request.tool_id);
        registry.server_tools.remove(&request.tool_id);

        Ok(ToolUninstallResponse {
            success: true,
            message: "Tool uninstalled successfully".to_string(),
        })
    } else {
        Ok(ToolUninstallResponse {
            success: false,
            message: format!("Tool with ID '{}' not found", request.tool_id),
        })
    }
}

/// Get all server data in a single function to avoid multiple locks
pub async fn get_all_server_data(mcp_state: &MCPState) -> Result<Value, String> {
    // Acquire a single read lock for all operations
    let registry = mcp_state.tool_registry.read().await;

    // 1. Get registered servers
    let mut servers = Vec::new();
    for (id, tool_struct) in registry.tools.iter() {
        // Convert the Tool struct to a Value
        let mut tool_value = json!({
            "name": tool_struct.name,
            "description": tool_struct.description,
            "enabled": tool_struct.enabled,
            "tool_type": tool_struct.tool_type,
            "id": id,
        });

        // Add optional fields if they exist
        if let Some(entry_point) = &tool_struct.entry_point {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert("entry_point".to_string(), json!(entry_point));
            }
        }

        if let Some(configuration) = &tool_struct.configuration {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert(
                    "configuration".to_string(),
                    json!({
                        "command": configuration.command,
                        "args": configuration.args
                    }),
                );
            }
        }

        if let Some(distribution) = &tool_struct.distribution {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert("distribution".to_string(), distribution.clone());
            }
        }

        if let Some(config) = &tool_struct.config {
            if let Some(obj) = tool_value.as_object_mut() {
                let mut config_json = json!({});
                if let Some(env) = &config.env {
                    if let Some(config_obj) = config_json.as_object_mut() {
                        config_obj.insert("env".to_string(), json!(env));
                    }
                }
                if let Some(command) = &config.command {
                    if let Some(config_obj) = config_json.as_object_mut() {
                        config_obj.insert("command".to_string(), json!(command));
                    }
                }
                if let Some(args) = &config.args {
                    if let Some(config_obj) = config_json.as_object_mut() {
                        config_obj.insert("args".to_string(), json!(args));
                    }
                }
                obj.insert("config".to_string(), config_json);
            }
        }

        if let Some(authentication) = &tool_struct.authentication {
            if let Some(obj) = tool_value.as_object_mut() {
                obj.insert("authentication".to_string(), authentication.clone());
            }
        }

        // Add process status - check the processes map
        if let Some(obj) = tool_value.as_object_mut() {
            let process_running = registry.processes.contains_key(id);
            obj.insert("process_running".to_string(), json!(process_running));

            // Add number of available tools from this server
            let server_tool_count = registry
                .server_tools
                .get(id)
                .map_or_else(|| 0, |tools| tools.len());
            obj.insert("tool_count".to_string(), json!(server_tool_count));
        }

        servers.push(tool_value);
    }

    // 2. Get all server tools
    let mut all_tools = Vec::new();
    for (server_id, tools) in &registry.server_tools {
        for tool in tools {
            // Extract the basic tool information
            let mut tool_info = serde_json::Map::new();

            // Copy the original tool properties
            if let Some(obj) = tool.as_object() {
                for (key, value) in obj {
                    tool_info.insert(key.clone(), value.clone());
                }
            }

            // Add server_id
            tool_info.insert("server_id".to_string(), json!(server_id));

            // Create a proxy ID
            let tool_id = tool.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let proxy_id = format!("{}:{}", server_id, tool_id);
            tool_info.insert("proxy_id".to_string(), json!(proxy_id));

            all_tools.push(json!(tool_info));
        }
    }

    // Return all data in one response
    Ok(json!({
        "servers": servers,
        "tools": all_tools
    }))
}

/// Save the current MCP state to the database
pub async fn save_mcp_state_command(mcp_state: &MCPState) -> Result<String, String> {
    match ToolRegistry::save_mcp_state(mcp_state).await {
        Ok(_) => Ok("MCP state saved successfully".to_string()),
        Err(e) => Err(format!("Failed to save MCP state: {}", e)),
    }
}

/// Load MCP state from the database
pub async fn load_mcp_state_command(mcp_state: &MCPState) -> Result<String, String> {
    match database::DatabaseManager::new() {
        Ok(db_manager) => {
            match db_manager.load_tool_registry() {
                Ok(registry) => {
                    // Update the tool registry with loaded data
                    let mut state_registry = mcp_state.tool_registry.write().await;
                    state_registry.tools = registry.tools;
                    state_registry.server_tools = registry.server_tools;
                    // Note: processes and process_ios are not persisted

                    Ok("MCP state loaded successfully".to_string())
                }
                Err(e) => Err(format!("Failed to load tool registry: {}", e)),
            }
        }
        Err(e) => Err(format!("Failed to initialize database: {}", e)),
    }
}

/// Check if the database exists and has data
pub async fn check_database_exists_command() -> Result<bool, String> {
    database::check_database_exists()
}

/// Clear all data from the database
pub async fn clear_database_command() -> Result<String, String> {
    let mut db_manager = database::DatabaseManager::new()?;
    match db_manager.clear_database() {
        Ok(_) => Ok("Database cleared successfully".to_string()),
        Err(e) => Err(format!("Failed to clear database: {}", e)),
    }
}

/// Restart a tool by its ID
pub async fn restart_tool_command(
    mcp_state: &MCPState,
    tool_id: String,
) -> Result<ToolUpdateResponse, String> {
    info!("Received request to restart tool: {}", tool_id);

    // Check if the tool exists
    let tool_exists = {
        let registry = mcp_state.tool_registry.read().await;
        registry.tools.contains_key(&tool_id)
    };

    if !tool_exists {
        error!("Tool with ID '{}' not found for restart", tool_id);
        return Ok(ToolUpdateResponse {
            success: false,
            message: format!("Tool with ID '{}' not found", tool_id),
        });
    }

    info!("Tool '{}' found, attempting to restart", tool_id);

    // Get a write lock on the registry to restart the tool
    let restart_result = {
        let mut registry = mcp_state.tool_registry.write().await;
        registry.restart_tool(&tool_id).await
    };

    match restart_result {
        Ok(_) => {
            info!("Successfully restarted tool: {}", tool_id);
            Ok(ToolUpdateResponse {
                success: true,
                message: format!("Tool '{}' restarted successfully", tool_id),
            })
        }
        Err(e) => {
            error!("Failed to restart tool {}: {}", tool_id, e);
            Ok(ToolUpdateResponse {
                success: false,
                message: format!("Failed to restart tool: {}", e),
            })
        }
    }
}
