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

- **Multi-provider** — 13 LLM providers supported (Anthropic, OpenAI, Gemini, Groq, DeepSeek, Mistral, Ollama, Azure, Bedrock, OpenRouter, xAI, OpenCode Go, OpenCode Zen)
- **7 built-in tools** — `read`, `write`, `edit`, `bash`, `glob`, `grep`, `spawn_agent` for full codebase interaction
- **Cost tracking** — Built-in API cost calculation with `/cost` command for supported providers
- **Interactive & single-shot modes** — Full-screen REPL with splash screen, prompt bar, and scrollback
- **Color themes** — 12 built-in themes (dark/light) with live `/theme` switcher
- **Permission sandbox** — Allow/deny/prompt modes with granular bash command control and security-hardened command validation
- **Session persistence** — SQLite-backed conversation history, resume anytime
- **Streaming responses** — Real-time token-by-token streaming with live indicator
- **Fast & lightweight** — Built in Rust, minimal dependencies, no Node.js or Python required
- **Provider switching** — Change providers on the fly with `/connect` and `/model`
- **Project-level config** — Per-project `.io/config.toml` initialization with `io init`
- **Interactive picker** — Arrow-key menus for agents, providers, models, and themes
- **Agent cycling** — Tab key at empty prompt to swap agents mid-session
- **@file mentions** — Type `@path/to/file` to inline file or directory contents
- **Project context** — Reads `AGENTS.md` / `CLAUDE.md` from the project root and injects them as context so agents understand your conventions automatically
- **Smart write permissions** — Most agents (build, debug, docs, refactor, …) auto-approve `write`/`edit` calls; plan, git, review, security, and test always prompt

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

When a new `io` release adds config options, your existing config file is
auto-upgraded on load: missing keys are filled in from defaults, your values
are preserved, and OAuth tokens (`~/.io/oauth.toml`) and API keys
(`~/.io/keys.toml`) are never touched.

## Security

The permission sandbox system provides defense-in-depth protection against command injection and unauthorized execution:

**Permission Modes**:
- `allow` — Auto-approve all tool executions
- `agent` — Agent decides (default): read-only and caution-level commands run without asking; destructive and network commands still prompt. Sub-agent spawns are delegated to the agent (sub-agents inherit the same checker and fail closed). The user's `allowed_commands`/`denied_commands` always win.
- `prompt` — Ask user for approval for anything beyond read-only commands (strict)
- `deny` — Block all tool executions

```bash
# Switch modes
io config set permissions.default agent     # agent decides (default)
io config set permissions.default prompt    # strict: ask for anything non-read-only
io config set permissions.default allow     # fully autonomous
io config set permissions.default deny      # deny everything

# Allow/deny specific commands (coordinated with every mode, persisted in config)
io config set permissions.allowed_commands '["ls", "git", "cargo", "curl"]'
io config set permissions.denied_commands '["rm", "sudo"]'

# Opt into read-only network fetches in agent mode (curl to stdout, wget -O-).
# File-writing, upload, and custom-method network commands still prompt.
io config set permissions.allow_network_fetch true
```

**Security Features**:
- **Semantic command analyzer**: Classifies every bash command as `Safe`, `Caution`, or `Destructive` before execution — `ls`, `git status`, `cargo check` are auto-allowed; `rm -rf`, `dd`, `git reset --hard` always prompt
- **Granular bash approvals**: "Always" approvals are command-specific (approving `ls -la` won't approve `rm -rf`)
- **Command normalization**: Handles path obfuscation (`/bin/rm` → `rm`, `r\m` → `rm`)
- **Injection prevention**: Detects command substitution (`$(rm)`, `` `rm` ``), chaining (`;`, `&`, `|`), and subshells
- **Conservative denylist**: Any dangerous token denies the entire command
- **Strict allowlist**: Requires all pipeline heads to be explicitly allowed
- **Expansion guard**: Commands containing `$`, backtick, or `(` bypass auto-allow (static analysis can't classify them safely)
- **Privilege escalation gated**: `sudo`/`su`/`pkexec` always prompt unless explicitly allowlisted, and `sudo` is analyzed by the command it elevates — `sudo rm -rf /` is destructive, never hidden behind `sudo`

**Command Safety Classifications**:
| Level | Examples | Prompt mode | Agent mode |
|---|---|---|---|
| `Safe` | `ls`, `cat`, `grep`, `git status`, `cargo check`, `find` (no `-delete`) | Auto-allowed | Auto-allowed |
| `Caution` | `mv`, `git commit`, `rm file.txt`, `sed -i`, `chmod`, `mkdir` | Prompts | Auto-allowed (agent decides) |
| `Destructive` | `rm -rf`, `dd`, `git reset --hard`, `git push --force`, `mkfs.*` | Prompts | Prompts |
| Network | `curl`, `wget`, `ssh`, `scp` | Prompts | Prompts (unless allowlisted, or read-only fetch with `allow_network_fetch = true`) |
| Privilege | `sudo`, `su`, `pkexec` | Prompts | Prompts (unless allowlisted) |

**Ecosystem-agnostic build tool classification** — subcommands are classified consistently across all package managers:

| Subcommand | Ecosystems | Level |
|---|---|---|
| `test`, `build`, `check`, `lint`, `fmt`, `doc`, `bench`, `audit` | npm/yarn/pnpm/bun, cargo, go, pip, mvn/gradle, dotnet, … | `Safe` — auto-allowed |
| `list`, `show`, `freeze`, `outdated`, `tree`, `info` | pip, npm, cargo, go mod | `Safe` — auto-allowed |
| `install`, `add`, `update`, `upgrade`, `remove` | all | `Caution` — prompts |
| `publish`, `deploy`, `release` | npm, cargo, mvn | `Caution` — prompts |
| `run <script>` | npm/yarn/pnpm/bun | Safe or Caution based on script name |
| `go mod download/verify/graph/why` | go | `Safe` — read-only module ops |
| `go mod tidy/edit` | go | `Caution` — rewrites go.sum |

`make`/`cmake`/`ninja` targets are always `Caution` — Makefile rules are opaque to static analysis.

**Defended Attack Vectors**:
- Path obfuscation, command substitution, environment injection
- Command chaining with `;`, `&`, `|`, `&&`, `||`
- Subshells, brace groups, HEREDOC, process substitution

## Modes

### Interactive REPL

```bash
io
```

Launches an interactive session with a streaming agent loop. Commands:

| Command | Description |
|---|---|---|
| `/help` | Show available commands |
| `/new` | Start a new session |
| `/agent` | Switch agent mode (build, plan, debug, refactor) |
| `/connect` | Set up a provider interactively (with live model fetching) |
| `/login` | Sign in with OAuth (ChatGPT / Claude) |
| `/model` | Switch between configured providers |
| `/theme` | Switch UI color theme (12 themes) |
| `/cost` | Show API cost summary for the current session |
| `/compact` | Summarize and compress conversation history |
| `/exit`, `/quit`, `/q` | Exit the session |
| `!<cmd>` | Run a shell command directly |

Tab key at the empty prompt cycles through available full agents.
Type `@path/to/file` to expand file or directory contents inline.

The TUI features a splash screen with centered logo and command reference,
a fixed prompt bar at the bottom showing agent/model/provider and context usage,
mouse scrollback through session history, and streaming tool call visualization
with syntax-colored diffs. Long splash input is automatically truncated with `…`
so it never overflows the input box border. Thought blocks are rendered in the
active theme's accent and muted colors.

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
default = "agent"       # "allow" | "agent" | "prompt" | "deny"
allowed_commands = []
denied_commands = ["rm", "sudo"]

# Optional: theme = "ocean"  (default, ocean, rose, forest, sunset, mono, breeze, ink, dawn, sand, mint, dusk)
```

### API Keys

Keys are stored in `~/.io/keys.toml` (with `chmod 600` on Unix). You can also use environment variables:

```toml
# ~/.io/keys.toml
anthropic = "sk-ant-..."
openai = "sk-proj-..."
```

Or set `api_key_env` in `config.toml` to reference an environment variable.

### OAuth Login (subscription access)

OpenAI and Anthropic also support signing in with your ChatGPT or Claude
account instead of an API key. This uses the OAuth 2.0 authorization-code flow
with PKCE; tokens are stored in `~/.io/oauth.toml` (with `chmod 600` on Unix)
and refreshed automatically as they expire.

```bash
# Sign in with ChatGPT (opens a browser, waits for the callback)
io login openai

# Sign in with a Claude subscription (opens a browser, paste the code back)
io login anthropic
```

`io login` marks the provider as OAuth-authenticated and makes it active. You
can also pick this flow interactively with `io connect`, or toggle it manually:

```bash
io config set provider.openai.auth oauth      # use ChatGPT login
io config set provider.openai.auth api_key    # back to API key
```

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

# OAuth sign-in (ChatGPT / Claude subscription)
io login openai
io login anthropic
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
│       ├── tui.rs               # REPL orchestration: agent construction, slash command dispatch
│       ├── input.rs             # Readline loops (REPL + splash), popup helpers, @file resolution
│       ├── stream.rs            # Streaming event processing: ThinkParser, blink_and_print
│       ├── cost.rs              # /cost report
│       ├── config_cmd.rs        # `io config …` and `io init` handlers
│       ├── agent.rs             # Agent switching (/agent command)
│       ├── connect.rs           # Interactive provider setup wizard (13 providers)
│       └── model.rs             # Provider switching (/model command)
│
├── io-tui/                      # Terminal UI components (library crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs               # Re-exports: picker, readline, render, theme
│       ├── render.rs            # Terminal rendering: markdown, diffs, splash, prompt bar, scrollback
│       ├── picker.rs            # Terminal interactive picker (arrow keys, viewport)
│       ├── readline.rs          # Custom readline with slash commands
│       └── theme.rs             # Interactive theme picker
│
├── io-runtime/                  # Core engine (library crate)
│   ├── Cargo.toml
│   ├── tests/
│   │   └── agent_loop.rs        # 11 integration tests against a scripted mock provider
│   └── src/
│       ├── lib.rs               # Crate root — re-exports public API + load_project_context()
│       ├── agent.rs             # Agent loop: LLM completion + tool execution (sync & streaming)
│       ├── compact.rs           # /compact + auto-compact summarization
│       ├── config.rs            # Config/schema (TOML), KeyStore, provider config structs
│       ├── types.rs             # Core data types — Session, Turn, ToolCallRecord, TurnUsage
│       ├── memory.rs            # SQLite-backed session persistence (CRUD)
│       ├── sandbox.rs           # Permission checker (allow/deny/prompt modes)
│       ├── command_safety.rs    # Semantic command classifier (Safe/Caution/Destructive)
│       ├── pricing.rs           # Per-token cost calculation for supported providers
│       ├── tools/               # Built-in tools (7 tools, each with unit tests)
│       │   ├── mod.rs           # Tool trait, ToolRegistry, default_registry()
│       │   ├── read.rs          # Read files with offset/limit
│       │   ├── write.rs         # Write/create files, returns unified diff
│       │   ├── edit.rs          # Replace first occurrence of text, returns unified diff
│       │   ├── bash.rs          # Execute shell commands with timeout & workdir
│       │   ├── glob.rs          # Find files by glob pattern (sorted by mtime)
│       │   ├── grep.rs          # Search file contents with regex
│       │   └── spawn.rs         # spawn_agent — delegate to a restricted sub-agent
│       │
│       └── provider/            # LLM provider implementations
│           ├── mod.rs           # ProviderKind enum, CompletionModel trait, create_provider()
│           ├── anthropic.rs     # Anthropic Claude
│           ├── openai.rs        # OpenAI + 8 compat providers via OpenAICompatProvider
│           ├── gemini.rs        # Google Gemini
│           ├── azure.rs         # Azure OpenAI
│           └── bedrock.rs       # AWS Bedrock
│
├── io-agents/                   # Built-in agent definitions (library crate)
│   └── src/
│       ├── agent_config.rs      # AgentConfig + ToolAccess + auto_allow_writes
│       └── builtin/             # build, plan, debug, explore, review, test,
│                                #   security, docs, git, refactor, general
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
2. **Project context** — On startup, `load_project_context()` reads `AGENTS.md` / `CLAUDE.md` from the project root and passes the content to the agent; it is injected as a synthetic user/assistant message pair so the model always has project conventions in context
3. **Agent loop** — The agent iterates up to 20 turns: sends conversation history + system prompt + project context + tool specs to the LLM
4. **Tool execution** — If the LLM requests a tool call, the agent executes it via `ToolRegistry`, checks permissions via `PermissionChecker`, and feeds results back to the model
5. **Streaming** — In interactive mode, text deltas stream to the terminal as they arrive; tool starts and completions are rendered inline (with syntax-colored diffs for `write`/`edit`)
6. **Persistence** — Each turn (with tool call records and token usage) is saved to SQLite for session resumption

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

# Run tests (149 unit tests + 9 integration tests)
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
