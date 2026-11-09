//! Importing MCP server definitions written by other agents.
//!
//! Different assistants store their MCP configuration under different shapes.
//! This module reads those files and turns them into lia's own
//! [`McpServerConfig`] so the servers can be connected to directly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::config::McpServerConfig;

/// Parse an MCP configuration file, detecting its format from the contents.
pub fn parse_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the MCP configuration {}", path.display()))?;
    parse_mcp_config_str(&text)
}

/// Parse MCP configuration from a string.
pub fn parse_mcp_config_str(text: &str) -> Result<HashMap<String, McpServerConfig>> {
    let value: Value =
        serde_json::from_str(text).context("the MCP configuration is not JSON")?;
    parse_mcp_config_value(&value)
}

/// Parse MCP configuration from a JSON value.
pub fn parse_mcp_config_value(value: &Value) -> Result<HashMap<String, McpServerConfig>> {
    let mut out = HashMap::new();
    let Some(root) = value.as_object() else {
        return Err(anyhow!("the MCP configuration is not an object"));
    };

    // opencode / openclaw nesting under "mcp"
    if let Some(mcp) = root.get("mcp") {
        if let Some(servers) = mcp.get("servers").and_then(Value::as_object) {
            add_server_map(&mut out, servers);
        } else if let Some(map) = mcp.as_object() {
            // opencode-style entries; only those that look like servers
            add_server_map_filtered(&mut out, map);
        }
    }

    // Common top-level keys.
    for key in &["mcpServers", "mcp_servers", "servers"] {
        if let Some(map) = root.get(*key).and_then(Value::as_object) {
            add_server_map(&mut out, map);
        }
    }

    // Fall back to treating the whole object as a server map if nothing else
    // matched and its values look like server definitions.
    if out.is_empty() {
        add_server_map_filtered(&mut out, root);
    }

    Ok(out)
}

/// Convenience alias.
pub fn load_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}

/// Convenience alias.
pub fn import_mcp_servers(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}

/// Convenience alias.
pub fn import_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}

/// Convenience alias.
pub fn mcp_servers_from_file(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}

/// Convenience alias.
pub fn mcp_servers_from_path(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}

/// Discover the MCP config files other agents keep in well-known places,
/// relative to a workspace and a home directory.
pub fn discover_mcp_servers(
    workspace: &Path,
    home: Option<&Path>,
) -> HashMap<String, McpServerConfig> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Workspace-local files.
    candidates.push(workspace.join(".mcp.json"));
    candidates.push(workspace.join("mcp.json"));
    candidates.push(workspace.join(".vscode").join("mcp.json"));
    candidates.push(workspace.join("opencode.json"));

    if let Some(home) = home {
        candidates.push(home.join(".mcp.json"));
        candidates.push(home.join(".copilot").join("mcp-config.json"));
        candidates.push(home.join(".config").join("opencode").join("opencode.json"));
        candidates.push(home.join(".openclaw").join("openclaw.json"));
        candidates.push(home.join(".claude.json"));
        candidates.push(home.join(".cursor").join("mcp.json"));
        candidates.push(home.join(".cline").join("mcp.json"));
        candidates.push(home.join(".roo").join("mcp.json"));
    }

    let mut merged = HashMap::new();
    for path in candidates {
        if path.is_file() {
            if let Ok(servers) = parse_mcp_config(&path) {
                for (name, cfg) in servers {
                    merged.entry(name).or_insert(cfg);
                }
            }
        }
    }
    merged
}

/// Format-specific helpers. These all delegate to the auto-detecting parser,
/// which is tolerant of the shapes each agent uses.
pub fn parse_claude_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}
pub fn parse_copilot_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}
pub fn parse_vscode_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}
pub fn parse_opencode_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}
pub fn parse_openclaw_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}
pub fn parse_generic_mcp_config(path: &Path) -> Result<HashMap<String, McpServerConfig>> {
    parse_mcp_config(path)
}

// --- internals ---

fn add_server_map(out: &mut HashMap<String, McpServerConfig>, map: &serde_json::Map<String, Value>) {
    for (name, value) in map {
        if let Ok(cfg) = convert_server(name, value) {
            out.insert(name.clone(), cfg);
        }
    }
}

fn add_server_map_filtered(
    out: &mut HashMap<String, McpServerConfig>,
    map: &serde_json::Map<String, Value>,
) {
    for (name, value) in map {
        if is_server_spec(value) {
            if let Ok(cfg) = convert_server(name, value) {
                out.insert(name.clone(), cfg);
            }
        }
    }
}

fn is_server_spec(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.contains_key("command")
        || obj.contains_key("args")
        || obj.contains_key("url")
        || obj.contains_key("type")
        || obj.contains_key("transport")
        || obj.contains_key("environment")
        || obj.contains_key("env")
        || obj.contains_key("headers")
        || obj.contains_key("cwd")
        || obj.contains_key("enabled")
        || obj.contains_key("disabled")
}

fn convert_server(name: &str, value: &Value) -> Result<McpServerConfig> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("the server '{name}' is not an object"))?;

    let mut cfg = McpServerConfig::default();

    // enabled / disabled
    if let Some(enabled) = get_bool(obj, &["enabled"]) {
        cfg.enabled = enabled;
    }
    if matches!(get_bool(obj, &["disabled"]), Some(true)) {
        cfg.enabled = false;
    }

    // url
    if let Some(url) = get_str(obj, &["url", "address", "endpoint"]) {
        cfg.url = Some(expand_env(&url));
    }

    // command (string or array) + args
    let mut args: Vec<String> = get_string_array(obj, &["args", "arguments"]).unwrap_or_default();
    if let Some(cmd) = obj.get("command") {
        match cmd {
            Value::String(s) => {
                cfg.command = Some(expand_env(s));
            }
            Value::Array(arr) => {
                let mut parts: Vec<String> = arr
                    .iter()
                    .filter_map(Value::as_str)
                    .map(expand_env)
                    .collect();
                if !parts.is_empty() {
                    let first = parts.remove(0);
                    cfg.command = Some(first);
                    // prepend remaining parts to args
                    parts.append(&mut args);
                    args = parts;
                }
            }
            _ => {}
        }
    }

    // append explicit args after any command-array parts already merged above
    if let Some(extra) = get_string_array(obj, &["args", "arguments"]) {
        if cfg.command.is_some() && args.len() == extra.len() {
            // args already contains them from command array handling; nothing
        } else if !extra.is_empty() && args.is_empty() {
            args = extra;
        }
    }
    cfg.args = args;

    // env / environment
    for key in &["env", "environment", "envVars", "env_vars"] {
        if let Some(map) = obj.get(*key).and_then(Value::as_object) {
            for (k, v) in map {
                let val = match v {
                    Value::String(s) => expand_env(s),
                    other => other.to_string(),
                };
                cfg.env.insert(k.clone(), val);
            }
        }
    }

    // headers
    for key in &["headers", "http_headers"] {
        if let Some(map) = obj.get(*key).and_then(Value::as_object) {
            for (k, v) in map {
                let val = match v {
                    Value::String(s) => expand_env(s),
                    other => other.to_string(),
                };
                cfg.headers.insert(k.clone(), val);
            }
        }
    }

    // cwd
    if let Some(cwd) = get_str(obj, &["cwd", "working_dir", "working_directory", "workdir"]) {
        cfg.cwd = Some(PathBuf::from(expand_env(&cwd)));
    }

    // timeout
    for key in &["timeoutSecs", "timeout_secs", "timeoutSeconds", "timeout"] {
        if let Some(n) = obj.get(*key).and_then(Value::as_u64).or_else(|| {
            obj.get(*key)
                .and_then(Value::as_f64)
                .map(|f| f as u64)
        }) {
            if n > 0 {
                cfg.timeout_secs = n;
            }
        }
    }
    for key in &["timeoutMs", "timeout_ms", "connectionTimeoutMs", "connection_timeout_ms"] {
        if let Some(n) = obj.get(*key).and_then(Value::as_u64).or_else(|| {
            obj.get(*key)
                .and_then(Value::as_f64)
                .map(|f| f as u64)
        }) {
            if n > 0 {
                cfg.timeout_secs = ((n + 999) / 1000).max(1);
            }
        }
    }

    // transport
    let type_str = get_str(obj, &["type", "transport", "protocol"]).unwrap_or_default();
    let type_lower = type_str.to_ascii_lowercase();
    let transport = if type_lower.contains("sse") {
        crate::config::McpTransport::Sse
    } else if type_lower.contains("stdio")
        || type_lower == "local"
        || (cfg.url.is_none() && !type_lower.contains("http") && !type_lower.contains("remote"))
    {
        crate::config::McpTransport::Stdio
    } else if type_lower.contains("http") || type_lower.contains("remote") || cfg.url.is_some() {
        crate::config::McpTransport::StreamableHttp
    } else if cfg.url.is_some() {
        crate::config::McpTransport::StreamableHttp
    } else {
        cfg.transport
    };
    cfg.transport = transport;

    if cfg.transport == crate::config::McpTransport::Stdio && cfg.command.is_none() {
        // nothing to run; still return but connection will fail later
    }

    Ok(cfg)
}

fn get_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(Value::String(s)) = obj.get(*key) {
            return Some(s.clone());
        }
    }
    None
}

fn get_bool(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(Value::Bool(b)) = obj.get(*key) {
            return Some(*b);
        }
    }
    None
}

fn get_string_array(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<Vec<String>> {
    for key in keys {
        if let Some(Value::Array(arr)) = obj.get(*key) {
            return Some(
                arr
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect(),
            );
        }
    }
    None
}

fn expand_env(s: &str) -> String {
    let mut out = s.to_string();
    // ${VAR}
    while let Some(start) = out.find("${") {
        if let Some(rel) = out[start..].find('}') {
            let close = start + rel;
            let var = &out[start + 2..close];
            let repl = std::env::var(var).unwrap_or_default();
            out.replace_range(start..=close, &repl);
        } else {
            break;
        }
    }
    out
}

// Convenience reexports happen in mod.rs.
