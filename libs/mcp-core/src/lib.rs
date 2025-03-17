pub mod core;
pub mod database;
pub mod http_server;
pub mod mcp_installers;
pub mod mcp_state;
pub mod models;
pub mod registry;
pub mod schema;
pub mod spawned_process;
pub mod mcp_server;

// Re-export commonly used types and functions
pub use error::{MCPError, MCPResult};
pub use models::*;
pub use spawned_process::SpawnedProcess;
pub mod utils;

// pub use registry::ToolRegistry;

// Re-export key items for convenience
pub use mcp_server::{
    start_mcp_server,
    ClientManagerTrait,
    ClientManager,
};

// Re-export Tool from external mcp-core
pub use mcp_core::{Tool, ToolCall};

// Initialize logging
pub fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    log::info!("MCP Dockmaster Core library initialized");
}
