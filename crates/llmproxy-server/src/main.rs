mod config;
mod registry;
mod server;
mod usage_log;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{bail, Context};
use chrono::Utc;
use clap::{Parser, Subcommand};
use llmproxy_core::{
    openai_types::ChatMessage, openai_types::ChatRequest, openai_types::MessageContent,
};

use crate::{
    config::{load_config, AppConfig, UsageLogConfig},
    registry::ProviderRegistry,
    server::{router, AppState},
    usage_log::{parse_since, UsageStore},
};

#[derive(Parser)]
#[command(
    name = "llmproxy",
    about = "Localhost LLM API proxy (OpenAI-compatible)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the proxy server in the foreground (or as a daemon with --daemon).
    Serve(ServeArgs),
    /// Stop the running daemon.
    Stop,
    /// Show whether the daemon is running.
    Status,
    /// List configured providers.
    Providers(ConfigArgs),
    /// Send a hello ping to the given provider.
    Test {
        provider: String,
        #[arg(long)]
        config: Option<String>,
    },
    /// Install as a user-level autostart service.
    Install(ConfigArgs),
    /// Remove the autostart service.
    Uninstall,
    /// Config helpers.
    #[command(subcommand)]
    Config(ConfigSub),
    /// Query the persistent usage log (requires `usage_log.enabled: true`).
    #[command(subcommand)]
    Usage(UsageSub),
}

#[derive(Subcommand)]
enum UsageSub {
    /// Aggregate counts, success rate, latency, and token totals.
    Summary {
        #[arg(long)]
        config: Option<String>,
        /// How far back to summarize (e.g. `7d`, `24h`, `30m`). Default: 7d.
        #[arg(long, default_value = "7d")]
        since: String,
    },
    /// Print the most recent N log entries.
    Recent {
        #[arg(long)]
        config: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Also print raw request and response bodies.
        #[arg(long, short)]
        verbose: bool,
    },
    /// Run a one-shot retention prune now and exit.
    Prune {
        #[arg(long)]
        config: Option<String>,
    },
}

#[derive(Parser)]
struct ServeArgs {
    #[arg(long)]
    config: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    host: Option<String>,
    /// Fork to background and write a PID file.
    #[arg(long)]
    daemon: bool,
}

#[derive(Parser)]
struct ConfigArgs {
    #[arg(long)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum ConfigSub {
    /// Create a default config.yaml if one does not exist.
    Init,
    /// Print the loaded config with secrets redacted.
    Show {
        #[arg(long)]
        config: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    // `.env` is optional.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    match cli.command {
        Command::Serve(args) => serve(args),
        Command::Stop => stop_daemon(),
        Command::Status => status_daemon(),
        Command::Providers(args) => list_providers(args.config.as_deref()),
        Command::Test { provider, config } => cmd_test(&provider, config.as_deref()),
        Command::Install(args) => install(args.config.as_deref()),
        Command::Uninstall => uninstall(),
        Command::Config(sub) => match sub {
            ConfigSub::Init => config_init(),
            ConfigSub::Show { config } => config_show(config.as_deref()),
        },
        Command::Usage(sub) => match sub {
            UsageSub::Summary { config, since } => usage_summary(config.as_deref(), &since),
            UsageSub::Recent { config, limit, verbose } => usage_recent(config.as_deref(), limit, verbose),
            UsageSub::Prune { config } => usage_prune(config.as_deref()),
        },
    }
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
}

fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let mut cfg = load_config(args.config.as_deref())?;
    if let Some(h) = args.host {
        cfg.server.host = h;
    }
    if let Some(p) = args.port {
        cfg.server.port = p;
    }

    if args.daemon {
        run_as_daemon(cfg)
    } else {
        init_tracing();
        tokio_runtime()?.block_on(run_server(cfg))
    }
}

fn tokio_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

async fn run_server(cfg: AppConfig) -> anyhow::Result<()> {
    let registry = Arc::new(ProviderRegistry::from_config(&cfg));
    let usage_store = open_usage_store_for(&cfg.usage_log)?;
    if let Some(store) = &usage_store {
        spawn_retention_task(store.clone(), cfg.usage_log.retention_days);
        tracing::info!("usage log: enabled ({})", store.path().display());
    }
    let state = AppState {
        registry,
        usage_store,
        max_body_bytes: cfg.usage_log.max_body_bytes,
    };
    let app = router(state);

    // Binding via (host, port) handles IPv4, IPv6, and hostnames — unlike
    // `format!("{host}:{port}")`, which would produce an invalid string for
    // an IPv6 host like `::1`.
    let listener =
        tokio::net::TcpListener::bind((cfg.server.host.as_str(), cfg.server.port)).await?;
    let local = listener.local_addr()?;
    tracing::info!("llmproxy listening on http://{local}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn open_usage_store_for(cfg: &UsageLogConfig) -> anyhow::Result<Option<UsageStore>> {
    if !cfg.enabled {
        return Ok(None);
    }
    let path = cfg
        .path
        .clone()
        .unwrap_or_else(|| data_dir().join("usage.sqlite"));
    Ok(Some(UsageStore::open(&path)?))
}

fn spawn_retention_task(store: UsageStore, retention_days: u32) {
    let retention = Duration::from_secs(retention_days as u64 * 86_400);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match store.prune(retention).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("usage log: pruned {n} rows older than {retention_days}d"),
                Err(e) => tracing::warn!("usage log prune failed: {e}"),
            }
        }
    });
}

fn run_as_daemon(cfg: AppConfig) -> anyhow::Result<()> {
    use daemonize::Daemonize;

    let data = data_dir();
    std::fs::create_dir_all(&data)?;
    let pid_file = data.join("llmproxy.pid");
    let log_file = data.join("llmproxy.log");

    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)?;
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)?;

    Daemonize::new()
        .pid_file(&pid_file)
        .chown_pid_file(false)
        .working_directory(&data)
        .stdout(stdout)
        .stderr(stderr)
        .start()
        .context("failed to daemonize")?;

    init_tracing();
    tokio_runtime()?.block_on(run_server(cfg))
}

fn stop_daemon() -> anyhow::Result<()> {
    let pid_file = data_dir().join("llmproxy.pid");
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .with_context(|| format!("read {}", pid_file.display()))?
        .trim()
        .parse()
        .context("pid file is not an integer")?;

    unsafe {
        if libc::kill(pid, libc::SIGTERM) != 0 {
            bail!(
                "kill({}, SIGTERM) failed: {}",
                pid,
                std::io::Error::last_os_error()
            );
        }
    }
    let _ = std::fs::remove_file(&pid_file);
    println!("Sent SIGTERM to {pid}");
    Ok(())
}

fn status_daemon() -> anyhow::Result<()> {
    let pid_file = data_dir().join("llmproxy.pid");
    let pid: Option<i32> = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse().ok());

    match pid {
        Some(p) if unsafe { libc::kill(p, 0) } == 0 => {
            println!("running — pid {p}");
        }
        Some(p) => {
            println!("stale pid file for {p} (process not running)");
        }
        None => println!("not running"),
    }
    Ok(())
}

fn list_providers(config_path: Option<&str>) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let registry = ProviderRegistry::from_config(&cfg);
    println!("Configured providers:");
    for (name, configured) in registry.configured_names() {
        let mark = if configured { "✓" } else { "✗" };
        let note = if configured {
            format!("(use \"{name}/<model_id>\")")
        } else {
            format!(
                "(not configured — set {} or pass Authorization header)",
                match name.as_str() {
                    "openai" => "OPENAI_API_KEY",
                    "anthropic" => "ANTHROPIC_API_KEY",
                    "gemini" => "GEMINI_API_KEY",
                    "mistral" => "MISTRAL_API_KEY",
                    "togetherai" => "TOGETHERAI_API_KEY",
                    "bedrock" => "AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY + AWS_REGION",
                    "azure" => "azure.endpoint + azure.api_version + azure.api_key in config",
                    _ => "credentials",
                }
            )
        };
        println!("  {name:<12} {mark}  {note}");
    }
    Ok(())
}

fn cmd_test(provider: &str, config_path: Option<&str>) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let registry = ProviderRegistry::from_config(&cfg);
    let rt = tokio_runtime()?;

    let model_id = default_test_model(provider)?;
    let (p, m, cred) = registry
        .resolve(&format!("{provider}/{model_id}"), None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let req = ChatRequest {
        model: format!("{provider}/{model_id}"),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: MessageContent::Text("Say hi in one word.".into()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        stream: None,
        temperature: None,
        max_tokens: Some(32),
        top_p: None,
        stop: None,
        tools: None,
        tool_choice: None,
        response_format: None,
        extra: Default::default(),
    };

    let resp = rt
        .block_on(p.chat(req, &m, &cred))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("✓ {} / {} responded", provider, m);
    println!("  {}", resp.choices[0].message.content.as_text());
    Ok(())
}

fn default_test_model(provider: &str) -> anyhow::Result<&'static str> {
    Ok(match provider {
        "openai" => "gpt-4o-mini",
        "anthropic" => "claude-3-5-haiku-latest",
        "gemini" => "gemini-2.5-flash",
        "mistral" => "mistral-small-latest",
        "togetherai" => "meta-llama/Llama-3-8b-chat-hf",
        "bedrock" => "amazon.nova-lite-v1:0",
        other => bail!("no default test model for provider '{}'", other),
    })
}

fn install(config_path: Option<&str>) -> anyhow::Result<()> {
    let config_path = config_path
        .map(String::from)
        .or_else(|| {
            dirs::home_dir().map(|h| {
                h.join(".config/llmproxy/config.yaml")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .unwrap_or_else(|| "/etc/llmproxy/config.yaml".into());

    #[cfg(target_os = "macos")]
    return install_macos(&config_path);
    #[cfg(target_os = "linux")]
    return install_linux(&config_path);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = config_path;
        bail!("autostart is only supported on macOS and Linux")
    }
}

fn uninstall() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return uninstall_macos();
    #[cfg(target_os = "linux")]
    return uninstall_linux();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("autostart is only supported on macOS and Linux")
}

#[cfg(target_os = "macos")]
fn install_macos(config_path: &str) -> anyhow::Result<()> {
    let binary = std::env::current_exe()?;
    let log = data_dir().join("llmproxy.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.llmproxy</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>serve</string>
        <string>--config</string>
        <string>{config_path}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>"#,
        binary = binary.display(),
        log = log.display(),
    );
    let plist_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no HOME"))?
        .join("Library/LaunchAgents/com.llmproxy.plist");
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist_path, plist)?;
    std::process::Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()?;
    println!("✓ llmproxy installed as a launchd agent.");
    println!("  Plist: {}", plist_path.display());
    println!("  Logs:  {}", log.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_macos() -> anyhow::Result<()> {
    let plist_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no HOME"))?
        .join("Library/LaunchAgents/com.llmproxy.plist");
    std::process::Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path)
        .status()
        .ok();
    let _ = std::fs::remove_file(&plist_path);
    println!("✓ Autostart removed.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_linux(config_path: &str) -> anyhow::Result<()> {
    let binary = std::env::current_exe()?;
    let log = data_dir().join("llmproxy.log");
    let unit = format!(
        "[Unit]\nDescription=llmproxy — local LLM API proxy\nAfter=network.target\n\n\
[Service]\nExecStart={binary} serve --config {config_path}\nRestart=on-failure\nRestartSec=5\n\
StandardOutput=append:{log}\nStandardError=append:{log}\n\n\
[Install]\nWantedBy=default.target\n",
        binary = binary.display(),
        log = log.display(),
    );

    let unit_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no HOME"))?
        .join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir)?;
    let unit_path = unit_dir.join("llmproxy.service");
    std::fs::write(&unit_path, unit)?;

    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .ok();
    std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "llmproxy"])
        .status()
        .ok();

    println!("✓ llmproxy enabled as a systemd user service.");
    println!("  Unit: {}", unit_path.display());
    println!("  View logs: journalctl --user -u llmproxy -f");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_linux() -> anyhow::Result<()> {
    std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "llmproxy"])
        .status()
        .ok();
    let unit_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no HOME"))?
        .join(".config/systemd/user/llmproxy.service");
    let _ = std::fs::remove_file(&unit_path);
    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .ok();
    println!("✓ Autostart removed.");
    Ok(())
}

fn config_init() -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?;
    let path = home.join(".config/llmproxy/config.yaml");
    if path.exists() {
        println!("Config already exists at {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, DEFAULT_CONFIG)?;
    println!("✓ Wrote default config to {}", path.display());
    Ok(())
}

fn config_show(path: Option<&str>) -> anyhow::Result<()> {
    let cfg = load_config(path)?;
    let redacted = crate::config::redacted(&cfg);
    println!("{}", serde_yaml::to_string(&redacted)?);
    Ok(())
}

fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/llmproxy")
}

fn open_store_or_error(cfg: &AppConfig) -> anyhow::Result<UsageStore> {
    if !cfg.usage_log.enabled {
        bail!("usage log is disabled — set usage_log.enabled: true in config");
    }
    let path = cfg
        .usage_log
        .path
        .clone()
        .unwrap_or_else(|| data_dir().join("usage.sqlite"));
    if !path.exists() {
        bail!(
            "usage log database not found at {} — start the server first so it can be created",
            path.display()
        );
    }
    UsageStore::open(&path)
}

fn usage_summary(config_path: Option<&str>, since: &str) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let since_dur = parse_since(since)?;
    let rt = tokio_runtime()?;
    rt.block_on(async move {
        let store = open_store_or_error(&cfg)?;
        let start = Utc::now() - since_dur;
        let (rows, totals) = store.summary(start).await?;

        println!("Usage since {} ({})", start.to_rfc3339(), since);
        println!();
        println!(
            "  {:<12} {:<36} {:>8} {:>8} {:>9} {:>9} {:>9} {:>10} {:>10}",
            "PROVIDER", "MODEL", "COUNT", "OK", "AVG_MS", "P50_MS", "P95_MS", "PROMPT_T", "COMPL_T"
        );
        for r in &rows {
            println!(
                "  {:<12} {:<36} {:>8} {:>8} {:>9.1} {:>9} {:>9} {:>10} {:>10}",
                r.provider,
                truncate(&r.model_id, 36),
                r.count,
                r.success_count,
                r.avg_latency_ms,
                r.p50_latency_ms,
                r.p95_latency_ms,
                r.prompt_tokens,
                r.completion_tokens,
            );
        }
        println!();
        println!(
            "  Total requests: {} ({} successful, {:.1}% success)",
            totals.count,
            totals.success_count,
            if totals.count == 0 {
                0.0
            } else {
                100.0 * totals.success_count as f64 / totals.count as f64
            },
        );
        println!(
            "  Tokens: {} prompt + {} completion = {} total",
            totals.prompt_tokens,
            totals.completion_tokens,
            totals.prompt_tokens + totals.completion_tokens
        );
        anyhow::Ok(())
    })
}

fn usage_recent(config_path: Option<&str>, limit: usize, verbose: bool) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let rt = tokio_runtime()?;
    rt.block_on(async move {
        let store = open_store_or_error(&cfg)?;
        let rows = store.recent(limit).await?;
        println!(
            "  {:<26} {:<10} {:<30} {:>6} {:>8} {:>10} {:>10}",
            "TIMESTAMP", "PROVIDER", "MODEL", "STATUS", "LAT_MS", "PROMPT_T", "COMPL_T"
        );
        for e in rows {
            println!(
                "  {:<26} {:<10} {:<30} {:>6} {:>8} {:>10} {:>10}",
                e.created_at.to_rfc3339(),
                e.provider,
                truncate(&e.model_id, 30),
                e.status,
                e.latency_ms,
                e.prompt_tokens.map(|v| v.to_string()).unwrap_or_default(),
                e.completion_tokens
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            );
            if verbose {
                let req = serde_json::from_str::<serde_json::Value>(&e.request_body)
                    .map(|v| serde_json::to_string_pretty(&v).unwrap_or(e.request_body.clone()))
                    .unwrap_or(e.request_body.clone());
                let resp = serde_json::from_str::<serde_json::Value>(&e.response_body)
                    .map(|v| serde_json::to_string_pretty(&v).unwrap_or(e.response_body.clone()))
                    .unwrap_or(e.response_body.clone());
                println!("  request:  {req}");
                println!("  response: {resp}");
                println!();
            }
        }
        anyhow::Ok(())
    })
}

fn usage_prune(config_path: Option<&str>) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let retention = Duration::from_secs(cfg.usage_log.retention_days as u64 * 86_400);
    let rt = tokio_runtime()?;
    rt.block_on(async move {
        let store = open_store_or_error(&cfg)?;
        let n = store.prune(retention).await?;
        println!(
            "pruned {n} rows older than {}d",
            cfg.usage_log.retention_days
        );
        anyhow::Ok(())
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

const DEFAULT_CONFIG: &str = r#"# ~/.config/llmproxy/config.yaml
server:
  host: 127.0.0.1
  port: 8080

providers:
  openai:
    api_key: ${OPENAI_API_KEY}
  anthropic:
    api_key: ${ANTHROPIC_API_KEY}
  gemini:
    api_key: ${GEMINI_API_KEY}
  mistral:
    api_key: ${MISTRAL_API_KEY}
  bedrock:
    region: us-east-1
  azure:
    api_key: ${AZURE_OPENAI_API_KEY}
    endpoint: https://my-resource.openai.azure.com
    api_version: "2024-02-01"

# Persistent request/response log. Opt-in: captured prompts/responses can
# contain sensitive content, so keep this disabled unless you need it.
usage_log:
  enabled: false
  retention_days: 30
  # path: ${HOME}/.local/share/llmproxy/usage.sqlite
"#;
