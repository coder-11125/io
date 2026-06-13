# AGENTS.md - io Development Guide

This document provides comprehensive guidance for AI agents and developers working on the `io` codebase - a Rust-based AI coding agent for the terminal.

## Project Overview

**io** is an AI coding assistant that runs directly in the terminal, built entirely in Rust. It supports 13 LLM providers, interactive REPL sessions, tool execution, and permission sandboxing.

### Key Features
- Multi-provider support (13 providers: Anthropic, OpenAI, Gemini, Groq, Ollama, Azure, Bedrock, Mistral, DeepSeek, OpenRouter, xAI, OpenCode Go, OpenCode Zen)
- 7 built-in tools: read, write, edit, bash, glob, grep, spawn_agent (sub-agent delegation)
- Built-in agent roles (`io-agents` crate): full agents (build, plan, debug, refactor) and restricted sub-agents (explore, review, test, security, docs, git, …)
- Interactive REPL and single-shot modes
- Permission sandboxing (allow/deny/prompt modes)
- SQLite-backed session persistence
- Real-time streaming responses
- Terminal UI components (interactive picker, readline with completions)
- Project-level configuration

## Architecture

### Workspace Structure

```
io/
├── Cargo.toml                 # Workspace configuration
├── io/                        # CLI frontend (binary crate)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            # Entry point, CLI parsing, subcommand dispatch
│       ├── repl.rs            # Interactive REPL + single-shot runner, streaming turn loop
│       ├── render.rs          # Terminal rendering: markdown, diffs, tool calls, status line
│       ├── cost.rs            # /cost report
│       ├── config_cmd.rs      # `io config …` and `io init` handlers
│       ├── agent.rs           # Agent switching (/agent command)
│       ├── connect.rs         # Interactive provider setup wizard
│       ├── model.rs           # Provider switching (/model command)
│       ├── picker.rs          # Terminal interactive picker (typed Dismissed errors)
│       ├── readline.rs        # Custom readline with slash commands
│       └── theme.rs           # Interactive theme picker
├── io-runtime/                # Core engine (library crate)
│   ├── Cargo.toml
│   ├── tests/
│   │   └── agent_loop.rs      # Integration tests against a scripted mock provider
│   └── src/
│       ├── lib.rs             # Public API re-exports
│       ├── agent.rs           # Agent loop (LLM + tool execution)
│       ├── compact.rs         # /compact + auto-compact summarization
│       ├── config.rs          # Configuration schema, loading, provider lookups
│       ├── pricing.rs         # Per-model cost tables
│       ├── types.rs           # Core data types (Session, Turn, etc.)
│       ├── memory.rs          # SQLite-backed session persistence
│       ├── sandbox.rs         # Permission checking system
│       ├── provider/          # LLM provider implementations
│       │   ├── mod.rs         # Provider trait, retry wrapper, OpenAI-compat table,
│       │   │                  #   create_provider() — 8 providers are one-line
│       │   │                  #   entries in compat_provider()
│       │   ├── anthropic.rs   # Anthropic
│       │   ├── openai.rs      # OpenAI + 8 compat providers (groq, mistral,
│       │   │                  #   deepseek, openrouter, xai, opencode_go,
│       │   │                  #   opencode_zen, ollama) via OpenAICompatProvider
│       │   ├── gemini.rs      # Google Gemini
│       │   ├── azure.rs       # Azure OpenAI
│       │   └── bedrock.rs     # AWS Bedrock
│       └── tools/             # Built-in tool implementations
│           ├── mod.rs         # Tool trait and registry
│           ├── read.rs
│           ├── write.rs
│           ├── edit.rs
│           ├── bash.rs
│           ├── glob.rs
│           ├── grep.rs
│           └── spawn.rs       # spawn_agent — delegate to a restricted sub-agent
└── io-agents/                 # Built-in agent definitions (library crate)
    └── src/
        ├── agent_config.rs    # AgentConfig + ToolAccess (All / Only(tools))
        └── builtin/           # build, plan, debug, explore, review, test,
                               #   security, docs, git, refactor, general
```

### Crate Responsibilities

**io (CLI crate)**:
- Command-line argument parsing with `clap`
- Interactive REPL loop with streaming display
- Terminal UI components (picker, readline, theme picker)
- Provider/model switching commands
- Session management commands
- Configuration management commands

**io-runtime (library crate)**:
- Core agent execution logic
- LLM provider abstraction and implementations
- Tool execution framework
- Session persistence (SQLite)
- Permission sandboxing
- Context management for LLM conversations

**io-agents (library crate)**:
- Built-in agent definitions: id, system prompt, tool access, suggested model
- `ToolAccess::All` agents are selectable in the REPL; `ToolAccess::Only(...)`
  agents are spawnable as sub-agents via the `spawn_agent` tool

## Development Workflow

### Building the Project

```bash
# Build the entire workspace
cargo build

# Build with optimizations
cargo build --release

# Run the CLI
cargo run --bin io

# Run with arguments
cargo run --bin io -- --help
```

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for specific crate
cargo test -p io-runtime

# Run tests with output
cargo test -- --nocapture
```

### Development Commands

```bash
# Install locally for testing
cargo install --path .

# Run in development mode with debug logging
RUST_LOG=debug cargo run --bin io

# Check code formatting
cargo fmt --check

# Run linter
cargo clippy
```

## Code Conventions

### Rust Style
- Follow standard Rust formatting (`cargo fmt`)
- Use `cargo clippy` for linting
- Prefer idiomatic Rust patterns
- Use `async/await` for asynchronous operations
- Leverage `anyhow` for error handling
- Use `thiserror` for custom error types when needed

### Naming Conventions
- **Structs**: `PascalCase` (e.g., `Agent`, `Session`)
- **Functions**: `snake_case` (e.g., `run_turn`, `build_messages`)
- **Constants**: `SCREAMING_SNAKE_CASE` (e.g., `MAX_ITERATIONS`, `VIEWPORT`)
- **Acronyms**: Follow Rust conventions (e.g., `api_key` not `apiKey`)

### Error Handling
- Use `anyhow::Result<T>` for application errors
- Use `thiserror` for library-level error types
- Provide context with `.context()` from anyhow
- Avoid silent error swallowing - log warnings where appropriate
- Cross-module error contracts are typed, not string-matched:
  - `provider::ApiError { provider, status, message }` — non-2xx provider
    responses; retry classification reads the status structurally
  - `agent::Cancelled` — turn aborted via the cancellation flag; detect
    with `err.is::<Cancelled>()`
  - `picker::Dismissed::{Cancelled, Interrupted}` — picker backed out;
    detect with `err.is::<picker::Dismissed>()`

### Async Patterns
- Use `tokio` as the async runtime
- Mark async functions with `async fn`
- Use `Box::pin()` for trait object futures
- Prefer `async_trait` for trait implementations

## Key Components

### Agent System (io-runtime/src/agent.rs)

The `Agent` struct orchestrates the core conversation loop:

```rust
pub struct Agent {
    provider: Arc<dyn CompletionModel>,  // trait object — mockable in tests
    tools: Arc<ToolRegistry>,
    session: Arc<Mutex<Session>>,
    memory: Arc<SessionStore>,
    permissions: Arc<PermissionChecker>, // shared with SpawnAgentTool — sub-agents inherit it
    system_prompt: String,
    max_tokens: u32,
    pub model_id: String,
    pub provider_id: &'static str,
}
```

**Key Methods**:
- `run_turn()` - Execute a single conversation turn (non-streaming)
- `run_turn_streaming()` - Execute with real-time token streaming
- `session_id()` - Get the current session ID
- `context_window()` - Get the provider's context window size (delegates to `CompletionModel`)
- `set_prompt_fn()` / `set_cancel()` - Permission prompt callback and cancellation flag

Both run methods share one implementation (`run_turn_inner`); the optional
event channel selects streaming vs. blocking mode. Per-turn usage records
sum input tokens across loop iterations (billing-accurate), while the
`Usage` event and auto-compact threshold use the last reported input
(context-accurate).

**Replay policy**: prior turns are replayed into context as user/assistant
text only — tool calls and results are deliberately dropped (token-heavy,
and providers reject tool blocks with stale IDs). Within a turn the model
sees full tool traffic; across turns it relies on its own prose.

**Mid-turn provider failures** save partial progress (text and executed
tool calls) to the session before surfacing the error; tool calls parsed
from a truncated stream are not executed.

**Agent Loop**:
1. Build message history from session
2. Call LLM with tools
3. Parse response for text and tool calls
4. Execute permitted tools
5. Feed tool results back to LLM
6. Repeat until no more tool calls
7. Save turn to session

### Provider System (io-runtime/src/provider/)

The provider system supports 13 LLM providers through a common trait:

```rust
#[async_trait]
pub trait CompletionModel: Debug + Send + Sync {
    fn provider_name(&self) -> &'static str;
    fn context_window(&self) -> u64; // defaults to 128K
    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse>;
    async fn complete_stream(&self, request: CompletionRequest)
        -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>>;
}
```

`create_provider()` returns `Arc<dyn CompletionModel>` wrapped in `Retrying`,
a decorator that retries transient failures (classified structurally via the
`ApiError` HTTP status) with exponential backoff. Eight providers (Groq,
Mistral, DeepSeek, OpenRouter, xAI, OpenCode Go/Zen, Ollama) are thin
`OpenAICompatProvider` instances driven by the `compat_provider()` id →
base-URL table; only Anthropic, OpenAI, Gemini, Azure, and Bedrock have
their own implementation files.

**Adding a New Provider** (OpenAI-compatible — the common case):
1. Add a config struct in `io-runtime/src/config.rs` and a field on `ProviderConfig`
2. Add the id to `key_overrides()`, `model_for()`, and `context_window_for()` in `config.rs`
3. Add a one-line entry to `compat_provider()` in `io-runtime/src/provider/mod.rs`
   (plus `default_key_env()` and optional `compat_context_window()` tuning)
4. Add to `PROVIDERS` array and match block in `io/src/connect.rs`

For non-OpenAI protocols, additionally implement `CompletionModel` in a new
file and give it a match arm in `create_provider()`.

**Provider Configuration Pattern**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub model: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Overrides the built-in model-name-based context window guess.
    pub context_window: Option<u64>,
}
```

### Tool System (io-runtime/src/tools/)

Tools implement a common trait for execution:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: ToolInput) -> ToolOutput;
}
```

**Built-in Tools**:
- `ReadTool` - Read file contents
- `WriteTool` - Write/create files
- `EditTool` - String replacement in files
- `BashTool` - Execute shell commands
- `GlobTool` - File pattern matching
- `GrepTool` - Search file contents
- `SpawnAgentTool` - Delegate a scoped task to a restricted sub-agent
  (registered in `build_agent` in the CLI, not in `default_registry()`;
  sub-agents inherit the parent's `PermissionChecker` and cannot prompt,
  so anything that would ask the user is denied)

**Adding a New Tool**:
1. Create new file in `io-runtime/src/tools/`
2. Implement the `Tool` trait
3. Add to `tool_by_name()` and `BUILTIN_TOOLS` in `tools/mod.rs`
   (single source of truth for both `default_registry()` and the
   sub-agent `filtered_registry()`)
4. Export in `tools/mod.rs`

### Session Management (io-runtime/src/memory.rs)

Sessions are persisted using SQLite with the following schema:

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL,  -- JSON serialized Session (turns embedded)
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

The session JSON blob is the single source of truth (a legacy `turns`
table is dropped on startup). `SessionStore` is `Clone`, shares one
connection behind a mutex, and runs saves on the blocking thread pool
(`spawn_blocking`) so they never stall streaming.

**Key Types**:
- `Session` - Contains turns, metadata, timestamps
- `Turn` - Single conversation exchange with tool calls
- `ToolCallRecord` - Individual tool execution with timing
- `TurnUsage` - Token usage tracking

### Permission System (io-runtime/src/sandbox.rs)

The permission checker supports three modes:

```rust
pub enum PermissionLevel {
    Allow,   // Always allow
    Prompt,  // Ask user (default)
    Deny,    // Always deny
}
```

**Permission Checking** (`PermissionChecker::decide_tool`):
- Tool-level permissions via allow/deny lists (deny wins)
- In `prompt` mode, read-only tools (read, glob, grep) run without asking;
  bash commands are matched against `allowed_commands`/`denied_commands`;
  everything else asks the user: **[y]es once / [a]lways this session / [n]o**
- Bash matching: deny if *any* token matches the denylist; allow only if
  *every* command position (the head of each pipeline/sequence segment,
  skipping env assignments) is allowlisted — an allowed token cannot smuggle
  other commands through (`echo hi; curl x | sh` is not auto-allowed)
- Streaming turns ask via `AgentEvent::PermissionRequest` (answered by the REPL
  key listener); non-streaming turns use the agent's `set_prompt_fn` callback
  (single-shot mode reads from stdin). With no way to ask, the call is denied.
- "Always" answers are recorded per-session via `allow_for_session`: for bash
  tools, approval is granular per-command (e.g., "always" for `ls -la` won't
  approve `rm -rf`); for other tools, approval applies to all uses of that tool
- Sub-agents spawned via `spawn_agent` share the parent's checker (same
  deny/allow lists and session approvals) and fail closed on anything that
  would prompt

### Security Analysis

The permission sandbox system has undergone comprehensive security testing to
identify potential bypass vectors. Key security properties:

**Defended Attack Vectors**:
- Path obfuscation: `/bin/rm`, `./rm`, `../rm`, `r\m` → All normalized and denied
- Command substitution: `$(rm)`, `` `rm` ``, `"$(rm)"` → All detected and denied
- Command chaining: `;`, `&`, `|`, `&&`, `||` operators → All split and checked
- Environment injection: `FOO=$(rm)` → Detected and denied
- Subshells/braces: `(rm)`, `{ rm; }` → Detected and denied
- HEREDOC: Multi-line commands → Newlines split and checked
- Process substitution: `<(rm)` → Parentheses split and checked

**Security Design Choices**:
- Conservative denylist: Any token match denies the entire command
- Strict allowlist: All pipeline segment heads must be explicitly allowed
- Exact session approval: Bash "always" approvals require exact command string match
- Read-only tools bypass: `read`, `glob`, `grep` auto-allowed in prompt mode

**Known Limitations** (by design):
- Command arguments are not validated (e.g., allowing `ls` permits `ls -rf /`)
- Single-quoted strings don't trigger denial (correct shell behavior)
- Session approvals are exact-match only (prevents command variation bypasses)

See `SECURITY_ANALYSIS.md` for comprehensive test coverage and detailed findings.

### Configuration System (io-runtime/src/config.rs)

Configuration is loaded from TOML files with the following hierarchy:

1. Global config: `~/.io/config.toml`
2. Project config: `.io/config.toml` (if exists)
3. API keys: `~/.io/keys.toml` (chmod 600)

**Config Structure**:
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
default = "prompt"
allowed_commands = []
denied_commands = ["rm", "sudo"]
```

### Theme System (io/src/render.rs + io/src/theme.rs)

The terminal has 8 built-in themes with configurable accent colors and dark/light
modes. Each theme defines:
- `accent` — primary color (logo, status bar, highlights)
- `muted` — secondary color (borders, dots, hints)
- `diff_add_fg`/`diff_add_bg` — diff addition styling
- `diff_del_fg`/`diff_del_bg` — diff deletion styling
- `diff_add_prefix`/`diff_del_prefix` — single-char diff markers (▶, +, ◆, etc.)

| Theme | Type | Accent |
|---|---|---|
| `default` | dark | Cyan |
| `ocean` | dark | Blue |
| `rose` | dark | Magenta |
| `forest` | dark | Green |
| `sunset` | dark | Yellow |
| `mono` | dark | White |
| `breeze` | light | DarkCyan |
| `ink` | light | DarkBlue |

The active theme is persisted in `~/.io/config.toml` via the `theme` key.
Switch with `/theme` in the REPL or `io config set theme <name>`.

## CLI Implementation Details

### Command Structure (io/src/main.rs)

The CLI uses `clap` for argument parsing:

```rust
#[derive(Parser)]
#[command(name = "io", version, about = "AI coding agent for the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    prompt: Option<String>,
    #[arg(long)]
    new: bool,
    #[arg(long)]
    r#continue: bool,
    #[arg(long)]
    model: Option<String>,
}
```

**Subcommands**:
- `io session {list|show|delete}` - Session management
- `io config {show|set}` - Configuration management. `config set` keys:
  `provider.default`, `provider.<name>.model`, `provider.<name>.api_key_env`,
  `provider.azure.deployment`, `session.auto_compact`, `session.memory_enabled`,
  `session.max_turns`, `session.max_tokens`, `permissions.default`, `theme`
- `io init` - Initialize project-level config

**REPL slash commands**: `/help`, `/new`, `/agent`, `/connect`, `/model`, `/theme`, `/cost`, `/compact`, `/exit` (also `/quit`, `/q`).
Switching agent, provider, or model mid-conversation keeps the current session
(`SessionChoice::Existing` in `build_agent`) — history is preserved.

**@file mentions**: Typing `@path/to/file` expands the file or directory contents inline
before sending to the LLM. Supports text files (up to 100KB) and directories (lists entries).

### REPL Loop (io/src/repl.rs)

The interactive REPL:

1. Load/create session
2. Initialize agent with provider, tools, permissions
3. Enter read-eval-print loop:
   - Read user input with custom readline
   - Handle slash commands (/help, /connect, /model, /cost, /exit)
   - Execute agent turn with streaming
   - Display tool calls and results inline
   - Save session after each turn

Tab key at the empty prompt cycles through available full agents (build, plan,
debug, refactor) without needing the `/agent` command.

### Full-Screen TUI

The interactive REPL uses an alternate-screen TUI with:

- **Splash screen**: Centered logo, input box with placeholder, agent/model/provider
  status line, commands reference, cwd/version footer. Agent cycling with Tab.
- **Fixed prompt bar** (bottom 3 rows): Thin separator, `▌`-accented input line,
  status row with `agent · model · provider` and context usage info (`X% used · Y rem · Z ctx`).
- **Scrollback**: Mouse scroll-wheel navigates session history; any key returns to live.
- **File completion popup**: `@` triggers filesystem completion with keyboard navigation.
- **Slash command popup**: `/` triggers filtered command completion in a bordered box.
- **Streaming**: Events (`Text`, `Thinking`, `ToolStart`, `ToolDone`, `Usage`) stream
  token-by-token with a cursor indicator. Syntax-colored diffs for write/edit tools.
- **Resize handling**: Prompt bar and scroll region re-flow on terminal resize.

### Terminal UI Components

**Picker (io/src/picker.rs)**:
- Arrow-key navigation
- Viewport scrolling for long lists
- Highlight current selection
- Support for hints (secondary labels)

**Readline (io/src/readline.rs)**:
- Custom input handling with raw mode
- Inline slash command completion
- Tab completion and arrow navigation
- Ctrl+C (cancel) and Ctrl+D (exit) handling

## Important Patterns

### Provider Selection Logic

`ProviderConfig` in `io-runtime/src/config.rs` owns all id → config-slot
lookups: `active_model()` / `model_for()` (display, pricing, session
metadata), `key_overrides()` (credentials), and `context_window_for()`.
When adding a new provider, update these three lookup methods.

### Tool Execution Flow

1. Agent receives tool call from LLM
2. Permission checker validates tool execution
3. Tool registry retrieves tool implementation
4. Tool executes with provided arguments
5. Result formatted and returned to LLM
6. Turn record saved with timing and success status

### Session Context Building

Message history is built inline in `Agent::run_turn_inner`:
- System prompt (plus the compaction summary, when one exists)
- Prior turns replayed as user/assistant text only (see the replay policy
  under Agent System — tool traffic is not replayed across turns)
- The current user input

### Error Recovery

- Session load failures create new sessions
- Tool execution failures return error messages
- Provider failures propagate with context
- Configuration errors provide actionable messages

## Testing

The project has 34 unit tests (29 in `io-runtime`, 5 in `io`) plus 11
integration tests (`io-runtime/tests/agent_loop.rs` — full agent-loop runs
against a scripted mock provider, covering tool execution, permission
prompting/denial, streaming events, usage tracking, session resumption,
sub-agent permission inheritance, and partial-progress persistence on
mid-turn provider failure). Run with `cargo test`.

| Module | Tests |
|---|---|
| `tools/read.rs` | missing arg, nonexistent file, content, offset, limit |
| `tools/write.rs` | missing args, create new file, overwrite + diff |
| `tools/edit.rs` | missing args, nonexistent file, not found, replace first |
| `tools/bash.rs` | missing arg, stdout, nonzero exit, timeout |
| `tools/glob.rs` | missing arg, finds files, no matches, invalid pattern |
| `tools/grep.rs` | missing arg, invalid regex, matches with line numbers, no matches |
| `sandbox.rs` | allow/deny/prompt modes, denylist, allowlist requires every command head, env-assignment skipping, decide_tool prompting, session approvals |
| `provider/mod.rs` | retry classification by HTTP status, unknown-provider rejection, compat provider resolution, context-window tuning |
| `config.rs` | default config, roundtrip serialization |
| `pricing.rs` | cost calculation, known models, free/subscription/passthrough providers |
| `io/src/repl.rs` | ThinkParser: plain text, think blocks, tags split across deltas, unterminated blocks, multibyte boundaries |

### CI

GitHub Actions runs on every push and PR to `main` (`.github/workflows/ci.yml`):
1. `cargo fmt --check`
2. `cargo clippy -- -D warnings`
3. `cargo build`
4. `cargo test`

### Manual Testing
- Test with real LLM providers
- Test interactive REPL flows
- Test streaming display
- Test permission prompts
- Test session resumption

## Common Tasks

### Adding a New LLM Provider

1. Create provider file in `io-runtime/src/provider/`
2. Implement `CompletionModel` trait
3. Add config struct to `io-runtime/src/config.rs`
4. Add to `ProviderKind` enum and `create_provider()` in `io-runtime/src/provider/mod.rs`
5. Add to `PROVIDERS` array and match block in `io/src/connect.rs`
6. Test with real API credentials

### Adding a New Tool

1. Create tool file in `io-runtime/src/tools/`
2. Implement `Tool` trait with name, description, schema
3. Implement async execute method
4. Register in `default_registry()` in `tools/mod.rs`
5. Export in `tools/mod.rs`
6. Test tool execution in agent context

### Modifying System Prompt

The system prompt comes from the active agent's `AgentConfig` in the `io-agents` crate (selected in `io/src/repl.rs`). It includes:
- Agent role and behavior instructions
- Tool descriptions (automatically appended)
- Permission and safety guidelines

### Debugging Session Issues

Session data is stored in:
- macOS: `~/Library/Application Support/io/sessions.db`
- Linux: `~/.local/share/io/sessions.db`

Use SQLite tools to inspect session data:
```bash
sqlite3 ~/.local/share/io/sessions.db "SELECT * FROM sessions;"
```

## Dependencies

### Key Workspace Dependencies
- `tokio` - Async runtime
- `serde` + `serde_json` - Serialization
- `reqwest` - HTTP client for provider APIs
- `rusqlite` - SQLite for session storage
- `clap` - CLI argument parsing
- `crossterm` - Terminal UI
- `anyhow` - Error handling
- `thiserror` - Custom error types
- `tracing` - Structured logging

### CLI-Specific Dependencies
- `termimad` - Terminal markdown rendering

## Environment Variables

Provider API keys (can also be stored in `~/.io/keys.toml`):
- `ANTHROPIC_API_KEY` - Anthropic
- `OPENAI_API_KEY` - OpenAI
- `GEMINI_API_KEY` - Google Gemini
- `GROQ_API_KEY` - Groq
- `MISTRAL_API_KEY` - Mistral AI
- `DEEPSEEK_API_KEY` - DeepSeek
- `OPENROUTER_API_KEY` - OpenRouter
- `XAI_API_KEY` - xAI (Grok)
- `AZURE_OPENAI_API_KEY` - Azure OpenAI
- `OPENCODE_GO_API_KEY` / `OPENCODE_API_KEY` - OpenCode Go / Zen
- Ollama and Bedrock use local credentials — no API key required

Debug logging:
```bash
RUST_LOG=debug cargo run --bin io
RUST_LOG=io_runtime=trace cargo run --bin io
```

## License

MIT License - See LICENSE file for details.

## Contributing

When contributing to io:
1. Follow existing code conventions
2. Add tests for new functionality
3. Update documentation as needed
4. Test with multiple providers when applicable
5. Ensure terminal UI interactions work correctly
