use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Which agents this server is assigned to: "claude_code", "codex", "gemini"
    #[serde(default)]
    pub agents: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct McpServerInput {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub agents: Vec<String>,
}

fn store_path(config_dir: &std::path::Path) -> std::path::PathBuf {
    config_dir.join("mcp_servers.json")
}

pub fn load(config_dir: &std::path::Path) -> Vec<McpServer> {
    let path = store_path(config_dir);
    if !path.exists() {
        return vec![];
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save(config_dir: &std::path::Path, servers: &[McpServer]) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let path = store_path(config_dir);
    let content = serde_json::to_string_pretty(servers).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

pub fn add(config_dir: &std::path::Path, input: McpServerInput) -> Result<McpServer, String> {
    let mut servers = load(config_dir);
    let server = McpServer {
        id: uuid_v4(),
        name: input.name,
        command: input.command,
        args: input.args,
        env: input.env,
        agents: input.agents,
    };
    servers.push(server.clone());
    save(config_dir, &servers)?;
    Ok(server)
}

pub fn remove(config_dir: &std::path::Path, id: &str) -> Result<(), String> {
    let mut servers = load(config_dir);
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
    let mut servers = load(config_dir);
    let server = servers
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("MCP server '{id}' not found"))?;
    server.name = input.name;
    server.command = input.command;
    server.args = input.args;
    server.env = input.env;
    server.agents = input.agents;
    let updated = server.clone();
    save(config_dir, &servers)?;
    Ok(updated)
}

/// Import MCP servers that exist in the coding agents' own config files
/// but are not yet tracked in our store.
pub fn import_from_agents(
    config_dir: &std::path::Path,
    home: &std::path::Path,
) -> Result<Vec<McpServer>, String> {
    let existing = load(config_dir);
    let mut imported: Vec<McpServer> = vec![];

    // Claude Code: ~/.claude/settings.json
    if let Some(servers) = read_claude_mcp(home) {
        for (name, entry) in servers {
            if existing.iter().any(|s| s.name == name) {
                continue;
            }
            imported.push(McpServer {
                id: uuid_v4(),
                name,
                command: entry.command,
                args: entry.args,
                env: entry.env,
                agents: vec!["claude_code".into()],
            });
        }
    }

    // Gemini: ~/.gemini/settings.json
    if let Some(servers) = read_gemini_mcp(home) {
        for (name, entry) in servers {
            if existing.iter().chain(imported.iter()).any(|s| s.name == name) {
                continue;
            }
            imported.push(McpServer {
                id: uuid_v4(),
                name,
                command: entry.command,
                args: entry.args,
                env: entry.env,
                agents: vec!["gemini".into()],
            });
        }
    }

    // Codex: ~/.codex/mcp.json
    if let Some(servers) = read_codex_mcp(home) {
        for (name, entry) in servers {
            if existing.iter().chain(imported.iter()).any(|s| s.name == name) {
                continue;
            }
            imported.push(McpServer {
                id: uuid_v4(),
                name,
                command: entry.command,
                args: entry.args,
                env: entry.env,
                agents: vec!["codex".into()],
            });
        }
    }

    if !imported.is_empty() {
        let mut all = existing;
        all.extend(imported.clone());
        save(config_dir, &all)?;
    }
    Ok(imported)
}

#[derive(Deserialize, Default)]
struct McpEntry {
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

fn read_claude_mcp(home: &std::path::Path) -> Option<HashMap<String, McpEntry>> {
    let path = home.join(".claude").join("settings.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = val.get("mcpServers")?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let Ok(e) = serde_json::from_value::<McpEntry>(v.clone()) {
            map.insert(k.clone(), e);
        }
    }
    Some(map)
}

fn read_gemini_mcp(home: &std::path::Path) -> Option<HashMap<String, McpEntry>> {
    let path = home.join(".gemini").join("settings.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = val.get("mcpServers")?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let Ok(e) = serde_json::from_value::<McpEntry>(v.clone()) {
            map.insert(k.clone(), e);
        }
    }
    Some(map)
}

fn read_codex_mcp(home: &std::path::Path) -> Option<HashMap<String, McpEntry>> {
    let path = home.join(".codex").join("mcp.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = val.get("mcpServers")?.as_object()?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let Ok(e) = serde_json::from_value::<McpEntry>(v.clone()) {
            map.insert(k.clone(), e);
        }
    }
    Some(map)
}

/// Build the mcpServers JSON object for writing into agent config files.
pub fn mcp_servers_json(
    servers: &[McpServer],
    agent: &str,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for s in servers {
        if s.agents.iter().any(|a| a == agent) {
            let mut entry = serde_json::Map::new();
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
            obj.insert(s.name.clone(), serde_json::Value::Object(entry));
        }
    }
    serde_json::Value::Object(obj)
}

fn uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("getrandom failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}
