<h1 align="center">io</h1>

<p align="center">
  <strong>AI coding agent for the terminal</strong><br>
  <em>Built in Rust — powered by 13 LLM providers</em>
</p>

<p align="center">
  <img src="assets/io.png" width="600" alt="io terminal agent" />
</p>

<p align="center">
  <code>io</code> is an AI coding assistant that runs directly in your terminal.
  It reads, writes, edits, and understands code — with support for <strong>13 LLM providers</strong>,
  interactive sessions, tool execution, and permission sandboxing.
</p>

---

## Features

- **🤖 Multi-provider** — 13 LLM providers supported (Anthropic, OpenAI, Gemini, Groq, DeepSeek, Mistral, Ollama, Azure, Bedrock, OpenRouter, xAI, OpenCode Go, OpenCode Zen)
- **🛠️ 7 built-in tools** — `read`, `write`, `edit`, `bash`, `glob`, `grep`, `spawn_agent` for full codebase interaction
- **� Cost tracking** — Built-in API cost calculation with `/cost` command for supported providers
- **�💬 Interactive & single-shot modes** — REPL for conversation, or `io "do this"` for one-off tasks
- **🔐 Permission sandbox** — Allow/deny/prompt modes for command execution control
- **💾 Session persistence** — SQLite-backed conversation history, resume anytime
- **📡 Streaming responses** — Real-time token-by-token streaming with live indicator
- **⚡ Fast & lightweight** — Built in Rust, minimal dependencies, no Node.js or Python required
- **🔄 Provider switching** — Change providers on the fly with `/connect` and `/model`
- **📁 Project-level config** — Per-project `.io/config.toml` initialization with `io init`
- **🎨 Interactive picker** — Arrow-key provider/model selector with viewport scrolling
- **📋 Context-aware** — Full conversation history management with turn-level tool call tracking

## Quick Start

```bash
# Install
cargo install --path .

# Start interactive mode
io

# Or run a single command
io "explain this codebase"
```

On first run, `io` creates a default configuration at `~/.io/config.toml`.
Use `/connect` inside the REPL to set up your preferred LLM provider.

## Modes

### Interactive REPL

```bash
io
```

Launches an interactive session with a streaming agent loop. Commands:

| Command | Description |
|---|---|
| `/help` | Show available commands |
| `/connect` | Set up a provider interactively (with live model fetching) |
| `/model` | Switch between configured providers |
| `/cost` | Show API cost summary for the current session |
| `/exit`, `/quit`, `/q` | Exit the session |
| `!<cmd>` | Run a shell command directly |

In interactive mode, tool calls are visualized inline:
- Tool start events show the tool name and arguments
- Tool completion shows diffs for `write`/`edit` with syntax-colored diff output
- Text streams token-by-token with a blinking cursor indicator

### Single-shot

```bash
io "summarize the changes in src/"
```

Runs one turn and prints the response, then exits.

### Flags

```bash
io --new              # Start a fresh session (ignore history)
io --continue         # Resume the last session
io --model anthropic  # Override the default provider
```

## Supported Providers

| Provider | Config key | Default Model |
|---|---|---|
| **Anthropic** | `anthropic` | `claude-sonnet-4-20250514` |
| **OpenAI** | `openai` | `gpt-4o` |
| **Google Gemini** | `gemini` | `gemini-2.5-pro` |
| **Groq** | `groq` | `llama-3.3-70b-versatile` |
| **Ollama** | `ollama` | `llama3.2` |
| **Azure OpenAI** | `azure` | `gpt-4o` |
| **AWS Bedrock** | `bedrock` | `anthropic.claude-3-5-sonnet-20241022-v2:0` |
| **Mistral AI** | `mistral` | `mistral-large-latest` |
| **DeepSeek** | `deepseek` | `deepseek-chat` |
| **OpenRouter** | `openrouter` | `anthropic/claude-sonnet-4` |
| **xAI (Grok)** | `xai` | `grok-3-beta` |
| **OpenCode Go** | `opencode_go` | `deepseek-v3` |
| **OpenCode Zen** | `opencode_zen` | `opencode/claude-sonnet-4` |

## Configuration

Configuration is stored in `~/.io/config.toml` (global) and optionally `.io/config.toml` (per-project).

```toml
[provider]
default = "anthropic"

[provider.anthropic]
model = "claude-sonnet-4-20250514"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"

[session]
auto_compact = true
memory_enabled = true
max_turns = 100

[permissions]
default = "prompt"       # "allow" | "deny" | "prompt"
allowed_commands = []
denied_commands = ["rm", "sudo"]
```

### API Keys

Keys are stored in `~/.io/keys.toml` (with `chmod 600` on Unix). You can also use environment variables:

```toml
# ~/.io/keys.toml
anthropic = "sk-ant-..."
openai = "sk-proj-..."
```

Or set `api_key_env` in `config.toml` to reference an environment variable.

### Commands

```bash
# View current config
io config show

# Modify config
io config set provider.default anthropic
io config set session.auto_compact true
io config set permissions.default allow

# Initialize io in the current project
io init
```

### Session Management

```bash
# List sessions
io session list

# Show session details
io session show <id>

# Delete a session
io session delete <id>
```

## Architecture

```
io/
├── io/                          # CLI frontend (binary crate)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs              # Entry point, CLI parsing, subcommand dispatch
│       ├── connect.rs           # Interactive provider setup wizard (13 providers)
│       ├── model.rs             # Provider switching (/model command)
│       └── picker.rs            # Terminal interactive picker (arrow keys, viewport)
│
├── io-runtime/                  # Core engine (library crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs               # Crate root — re-exports public API
│       ├── agent.rs             # Agent loop: LLM completion + tool execution (sync & streaming)
│       ├── config.rs            # Config/schema (TOML), KeyStore, provider config structs
│       ├── types.rs             # Core data types — Session, Turn, ToolCallRecord, TurnUsage
│       ├── memory.rs            # SQLite-backed session persistence (CRUD)
│       ├── sandbox.rs           # Permission checker (allow/deny/prompt modes)
│       ├── pricing.rs           # Per-token cost calculation for supported providers
│       ├── tools/               # Built-in tools (6 tools, each with unit tests)
│       │   ├── mod.rs           # Tool trait, ToolRegistry, default_registry()
│       │   ├── read.rs          # Read files with offset/limit
│       │   ├── write.rs         # Write/create files, returns unified diff
│       │   ├── edit.rs          # Replace first occurrence of text, returns unified diff
│       │   ├── bash.rs          # Execute shell commands with timeout & workdir
│       │   ├── glob.rs          # Find files by glob pattern (sorted by mtime)
│       │   └── grep.rs          # Search file contents with regex
│       │
│       └── provider/            # 13 LLM provider implementations
│           ├── mod.rs           # ProviderKind enum, CompletionModel trait, create_provider()
│           ├── anthropic.rs     # Anthropic Claude
│           ├── openai.rs        # OpenAI & compatible APIs (core implementation)
│           ├── gemini.rs        # Google Gemini
│           ├── groq.rs          # Groq
│           ├── ollama.rs        # Ollama (local)
│           ├── azure.rs         # Azure OpenAI
│           ├── bedrock.rs       # AWS Bedrock
│           ├── mistral.rs       # Mistral AI
│           ├── deepseek.rs      # DeepSeek
│           ├── openrouter.rs    # OpenRouter
│           ├── xai.rs           # xAI (Grok)
│           ├── opencode_go.rs   # OpenCode Go
│           └── opencode_zen.rs  # OpenCode Zen
│
├── .github/
│   └── workflows/
│       └── ci.yml               # CI: fmt, clippy, build, test on push/PR
├── assets/
│   └── io.png                   # Logo
├── Cargo.toml                   # Workspace root (resolver = "3")
└── README.md
```

### How It Works

1. **Input** — User types a prompt (interactive or single-shot)
2. **Agent loop** — The agent iterates up to 20 turns: sends conversation history + system prompt + tool specs to the LLM
3. **Tool execution** — If the LLM requests a tool call, the agent executes it via `ToolRegistry`, checks permissions via `PermissionChecker`, and feeds results back to the model
4. **Streaming** — In interactive mode, text deltas stream to the terminal as they arrive; tool starts and completions are rendered inline (with syntax-colored diffs for `write`/`edit`)
5. **Persistence** — Each turn (with tool call records and token usage) is saved to SQLite for session resumption

### Message Flow

```
User Input
    │
    ▼
Agent.run_turn() / run_turn_streaming()
    │
    ├── Build Messages (system prompt + history + new input)
    │
    ├── CompletionModel.complete() / complete_stream()
    │       │
    │       ├── Text blocks → stream to user / accumulate
    │       └── ToolUse blocks → execute tools
    │               │
    │               ├── PermissionChecker.check_tool()
    │               ├── ToolRegistry.get().execute()
    │               └── Feed ToolResult back to model
    │
    └── Save Turn to SQLite (SessionStore)
```

## Built-in Tools

| Tool | Description |
|---|---|
| **`read`** | Read files with optional `offset` (line number) and `limit` for partial reading |
| **`write`** | Write or create a file. Returns a unified diff of what changed |
| **`edit`** | Replace the first occurrence of `old_string` with `new_string` in a file. Returns a unified diff |
| **`bash`** | Execute shell commands with configurable `timeout` (ms) and `workdir` |
| **`glob`** | Find files by glob pattern, sorted by modification time (newest first) |
| **`grep`** | Search file contents with regex, optional file type filtering via `include` glob |

All tools implement the `Tool` trait with a JSON input schema exposed to the LLM, allowing autonomous discovery and use of parameters.

## Development

```bash
# Build
cargo build

# Run in development
cargo run -- "your prompt"
cargo run --

# Run tests (43 unit tests across tools, sandbox, config, pricing)
cargo test

# Add a new provider
# 1. Add a config struct in io-runtime/src/config.rs
# 2. Create a provider module in io-runtime/src/provider/
# 3. Add it to ProviderKind enum and create_provider() in provider/mod.rs
# 4. Add it to PROVIDERS array and match block in connect.rs
```

### Adding a New Tool

1. Create `io-runtime/src/tools/<name>.rs` implementing the `Tool` trait
2. Register it in `tools/mod.rs` (add module + register in `default_registry()`)
3. The LLM will discover it automatically via tool specs

## License

MIT
