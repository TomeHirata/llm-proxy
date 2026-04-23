# llmproxy

Localhost LLM proxy — OpenAI-compatible API, no Python required.

Point any OpenAI SDK at `http://localhost:8080/v1` and route to OpenAI,
Anthropic, Gemini, AWS Bedrock, Azure OpenAI, Mistral, or TogetherAI by
prefixing the model with a provider name:

```python
from openai import OpenAI
client = OpenAI(base_url="http://localhost:8080/v1", api_key="")
client.chat.completions.create(
    model="anthropic/claude-sonnet-4-5",
    messages=[{"role": "user", "content": "hello"}],
)
```

No config file is required. API keys can be read from standard environment
variables (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, …) or passed per-request in
the `Authorization: Bearer …` header. A YAML config file (§ Config below) is
supported if you'd rather keep keys out of your shell environment.

## Supported providers (v0.1)

| Provider key   | Transport                                   | Credential source                                   |
|----------------|---------------------------------------------|-----------------------------------------------------|
| `openai`       | passthrough                                 | `OPENAI_API_KEY` / header / config                  |
| `azure`        | passthrough (`api-key` header, per-deploy)  | `endpoint` + `api_version` + key in config          |
| `mistral`      | passthrough                                 | `MISTRAL_API_KEY` / header / config                 |
| `togetherai`   | passthrough                                 | `TOGETHERAI_API_KEY` / header / config              |
| `anthropic`    | translation + SSE                           | `ANTHROPIC_API_KEY` / header / config               |
| `gemini`       | translation + SSE                           | `GEMINI_API_KEY` / header / config                  |
| `bedrock`      | Converse API, SigV4-signed                  | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` + `AWS_REGION` |

All providers support both non-streaming and streaming (`stream: true`).

## Install

### macOS (universal binary — Intel + Apple Silicon)

```bash
curl -L https://github.com/TomeHirata/llm-proxy/releases/latest/download/llmproxy-$(git ls-remote --tags https://github.com/TomeHirata/llm-proxy | awk -F/ '{print $NF}' | grep '^v' | sort -V | tail -1)-universal-apple-darwin.tar.gz \
  | tar -xz
sudo mv llmproxy-*/llmproxy /usr/local/bin/llmproxy
# Clear Gatekeeper quarantine if prompted on first run:
xattr -d com.apple.quarantine /usr/local/bin/llmproxy
```

Or with a pinned version:

```bash
VERSION=v0.1.0
curl -L "https://github.com/TomeHirata/llm-proxy/releases/download/${VERSION}/llmproxy-${VERSION}-universal-apple-darwin.tar.gz" \
  | tar -xz
sudo mv "llmproxy-${VERSION}-universal-apple-darwin/llmproxy" /usr/local/bin/llmproxy
xattr -d com.apple.quarantine /usr/local/bin/llmproxy
```

### Debian / Ubuntu (APT)

```bash
# Add the signing key
curl -fsSL https://tomehirata.github.io/llm-proxy/apt/pubkey.asc \
  | sudo gpg --dearmor -o /etc/apt/keyrings/llmproxy.gpg

# Add the repository (replace <codename> with bookworm / trixie / jammy / noble)
echo "deb [signed-by=/etc/apt/keyrings/llmproxy.gpg] \
  https://tomehirata.github.io/llm-proxy/apt <codename> main" \
  | sudo tee /etc/apt/sources.list.d/llmproxy.list

sudo apt-get update
sudo apt-get install llmproxy
```

### Linux (binary tarball)

```bash
VERSION=v0.1.0
# For x86_64:
curl -L "https://github.com/TomeHirata/llm-proxy/releases/download/${VERSION}/llmproxy-${VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
  | tar -xz
sudo mv "llmproxy-${VERSION}-x86_64-unknown-linux-gnu/llmproxy" /usr/local/bin/llmproxy
# For arm64: replace x86_64-unknown-linux-gnu with aarch64-unknown-linux-gnu
```

### macOS desktop app (menu-bar UI)

Requires Node.js 18+ and Rust (install via [rustup](https://rustup.rs)).

```bash
git clone https://github.com/TomeHirata/llm-proxy
cd llm-proxy

# 1. Build the CLI binary and place it as a sidecar inside the app bundle
./scripts/prepare-sidecar.sh --release

# 2. Build the .app bundle
cd app
npm install
npm run tauri build

# 3. Install
cp -r src-tauri/target/release/bundle/macos/llmproxy.app /Applications/

# 4. Clear Gatekeeper quarantine (required until the app is code-signed)
xattr -dr com.apple.quarantine /Applications/llmproxy.app

# 5. Launch — the app appears in your menu bar
open /Applications/llmproxy.app
```

The menu-bar icon lets you start/stop the proxy and open the dashboard
(usage stats + API key config). No separate `llmproxy serve` needed.

**Dev mode** (hot-reload):
```bash
cd app
npm run tauri dev   # runs prepare-sidecar (debug build) + Vite + Tauri
```

### From source (CLI only)

```bash
cargo build --release -p llmproxy-server
./target/release/llmproxy serve
```

## Usage

```bash
llmproxy serve                 # foreground, default 127.0.0.1:8080
llmproxy serve --port 9000     # custom port
llmproxy serve --daemon        # fork, write PID to ~/.local/share/llmproxy/llmproxy.pid
llmproxy stop                  # SIGTERM the daemon
llmproxy status                # is the daemon alive?
llmproxy providers             # show which providers have credentials
llmproxy test anthropic        # send a hello ping to a provider
llmproxy install               # register launchd agent (macOS) or systemd user unit (Linux)
llmproxy uninstall             # remove the autostart service
llmproxy config init           # scaffold ~/.config/llmproxy/config.yaml
llmproxy config show           # print resolved config with secrets redacted
llmproxy usage summary         # aggregate stats from the persistent log
llmproxy usage recent          # most recent log entries
llmproxy usage prune           # one-shot retention cleanup
```

### Usage log

Enabled by default. Every request is persisted — provider, model, status,
latency, token counts, and truncated request/response bodies — into a local
SQLite database at `~/.local/share/llmproxy/usage.sqlite`. The `Authorization`
header is never recorded. Rows older than `retention_days` (default 30) are
pruned hourly.

```bash
llmproxy usage summary --since 7d
llmproxy usage recent --limit 50 --verbose
```

To disable, set `usage_log.enabled: false` in config:

```yaml
usage_log:
  enabled: false
```

### Routing

Every request's `model` field is parsed as `provider/model_id` on the first
`/`. Model IDs containing slashes — e.g. Bedrock cross-region ARNs like
`us.anthropic.claude-3-5-sonnet-20241022-v2:0` — are preserved verbatim.

| `model` field                           | Provider  | Upstream model id                   |
|-----------------------------------------|-----------|-------------------------------------|
| `openai/gpt-4o`                         | OpenAI    | `gpt-4o`                            |
| `anthropic/claude-sonnet-4-5`           | Anthropic | `claude-sonnet-4-5`                 |
| `gemini/gemini-2.5-flash`               | Gemini    | `gemini-2.5-flash`                  |
| `bedrock/amazon.nova-pro-v1:0`          | Bedrock   | `amazon.nova-pro-v1:0`              |
| `azure/my-gpt4-deployment`              | Azure     | `my-gpt4-deployment` (deployment)   |
| `mistral/mistral-large-latest`          | Mistral   | `mistral-large-latest`              |

### Credential resolution

Per-request, highest priority first:

1. `Authorization: Bearer <token>` header (not applicable to Bedrock)
2. `providers.<name>.api_key` from the config file
3. Well-known environment variable: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
   `GEMINI_API_KEY`, `MISTRAL_API_KEY`, `TOGETHERAI_API_KEY`,
   `AZURE_OPENAI_API_KEY`, or AWS credentials for Bedrock

If none resolve, the proxy returns `401 Unauthorized`.

## Config

The config file is entirely optional. Search order (first hit wins):

1. `--config <path>` CLI flag
2. `$LLMPROXY_CONFIG` env var
3. `~/.config/llmproxy/config.yaml`
4. `./llmproxy.yaml`

`${ENV_VAR}` interpolation is supported in YAML values.

```yaml
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
```

See `config.example.yaml` for the full schema.

## Recipes

### Claude Code

Route Claude Code through the proxy to track usage and latency:

```bash
llmproxy serve --daemon
export ANTHROPIC_BASE_URL="http://localhost:8080/anthropic"
claude
```

Then inspect what was sent:

```bash
llmproxy usage summary
llmproxy usage recent --verbose
```

### Cursor / VS Code Copilot

```bash
export OPENAI_BASE_URL="http://localhost:8080/openai/v1"
```

## Endpoints

| Method | Path                                            | Notes                                          |
|--------|-------------------------------------------------|------------------------------------------------|
| POST   | `/v1/chat/completions`                          | Unified OpenAI shape; `stream: true` uses SSE  |
| GET    | `/v1/models`                                    | Lists configured provider keys                 |
| GET    | `/health`                                       | Returns `ok`                                   |
| POST   | `/openai/v1/responses`                          | OpenAI Responses API passthrough               |
| POST   | `/anthropic/v1/messages`                        | Anthropic Messages API passthrough             |
| POST   | `/gemini/v1beta/models/:model/generateContent`  | Gemini generateContent passthrough             |

## Project layout

```
crates/
├── llmproxy-core/       # OpenAI types, Provider trait, Credential, errors
├── llmproxy-providers/  # Passthrough, Anthropic, Gemini, Bedrock implementations
└── llmproxy-server/     # Axum server, config, registry, CLI
```

Dependency direction: `server → providers → core`.

## Development

```bash
cargo test                          # unit tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --release -p llmproxy-server
```

## Roadmap

- **v0.1** (this release) — OpenAI, Anthropic, Gemini, Bedrock, Azure, Mistral,
  TogetherAI. Chat + streaming (all providers). Daemon + autostart on macOS/Linux.
- **v0.2** — Cohere, HuggingFace TGI, `/v1/embeddings`.
- **v0.3** — MLflow Model Serving, AI21Labs.
- **v1.0** — JSONL request log, full tool-call passthrough for translation
  providers.
