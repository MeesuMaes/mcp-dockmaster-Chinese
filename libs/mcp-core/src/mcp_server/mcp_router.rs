use std::{future::Future, pin::Pin, sync::Arc};

use mcp_sdk_core::{
    handler::{PromptError, ResourceError},
    prompt::Prompt,
    protocol::ServerCapabilities,
    Content, Resource, Tool, ToolError,
};
use mcp_sdk_server::router::CapabilitiesBuilder;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use log::{info, error};

use crate::{
    core::mcp_core::MCPCore,
    core::mcp_core_proxy_ext::McpCoreProxyExt,
    models::types::{ServerToolsResponse, ServerRegistrationRequest, ToolExecutionRequest},
};

use super::tools::{TOOL_REGISTER_SERVER, get_register_server_tool};

/// MCP Router implementation for the Dockmaster server
/// This router handles all MCP protocol methods and integrates with the MCPCore
#[derive(Clone)]
pub struct MCPDockmasterRouter {
    mcp_core: MCPCore,
    server_name: String,
    version: String,
    tools_cache: Arc<RwLock<Vec<Tool>>>,
}

impl MCPDockmasterRouter {
    /// Create a new MCP router for the Dockmaster server
    pub fn new(mcp_core: MCPCore) -> Self {
        Self {
            mcp_core,
            server_name: "mcp-dockmaster-server".to_string(),
            version: "1.0.0".to_string(),
            tools_cache: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get all server tools
    async fn list_all_tools(&self) -> Result<Vec<Tool>, ToolError> {
        // Check the cache first
        {
            let cache = self.tools_cache.read().await;
            if !cache.is_empty() {
                return Ok(cache.clone());
            }
        }

        // Get user-installed tools from MCPCore
        match self.get_server_tools().await {
            Ok(response) => {
                let mut tools = Vec::new();
                
                // Add built-in tools
                tools.push(get_register_server_tool());
                
                // Add user-installed tools
                for tool_info in response.tools {
                    // Convert ServerToolInfo to Tool
                    if let Some(input_schema) = tool_info.input_schema {
                        // Convert InputSchema to serde_json::Value
                        let schema_value = json!({
                            "type": input_schema.r#type,
                            "properties": input_schema.properties,
                            "required": input_schema.required,
                        });
                        
                        let tool = Tool {
                            name: tool_info.name,
                            description: tool_info.description,
                            input_schema: schema_value,
                        };
                        
                        tools.push(tool);
                    } else {
                        // Create a tool with an empty schema
                        let tool = Tool {
                            name: tool_info.name,
                            description: tool_info.description,
                            input_schema: json!({
                                "type": "object",
                                "properties": {},
                                "required": []
                            }),
                        };
                        
                        tools.push(tool);
                    }
                }
                
                // Update the cache
                {
                    let mut cache = self.tools_cache.write().await;
                    *cache = tools.clone();
                }
                
                Ok(tools)
            },
            Err(error) => Err(ToolError::NotFound(format!("Failed to list tools: {}", error.message))),
        }
    }
    
    /// Get server tools using MCPCore
    async fn get_server_tools(&self) -> Result<ServerToolsResponse, crate::models::types::ErrorResponse> {
        // Get the installed tools from MCPCore
        let result = self.mcp_core.list_all_server_tools().await;

        match result {
            Ok(tools) => {
                // Use the existing ServerToolsResponse struct
                Ok(ServerToolsResponse {
                    tools: tools,
                })
            },
            Err(e) => Err(crate::models::types::ErrorResponse {
                code: -32000,
                message: format!("Failed to list tools: {}", e),
            }),
        }
    }
    
    /// Handle register_server tool
    async fn handle_register_server(&self, args: Value) -> Result<Value, ToolError> {
        let tool_id = args.get("tool_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::ExecutionError("Missing tool_id parameter".to_string()))?;
            
        info!("Registering server with tool ID: {}", tool_id);
        
        // Create a server registration request with a default configuration
        // to prevent panics during restart
        let request = ServerRegistrationRequest {
            server_id: tool_id.to_string(),
            server_name: format!("Imported server {}", tool_id),
            description: format!("Server imported via MCP API with ID {}", tool_id),
            tools_type: "unknown".to_string(), // This will be updated during the registry lookup
            configuration: Some(crate::models::types::ServerConfiguration {
                command: Some("echo".to_string()),  // A safe default command that won't do anything harmful
                args: Some(vec!["Server registration pending".to_string()]),
                env: Some(std::collections::HashMap::new()),
            }),
            distribution: None,
        };
        
        match self.mcp_core.register_server(request).await {
            Ok(response) => {
                if response.success {
                    // Clear the tools cache to force a refresh
                    {
                        let mut cache = self.tools_cache.write().await;
                        cache.clear();
                    }
                    
                    Ok(json!({
                        "success": true,
                        "message": response.message,
                        "tool_id": response.tool_id
                    }))
                } else {
                    Err(ToolError::ExecutionError(response.message))
                }
            },
            Err(e) => Err(ToolError::ExecutionError(format!("Failed to register server: {}", e))),
        }
    }
    
    /// Execute a tool by finding the appropriate server and forwarding the call
    async fn execute_tool(&self, tool_name: &str, args: Value) -> Result<Value, ToolError> {
        // Handle built-in tools first
        if tool_name == TOOL_REGISTER_SERVER {
            return self.handle_register_server(args).await;
        }
        
        // For non-built-in tools, use the MCPCore to find and execute
        // The tool_id format is expected to be server_id:tool_name
        let tool_id = format!("auto:{}", tool_name); // Use "auto" as a special server_id to let MCPCore find the right server
        
        let request = ToolExecutionRequest {
            tool_id,
            parameters: args,
        };
        
        match self.mcp_core.execute_proxy_tool(request).await {
            Ok(response) => {
                if response.success {
                    Ok(response.result.unwrap_or(json!(null)))
                } else {
                    Err(ToolError::ExecutionError(response.error.unwrap_or_else(|| "Unknown error".to_string())))
                }
            },
            Err(e) => Err(ToolError::ExecutionError(format!("Failed to execute tool: {}", e))),
        }
    }
}

impl mcp_sdk_server::Router for MCPDockmasterRouter {
    fn name(&self) -> String {
        self.server_name.clone()
    }

    fn instructions(&self) -> String {
        "This server provides tools for managing Docker containers, images, and networks. You can use it to manage containers, build images, and interact with Docker registries. It also allows you to register new MCP servers.".to_string()
    }

    fn capabilities(&self) -> ServerCapabilities {
        // Build capabilities with tools support
        CapabilitiesBuilder::new()
            .with_tools(false)
            .with_resources(false, false)
            .with_prompts(false)
            .build()
    }

    fn list_tools(&self) -> Vec<Tool> {
        // This is synchronous, so we need to use a synchronous approach instead of block_on
        // Return the register_server tool by default
        let mut tools = vec![get_register_server_tool()];
        
        // Get cached tools if available (using a std::sync Mutex instead of an async lock)
        let cache_handle = self.tools_cache.clone();
        
        // We can't use an async lock in a sync context
        // Return only the built-in tools and let the cache update happen elsewhere
        
        // Log that we're returning only built-in tools
        log::info!("Returning built-in tools from list_tools (synchronous context)");
        
        // Trigger an async task to update the cache for future calls
        let mcp_core = self.mcp_core.clone();
        let cache_clone = cache_handle.clone();
        
        // Spawn a task to update the cache for future requests
        tokio::spawn(async move {
            match mcp_core.list_all_server_tools().await {
                Ok(server_tools) => {
                    let mut tools_vec = Vec::new();
                    
                    // Add built-in tools
                    tools_vec.push(get_register_server_tool());
                    
                    // Add user-installed tools
                    for tool_info in server_tools {
                        // Convert ServerToolInfo to Tool
                        if let Some(input_schema) = tool_info.input_schema {
                            // Convert InputSchema to serde_json::Value
                            let schema_value = json!({
                                "type": input_schema.r#type,
                                "properties": input_schema.properties,
                                "required": input_schema.required,
                            });
                            
                            let tool = Tool {
                                name: tool_info.name,
                                description: tool_info.description,
                                input_schema: schema_value,
                            };
                            
                            tools_vec.push(tool);
                        } else {
                            // Create a tool with an empty schema
                            let tool = Tool {
                                name: tool_info.name,
                                description: tool_info.description,
                                input_schema: json!({
                                    "type": "object",
                                    "properties": {},
                                    "required": []
                                }),
                            };
                            
                            tools_vec.push(tool);
                        }
                    }
                    
                    // Update the cache
                    let mut cache = cache_clone.write().await;
                    *cache = tools_vec;
                    log::info!("Tools cache updated with {} tools", cache.len());
                },
                Err(e) => {
                    log::error!("Failed to update tools cache: {}", e);
                }
            }
        });
        
        tools
    }

    fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Content>, ToolError>> + Send + 'static>> {
        let this = self.clone();
        let tool_name = tool_name.to_string();

        Box::pin(async move {
            match this.execute_tool(&tool_name, arguments).await {
                Ok(result) => {
                    let result_str = serde_json::to_string_pretty(&result).unwrap_or_default();
                    Ok(vec![Content::text(result_str)])
                },
                Err(e) => Err(e),
            }
        })
    }

    fn list_resources(&self) -> Vec<Resource> {
        // No resources for now
        vec![]
    }

    fn read_resource(
        &self,
        uri: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ResourceError>> + Send + 'static>> {
        let uri = uri.to_string();
        Box::pin(async move {
            Err(ResourceError::NotFound(format!("Resource not found: {}", uri)))
        })
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        // No prompts for now
        vec![]
    }

    fn get_prompt(
        &self,
        prompt_name: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, PromptError>> + Send + 'static>> {
        let prompt_name = prompt_name.to_string();
        Box::pin(async move {
            Err(PromptError::NotFound(format!("Prompt not found: {}", prompt_name)))
        })
    }
} 