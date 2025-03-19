use super::mcp_core::MCPCore;
use crate::{
    mcp_installers::{
        get_claude_config, get_cursor_config, get_generic_config, install_claude, install_cursor,
        is_claude_installed, is_cursor_installed,
    },
    utils::process::{is_process_running, restart_process},
};

pub trait McpCoreInstallersExt {
    fn is_claude_installed(&self) -> Result<bool, String>;
    fn install_claude(&self) -> Result<(), String>;
    fn is_cursor_installed(&self, after_0470: bool) -> Result<bool, String>;
    fn install_cursor(&self, after_0470: bool) -> Result<(), String>;
    fn get_claude_config(&self) -> Result<String, String>;
    fn get_cursor_config(&self, after_0470: bool) -> Result<String, String>;
    fn get_generic_config(&self) -> Result<String, String>;
    fn restart_process(&self, process_name: &str) -> Result<bool, String>;
    fn is_process_running(&self, process_name: &str) -> Result<bool, String>;
}

impl McpCoreInstallersExt for MCPCore {
    fn is_process_running(&self, process_name: &str) -> Result<bool, String> {
        Ok(is_process_running(process_name))
    }
    fn restart_process(&self, process_name: &str) -> Result<bool, String> {
        restart_process(process_name)
    }

    fn is_claude_installed(&self) -> Result<bool, String> {
        match is_claude_installed(&self.app_name) {
            Ok(_) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }
    fn install_claude(&self) -> Result<(), String> {
        let Some(proxy_server_binary_path) = self.proxy_server_binary_path.to_str() else {
            return Err("failed to convert path to string".to_string());
        };
        match install_claude(&self.app_name, proxy_server_binary_path) {
            Ok(_) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
    fn is_cursor_installed(&self, after_0470: bool) -> Result<bool, String> {
        match is_cursor_installed(&self.app_name, after_0470) {
            Ok(_) => Ok(true),
            Err(err) => Err(err.to_string()),
        }
    }
    fn install_cursor(&self, after_0470: bool) -> Result<(), String> {
        let Some(proxy_server_binary_path) = self.proxy_server_binary_path.to_str() else {
            return Err("failed to convert path to string".to_string());
        };
        match install_cursor(&self.app_name, proxy_server_binary_path, after_0470) {
            Ok(_) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
    fn get_claude_config(&self) -> Result<String, String> {
        let Some(proxy_server_binary_path) = self.proxy_server_binary_path.to_str() else {
            return Err("failed to convert path to string".to_string());
        };
        match get_claude_config(&self.app_name, proxy_server_binary_path) {
            Ok(config) => Ok(config),
            Err(err) => Err(err.to_string()),
        }
    }
    fn get_cursor_config(&self, after_0470: bool) -> Result<String, String> {
        let Some(proxy_server_binary_path) = self.proxy_server_binary_path.to_str() else {
            return Err("failed to convert path to string".to_string());
        };
        match get_cursor_config(&self.app_name, proxy_server_binary_path, after_0470) {
            Ok(config) => Ok(config),
            Err(err) => Err(err.to_string()),
        }
    }

    fn get_generic_config(&self) -> Result<String, String> {
        let Some(proxy_server_binary_path) = self.proxy_server_binary_path.to_str() else {
            return Err("failed to convert path to string".to_string());
        };
        Ok(get_generic_config(proxy_server_binary_path))
    }
}
