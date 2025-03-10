pub mod core;
pub mod database;
pub mod http_server;
pub mod mcp_installers;
mod mcp_protocol;
pub mod mcp_proxy;
pub mod mcp_state;
mod mcp_state;
pub mod models;
pub mod registry;
pub mod schema;
pub mod spawned_process;

// Re-export commonly used types and functions
pub use error::{MCPError, MCPResult};
pub use models::*;
pub use spawned_process::SpawnedProcess;
pub mod utils;

// pub use registry::ToolRegistry;

// Initialize logging
pub fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    log::info!("MCP Core library initialized");
}
