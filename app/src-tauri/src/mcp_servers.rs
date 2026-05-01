use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    #[default]
    Http,
    Stdio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub transport: McpTransport,
    // stdio fields
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    // http fields
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Which agents this server is assigned to: "claude_code", "codex", "gemini"
    #[serde(default)]
    pub agents: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct McpServerInput {
    pub name: String,
    #[serde(default)]
    pub transport: McpTransport,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

fn store_path(config_dir: &std::path::Path) -> std::path::PathBuf {
    config_dir.join("mcp_servers.json")
}

/// Returns Ok(empty) when the file doesn't exist, Err when it exists but is unreadable/corrupt.
pub fn load(config_dir: &std::path::Path) -> Result<Vec<McpServer>, String> {
    let path = store_path(config_dir);
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| format!("mcp_servers.json is corrupted: {e}"))
}

pub fn save(config_dir: &std::path::Path, servers: &[McpServer]) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let path = store_path(config_dir);
    let content = serde_json::to_string_pretty(servers).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

pub fn add(config_dir: &std::path::Path, input: McpServerInput) -> Result<McpServer, String> {
    let mut servers = load(config_dir)?;
    if servers.iter().any(|s| s.name == input.name) {
        return Err(format!("MCP server '{}' already exists", input.name));
    }
    let server = McpServer {
        id: uuid_v4()?,
        name: input.name,
        transport: input.transport,
        command: input.command,
        args: input.args,
        env: input.env,
        url: input.url,
        headers: input.headers,
        agents: input.agents,
    };
    servers.push(server.clone());
    save(config_dir, &servers)?;
    Ok(server)
}

pub fn remove(config_dir: &std::path::Path, id: &str) -> Result<(), String> {
    let mut servers = load(config_dir)?;
    let before = servers.len();
    servers.retain(|s| s.id != id);
    if servers.len() == before {
        return Err(format!("MCP server '{id}' not found"));
    }
    save(config_dir, &servers)
}

pub fn update(
    config_dir: &std::path::Path,
    id: &str,
    input: McpServerInput,
) -> Result<McpServer, String> {
    let mut servers = load(config_dir)?;
    // Enforce unique names, excluding the server being updated
    if servers.iter().any(|s| s.name == input.name && s.id != id) {
        return Err(format!("MCP server '{}' already exists", input.name));
    }
    let server = servers
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("MCP server '{id}' not found"))?;
    server.name = input.name;
    server.transport = input.transport;
    server.command = input.command;
    server.args = input.args;
    server.env = input.env;
    server.url = input.url;
    server.headers = input.headers;
    server.agents = input.agents;
    let updated = server.clone();
    save(config_dir, &servers)?;
    Ok(updated)
}

/// Import MCP servers from agent config files, merging agent assignments for duplicate names.
pub fn import_from_agents(
    config_dir: &std::path::Path,
    home: &std::path::Path,
) -> Result<Vec<McpServer>, String> {
    let mut existing = load(config_dir)?;
    let mut newly_added: Vec<McpServer> = vec![];
    let mut modified_existing = false;

    let sources: Vec<(&str, Option<HashMap<String, McpEntry>>)> = vec![
        ("claude_code", read_claude_mcp(home)),
        ("gemini", read_gemini_mcp(home)),
        ("codex", read_codex_mcp(home)),
    ];

    for (agent_key, maybe_map) in sources {
        let Some(map) = maybe_map else { continue };
        for (name, entry) in map {
            // Check if name already tracked in existing store — merge agents
            if let Some(s) = existing.iter_mut().find(|s| s.name == name) {
                if !s.agents.iter().any(|a| a == agent_key) {
                    s.agents.push(agent_key.to_string());
                    modified_existing = true;
                }
                continue;
            }
            // Check if already in newly_added — merge agents
            if let Some(s) = newly_added.iter_mut().find(|s| s.name == name) {
                if !s.agents.iter().any(|a| a == agent_key) {
                    s.agents.push(agent_key.to_string());
                }
                continue;
            }
            newly_added.push(entry_to_server(name, entry, agent_key.to_string())?);
        }
    }

    if !newly_added.is_empty() || modified_existing {
        let mut all = existing;
        all.extend(newly_added.clone());
        save(config_dir, &all)?;
    }
    Ok(newly_added)
}

fn entry_to_server(name: String, entry: McpEntry, agent: String) -> Result<McpServer, String> {
    Ok(McpServer {
        id: uuid_v4()?,
        name,
        transport: entry.transport,
        command: entry.command,
        args: entry.args,
        env: entry.env,
        url: entry.url,
        headers: entry.headers,
        agents: vec![agent],
    })
}

#[derive(Deserialize, Default)]
struct McpEntry {
    #[serde(rename = "type", default)]
    transport_type: Option<String>,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(skip)]
    transport: McpTransport,
}

impl McpEntry {
    fn resolve_transport(mut self) -> Self {
        self.transport = match self.transport_type.as_deref() {
            Some("http") => McpTransport::Http,
            _ => McpTransport::Stdio, // no type field (or legacy "sse") means stdio
        };
        self
    }
}

fn read_mcp_map(path: std::path::PathBuf) -> Option<HashMap<String, McpEntry>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = val.get("mcpServers")?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let Ok(e) = serde_json::from_value::<McpEntry>(v.clone()) {
            map.insert(k.clone(), e.resolve_transport());
        }
    }
    Some(map)
}

fn read_claude_mcp(home: &std::path::Path) -> Option<HashMap<String, McpEntry>> {
    read_mcp_map(home.join(".claude").join("settings.json"))
}

fn read_gemini_mcp(home: &std::path::Path) -> Option<HashMap<String, McpEntry>> {
    read_mcp_map(home.join(".gemini").join("settings.json"))
}

fn read_codex_mcp(home: &std::path::Path) -> Option<HashMap<String, McpEntry>> {
    read_mcp_map(home.join(".codex").join("mcp.json"))
}

/// Build the mcpServers JSON object for writing into agent config files.
pub fn mcp_servers_json(servers: &[McpServer], agent: &str) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for s in servers {
        if !s.agents.iter().any(|a| a == agent) {
            continue;
        }
        let mut entry = serde_json::Map::new();
        match s.transport {
            McpTransport::Http => {
                entry.insert("type".into(), serde_json::Value::String("http".into()));
                entry.insert("url".into(), serde_json::Value::String(s.url.clone()));
                if !s.headers.is_empty() {
                    let h: serde_json::Map<_, _> = s
                        .headers
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    entry.insert("headers".into(), serde_json::Value::Object(h));
                }
            }
            McpTransport::Stdio => {
                entry.insert("command".into(), serde_json::Value::String(s.command.clone()));
                entry.insert(
                    "args".into(),
                    serde_json::Value::Array(
                        s.args.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
                    ),
                );
                if !s.env.is_empty() {
                    let env_obj: serde_json::Map<_, _> = s
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    entry.insert("env".into(), serde_json::Value::Object(env_obj));
                }
            }
        }
        obj.insert(s.name.clone(), serde_json::Value::Object(entry));
    }
    serde_json::Value::Object(obj)
}

fn uuid_v4() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("RNG failure: {e}"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ))
}
