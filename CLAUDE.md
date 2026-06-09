# AGENTS.md - io Development Guide

This document provides comprehensive guidance for AI agents and developers working on the `io` codebase - a Rust-based AI coding agent for the terminal.

## Project Overview

**io** is an AI coding assistant that runs directly in the terminal, built entirely in Rust. It supports 13 LLM providers, interactive REPL sessions, tool execution, and permission sandboxing.

### Key Features
- Multi-provider support (13 providers: Anthropic, OpenAI, Gemini, Groq, Ollama, Azure, Bedrock, Mistral, DeepSeek, OpenRouter, xAI, OpenCode Go, OpenCode Zen)
- 6 built-in tools: read, write, edit, bash, glob, grep
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
│       ├── main.rs            # Entry point, CLI parsing, REPL loop
│       ├── connect.rs         # Interactive provider setup wizard
│       ├── model.rs           # Provider switching (/model command)
│       ├── picker.rs          # Terminal interactive picker
│       └── readline.rs        # Custom readline with slash commands
├── io-runtime/                # Core engine (library crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # Public API re-exports
│       ├── agent.rs           # Agent loop (LLM + tool execution)
│       ├── config.rs          # Configuration schema and loading
│       ├── types.rs           # Core data types (Session, Turn, etc.)
│       ├── memory.rs          # SQLite-backed session persistence
│       ├── context.rs         # Context building for LLM messages
│       ├── sandbox.rs         # Permission checking system
│       ├── provider/          # LLM provider implementations (13)
│       │   ├── mod.rs         # Provider trait and common types
│       │   ├── anthropic.rs
│       │   ├── openai.rs
│       │   ├── gemini.rs
│       │   ├── groq.rs
│       │   ├── ollama.rs
│       │   ├── azure.rs
│       │   ├── bedrock.rs
│       │   ├── mistral.rs
│       │   ├── deepseek.rs
│       │   ├── openrouter.rs
│       │   ├── xai.rs
│       │   ├── opencode_go.rs
│       │   └── opencode_zen.rs
│       └── tools/             # Built-in tool implementations
│           ├── mod.rs         # Tool trait and registry
│           ├── read.rs
│           ├── write.rs
│           ├── edit.rs
│           ├── bash.rs
│           ├── glob.rs
│           └── grep.rs
```

### Crate Responsibilities

**io (CLI crate)**:
- Command-line argument parsing with `clap`
- Interactive REPL loop with streaming display
- Terminal UI components (picker, readline)
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
    provider: Arc<ProviderKind>,
    tools: Arc<ToolRegistry>,
    session: Arc<Mutex<Session>>,
    memory: Arc<SessionStore>,
    permissions: Arc<PermissionChecker>,
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
pub trait CompletionModel: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse>;
    async fn stream(&self, request: CompletionRequest) -> anyhow::Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>>;
}
```

**Adding a New Provider**:
1. Create a new file in `io-runtime/src/provider/`
2. Implement the `CompletionModel` trait
3. Add configuration struct in `io-runtime/src/config.rs`
4. Add to `ProviderKind` enum and `create_provider()` in `io-runtime/src/provider/mod.rs`
5. Add to `PROVIDERS` array and match block in `io/src/connect.rs`

**Provider Configuration Pattern**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub model: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: String,
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

**Adding a New Tool**:
1. Create new file in `io-runtime/src/tools/`
2. Implement the `Tool` trait
3. Add to `default_registry()` in `tools/mod.rs`
4. Export in `tools/mod.rs`

### Session Management (io-runtime/src/memory.rs)

Sessions are persisted using SQLite with the following schema:

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL,  -- JSON serialized Session
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    data TEXT NOT NULL,  -- JSON serialized Turn
    timestamp TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);
```

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

**Permission Checking**:
- Tool-level permissions via allow/deny lists
- Command pattern matching for bash tool
- Configurable default mode
- Integration with agent loop for tool execution control

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
- `io config {show|set}` - Configuration management
- `io init` - Initialize project-level config

**REPL slash commands**: `/help`, `/connect`, `/model`, `/cost`, `/exit` (also `/quit`, `/q`)

### REPL Loop (io/src/main.rs)

The interactive REPL:

1. Load/create session
2. Initialize agent with provider, tools, permissions
3. Enter read-eval-print loop:
   - Read user input with custom readline
   - Handle slash commands (/help, /connect, /model, /cost, /exit)
   - Execute agent turn with streaming
   - Display tool calls and results inline
   - Save session after each turn

### Streaming Display

The streaming implementation:
- Uses `tokio::sync::mpsc::Sender<AgentEvent>` for events
- Events: `Text`, `Thinking`, `ToolStart`, `ToolDone`, `Usage` (token counts after each turn)
- Real-time token-by-token display with cursor indicator
- Syntax-colored diff output for write/edit tools
- Inline tool call visualization

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

The `active_model_id()` function in `main.rs` maps provider names to their model configurations. When adding a new provider, update this function to include the new provider.

### Tool Execution Flow

1. Agent receives tool call from LLM
2. Permission checker validates tool execution
3. Tool registry retrieves tool implementation
4. Tool executes with provided arguments
5. Result formatted and returned to LLM
6. Turn record saved with timing and success status

### Session Context Building

The `ContextManager` builds LLM message history:
- System prompt with tool descriptions
- Turn-by-turn conversation history
- Tool call and result serialization
- Proper role assignment (System, User, Assistant, Tool)

### Error Recovery

- Session load failures create new sessions
- Tool execution failures return error messages
- Provider failures propagate with context
- Configuration errors provide actionable messages

## Testing

The project has 43 unit tests across 7 modules. Run with `cargo test`.

| Module | Tests |
|---|---|
| `tools/read.rs` | missing arg, nonexistent file, content, offset, limit |
| `tools/write.rs` | missing args, create new file, overwrite + diff |
| `tools/edit.rs` | missing args, nonexistent file, not found, replace first |
| `tools/bash.rs` | missing arg, stdout, nonzero exit, timeout |
| `tools/glob.rs` | missing arg, finds files, no matches, invalid pattern |
| `tools/grep.rs` | missing arg, invalid regex, matches with line numbers, no matches |
| `sandbox.rs` | allow/deny/prompt modes, denylist, allowlist, command basename matching |
| `config.rs` | default config, roundtrip serialization |
| `pricing.rs` | cost calculation, known models, free/subscription/passthrough providers |

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

The system prompt is defined in the agent initialization in `main.rs`. It includes:
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
