# AGENTS.md - io Development Guide

This document provides comprehensive guidance for AI agents and developers working on the `io` codebase - a Rust-based AI coding agent for the terminal.

## Project Overview

**io** is an AI coding assistant that runs directly in the terminal, built entirely in Rust. It supports 13 LLM providers, interactive REPL sessions, tool execution, and permission sandboxing.

### Key Features
- Multi-provider support (13 providers: Anthropic, OpenAI, Gemini, Groq, Ollama, Azure, Bedrock, Mistral, DeepSeek, OpenRouter, xAI, OpenCode Go, OpenCode Zen)
- 13 built-in tools: read (text, images, PDFs, notebooks), write, edit, multi_edit, notebook_edit, bash, glob, grep, fetch (HTTP GET → readable text, optional extraction prompt), web_search, todo_write (task list), exit_plan_mode, spawn_agent (sub-agent delegation)
- Working modes cycled with Shift+Tab: default → accept edits → plan (read-only research, then a plan to approve)
- Edit safety: files must be read before they are modified, and edits are refused if the file changed on disk since
- Vision input (`read` on images, `@image.png`, Ctrl+V clipboard paste) and extended thinking (`/think`, Alt+T, `think`/`ultrathink` keywords)
- Built-in agent roles (`io-agents` crate): full agents (build, plan, debug, refactor) and restricted sub-agents (explore, review, test, security, docs, git, …)
- Hooks (shell commands at eight lifecycle points) and an MCP client (stdio / streamable HTTP / SSE servers; their tools run as `mcp__<server>__<tool>`)
- Full-screen TUI and single-shot modes
- Permission sandboxing (allow/deny/prompt modes)
- SQLite-backed session persistence
- Real-time streaming responses
- Terminal UI components in dedicated `io-tui` crate (interactive picker, readline with completions)
- Project-level configuration
- Project context loading: reads `AGENTS.md` / `CLAUDE.md` from the project root and injects them as context at the start of each conversation

## Architecture

### Workspace Structure

```
io/
├── Cargo.toml                 # Workspace configuration
├── io/                        # CLI frontend (binary crate)
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs            # Entry point, CLI parsing, subcommand dispatch
│       ├── tui.rs             # REPL orchestration: agent construction, slash command dispatch
│       ├── input.rs           # Readline loops (REPL + splash), popup helpers, @file resolution
│       ├── stream.rs          # Streaming event processing: ThinkParser, blink_and_print
│       ├── status.rs          # Working mode + thinking state, status-row badge, /think handling
│       ├── clipboard.rs       # Ctrl+V clipboard image paste, dropped image paths → @mentions
│       ├── cost.rs            # /cost report
│       ├── mcp_cmd.rs         # `io mcp …`, shared McpManager, project-server trust prompt
│       ├── config_cmd.rs      # `io config …` and `io init` handlers
│       ├── agent.rs           # Agent switching (/agent command)
│       ├── connect.rs         # Interactive provider setup wizard
│       └── model.rs           # Provider switching (/model command)
├── io-tui/                    # Terminal UI components (library crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # Re-exports: picker, readline, render, theme
│       ├── render.rs          # Terminal rendering: markdown, diffs, tool calls, status line
│       ├── picker.rs          # Terminal interactive picker (typed Dismissed errors)
│       ├── readline.rs        # Custom readline with slash commands
│       └── theme.rs           # Interactive theme picker
├── io-runtime/                # Core engine (library crate)
│   ├── Cargo.toml
│   ├── tests/
│   │   ├── agent_loop.rs      # Integration tests against a scripted mock provider
│   │   ├── mcp.rs             # MCP integration tests against tests/fixtures/mock_mcp_server.py (needs python3)
│   │   └── fixtures/
│   └── src/
│       ├── lib.rs             # Public API re-exports + load_project_context()
│       ├── agent.rs           # Agent loop (LLM + tool execution)
│       ├── compact.rs         # /compact + auto-compact summarization
│       ├── rewind.rs          # /rewind — restore files to an earlier point in the chat
│       ├── hooks.rs           # Lifecycle hooks: user shell commands around tool calls, prompts, turns
│       ├── mcp/               # MCP client: config.rs (files, trust), transport.rs (stdio/HTTP/SSE JSON-RPC),
│       │                      #   sse.rs, client.rs (McpClient), tool.rs (McpTool), mod.rs (McpManager)
│       ├── config.rs          # Configuration schema, loading, provider lookups
│       ├── pricing.rs         # Per-model cost tables
│       ├── types.rs           # Core data types (Session, Turn, etc.)
│       ├── memory.rs          # SQLite-backed session persistence
│       ├── sandbox.rs         # Permission checking system + working modes (InteractionMode)
│       ├── thinking.rs        # Extended thinking: per-turn budget, keyword tiers, /think parsing
│       ├── command_safety.rs  # Semantic command classifier (Safe/Caution/Destructive)
│       ├── provider/          # LLM provider implementations
│       │   ├── mod.rs         # Provider trait, retry wrapper, OpenAI-compat table,
│       │   │                  #   create_provider() — 8 providers are one-line
│       │   │                  #   entries in compat_provider()
│       │   ├── anthropic.rs   # Anthropic — SSE streaming via AnthropicStreamState
│       │   ├── openai.rs      # OpenAI + 8 compat providers (groq, mistral,
│       │   │                  #   deepseek, openrouter, xai, opencode_go,
│       │   │                  #   opencode_zen, ollama) via OpenAICompatProvider
│       │   ├── gemini.rs      # Google Gemini
│       │   ├── azure.rs       # Azure OpenAI
│       │   └── bedrock.rs     # AWS Bedrock — binary event-stream + shared AnthropicStreamState
│       └── tools/             # Built-in tool implementations
│           ├── mod.rs         # Tool trait and registry
│           ├── file_state.rs  # FileTracker — read-before-edit + external-change detection
│           ├── read.rs        # text (offset/limit), images, PDFs (pdf-extract), notebooks
│           ├── write.rs
│           ├── edit.rs        # also home of apply_edit(), shared with multi_edit
│           ├── multi_edit.rs  # several edits to one file, all-or-nothing
│           ├── notebook.rs    # .ipynb render/parse/serialize + notebook_edit
│           ├── bash.rs
│           ├── glob.rs
│           ├── grep.rs
│           ├── fetch.rs       # HTTP(S) GET → readable text; optional `prompt` extraction
│           ├── html.rs        # HTML → text converter (fetch, web_search)
│           ├── web_search.rs  # DuckDuckGo / Brave / Tavily search with domain filters
│           ├── todo.rs        # todo_write
│           ├── plan.rs        # exit_plan_mode
│           └── spawn.rs       # spawn_agent — delegate to a restricted sub-agent
└── io-agents/                 # Built-in agent definitions (library crate)
    └── src/
        ├── agent_config.rs    # AgentConfig + ToolAccess (All / Only(tools)) + auto_allow_writes
        └── builtin/           # build, plan, debug, explore, review, test,
                               #   security, docs, git, refactor, general
```

### Crate Responsibilities

**io (CLI crate)**:
- Command-line argument parsing with `clap`
- Interactive TUI loop with streaming display (`tui.rs`, `input.rs`, `stream.rs`)
- Provider/model switching commands
- Session management commands
- Configuration management commands

**io-tui (TUI crate)**:
- Terminal UI components: interactive picker, custom readline, theme picker
- Terminal rendering: markdown, diffs, splash screen, prompt bar (with a status badge for the working mode / thinking), scrollback, the live todo checklist
- Used by `io` (CLI) and available to any future binary that needs TUI

**io-runtime (library crate)**:
- Core agent execution logic
- LLM provider abstraction and implementations
- Tool execution framework
- Session persistence (SQLite)
- Permission sandboxing
- Context management for LLM conversations
- Project context loading (`load_project_context()` — reads `AGENTS.md`/`CLAUDE.md`)

**io-agents (library crate)**:
- Built-in agent definitions: id, system prompt, tool access, suggested model, auto_allow_writes flag
- `ToolAccess::All` agents are selectable in the TUI; `ToolAccess::Only(...)`
  agents are spawnable as sub-agents via the `spawn_agent` tool
- `auto_allow_writes: true` on build, debug, docs, explore, general, refactor — write/edit tool calls skip the permission prompt for those agents
- `auto_allow_writes: false` on plan, git, review, security, test — those agents always prompt before writing

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
    project_context: Option<String>,     // contents of AGENTS.md / CLAUDE.md at project root
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
`Usage` event and auto-compact threshold use the last reported context size
(context-accurate). Both are tracked by `UsageTally`; "context size" is
`Usage::context_tokens()` = uncached input + prompt-cache reads + cache
writes. Anthropic reports `input_tokens` *excluding* cached tokens, so
measuring `input_tokens` alone would make the context bar read near-zero and
silently stop auto-compact from ever triggering once caching is on.
`TurnUsage.input_tokens` stays uncached-only; `cache_read_tokens` /
`cache_write_tokens` are recorded beside it and priced by
`ModelPricing::calculate_cost_with_cache` using per-provider
`pricing::cache_rates_for_model` (approximate multiples of the input rate:
Anthropic reads 0.1× / writes 1.25×; OpenAI reads 0.1–0.5× by model; Gemini
2.x reads 0.25×; unknown models `CacheRates::NONE` = plain input rate, so
cost is never understated). `/cost` shows the cache totals.

**Replay policy**: prior turns are replayed into context as user/assistant
text only — tool calls and results are deliberately dropped (token-heavy,
and providers reject tool blocks with stale IDs). Within a turn the model
sees full tool traffic; across turns it relies on its own prose.

**Mid-turn provider failures** save partial progress (text and executed
tool calls) to the session before surfacing the error; tool calls parsed
from a truncated stream are not executed.

**Re-compaction** (`compact::run` in `compact.rs`, called from both
auto-compact and manual `/compact`) folds any existing `session.summary`
into the summarization prompt as a "prior summary" block, with an explicit
instruction to merge rather than replace it, before overwriting `summary`
with the model's response. Without this, a session compacted a second time
would silently lose everything the first summary captured, since the new
summary would only cover turns since the last compaction.

**Rewind** (`rewind::run` in `rewind.rs`, exposed as `Agent::rewind_to()`;
triggered by `/rewind` or pressing Esc twice within 500 ms at the idle
prompt) restores files to their state right before a chosen turn, then
truncates the session's turns to match. It walks the reverted turns in
order and, per file path, restores from the *first* `pre_edit_snapshot`
seen — the state right before the reverted range began — deleting files
that didn't exist yet. Only `write`/`edit` calls are tracked; `bash` has no
file-snapshotting and is out of scope, so file changes made via `bash` are
left as-is by a rewind.

**Edit safety** (`tools/file_state.rs`): one `FileTracker` is shared by
`read`, `write`, `edit`, `multi_edit` and `notebook_edit` in a registry (built by
`filtered_registry`, so each sub-agent registry has its own). `read` records a
fingerprint of the *whole* file (even for a partial read) and each successful
write records the new content, so consecutive edits need no re-read. Modifying
an existing file whose path was never read, or whose content no longer matches
the fingerprint, is refused with a message telling the model to read it again;
creating a new file needs no read. A formatter `post_tool_use` hook therefore
makes the next edit ask for a re-read — intended. After `/rewind` restores files
the same mechanism makes the model re-read them.

**Working modes** (`sandbox::InteractionMode`, held by the shared
`PermissionChecker`; `Agent::{interaction_mode, set_interaction_mode,
cycle_interaction_mode}`): `Default`, `AcceptEdits`, `Plan`. The TUI keeps the
user's mode in `io/src/status.rs` so it survives agent rebuilds (`/model`,
`/agent`, …), and the status-row badge comes from there.
- `Plan` is enforced first in `decide_tool` (`plan_mode_decision`), ahead of the
  permission mode — even `permissions.default = "allow"` cannot write in plan
  mode. File-write tools are denied; `bash` runs only if expansion-free,
  redirection-free and classified `Safe` (or it is background-job bookkeeping);
  `exit_plan_mode` always prompts (deny mode still denies). While plan mode is
  on the agent appends `PLAN_MODE_PROMPT` to the system message (rebuilt on every
  loop iteration, so it drops away the moment the plan is approved).
- `exit_plan_mode` is resolved in `Agent::resolve_permission`: outside plan mode
  it returns a harmless error; the user's reply decides the new mode —
  `AllowSession` → `AcceptEdits`, `AllowOnce` → `Default`, `Deny` stays in plan
  mode with a "revise your plan" message. It is never recorded as a session
  approval. The TUI shows the plan (markdown, under the tool header) and a
  three-option dialog whose order matches those replies.
- `AcceptEdits` approves `FILE_WRITE_TOOLS` at the end of `decide_tool`, i.e.
  after the sensitive-path and project-root guards.

**Extended thinking** (`thinking.rs`, `[thinking]` config, `Agent::{thinking,
set_thinking, with_thinking}`): per turn, `budget_for_turn` combines the
configured setting with a keyword in the user's text — `ultrathink` (32K),
`think hard`/`megathink`/… (10K), and a bare `think` only when it is an
instruction (sentence start or after "please"; never "I think …"). Keywords raise
a configured budget but never lower it, work even when thinking is off (unless
`keywords = false`), and ignore `@file`/hook/project-context blocks. The request
carries `CompletionRequest::thinking` and `max_tokens` is raised to
`budget + 4096` (cap 64K). All of that happens only when the active model
reports `CompletionModel::supports_thinking()` (Claude 3.7+/4 on Anthropic and
Bedrock, Gemini 2.5+, OpenAI o-series/GPT-5): for any other model a keyword or
the setting is ignored, because raising `max_tokens` past what such a model
allows would make its API reject the request. Anthropic additionally caps
`max_tokens` at 32K for Opus 4/4.1 (`anthropic::cap_output_tokens`). Reasoning blocks use `ContentBlock::Thinking`: an
unsigned block is a display delta (→ `AgentEvent::Thinking`, never replayed); a
*signed* block (Anthropic/Bedrock) is the finished block and, with
`RedactedThinking`, is stored ahead of the text/tool calls in the assistant
message so the next request of the tool loop carries it back verbatim (the API
requires this). Unsigned blocks are filtered out of history in blocking mode.
Thinking blocks are never persisted — prior turns replay as text only.

**Images**: `ContentBlock::Image { media_type, data }` (base64). A tool can
return images (`ToolOutput::with_image`, used by `read`); the agent appends them
as `Image` blocks after the `ToolResult` blocks of the same user message
(Anthropic requires results first; OpenAI turns them into a follow-up user
message because `role: tool` is text-only). User attachments go through
`Agent::run_turn_{streaming_}with_images` and lead the user message. Images are
not persisted: the saved turn gets a note ("N image(s) were attached"). Supported
types are sniffed by magic number (PNG/JPEG/GIF/WebP), limit 5 MB.

**Hooks** (`hooks.rs`; attached with `Agent::with_hooks`, built in
`build_agent` from `config.hooks`): user-configured shell commands run with
`sh -c` in the project root at eight points — `pre_tool_use`,
`post_tool_use`, `user_prompt_submit`, `stop`, `session_start`,
`session_end`, `notification` and `pre_compact`. Each gets a JSON description
of the event on stdin (plus `IO_HOOK_EVENT` / `IO_TOOL_NAME`). Exit codes:
`0` = success (stdout becomes extra prompt context for `user_prompt_submit`
and `session_start`, extra tool-result text for `post_tool_use`, summary
guidance for `pre_compact`), `2` = block (stderr is the reason; for
`pre_compact` it cancels the compaction), anything else / spawn failure /
timeout = logged and ignored, so a crashing guard fails *open*. A hook may
instead print JSON: `decision` (`block`/`deny` = veto, `ask` = force a
permission prompt, `allow` = no-op), `reason`, `additionalContext`,
`continue: false` + `stopReason`, or `hookSpecificOutput.permissionDecision`
(`interpret()` in `hooks.rs`). Matchers join tool names with `|` and accept
`*` globs (`mcp__github__*`). Properties worth keeping:
- `pre_tool_use` runs **before** the permission check and can only restrict
  (`PreToolOutcome { ask, context }`); a hook can never grant permission the
  checker withholds.
- `post_tool_use` runs only for calls that actually executed (not for
  hook-blocked or permission-denied ones); its feedback is appended to the
  tool output the model sees and to the saved `ToolCallRecord`.
- A blocked prompt returns the typed `agent::PromptBlocked` error before
  anything is sent or saved; hook-supplied prompt context rides on that
  request only — the saved turn keeps the user's text as typed.
- `session_start` fires once per session (a gate shared by every agent built
  for it — `tui.rs::session_gate()` — so `/model` and `/agent` rebuilds don't
  re-fire it; `/new` resets it); `session_end` fires from `Agent::end_session`
  on `/new`, exit and single-shot completion. `notification` is
  fire-and-forget, spawned when a permission prompt is shown.
- `SpawnAgentTool::with_hooks` gives sub-agents the parent's tool hooks
  (`Hooks::for_sub_agent`: `pre`/`post_tool_use` only), so delegating can't
  sidestep a guard. The other hooks are not inherited.
- Hooks are read from the **global** `~/.io/config.toml` only. Project
  config is deliberately not consulted, so a cloned repo can't plant a
  command that runs on the user's machine. Output per stream is capped at
  16 KB; default timeout 30 s (`timeout_ms` per hook), and a timed-out hook's
  whole process group is killed.

**MCP** (`io-runtime/src/mcp/`; `io/src/mcp_cmd.rs` owns the process-wide
`McpManager`): servers are read from `~/.io/mcp.json` (user) and `.mcp.json`
in the project root, in the Claude-compatible `mcpServers` shape
(`type`: stdio/http/sse, inferred from `command`/`url`; `${VAR}` and
`${VAR:-default}` expanded in commands, args, env, URLs, headers).
`McpManager::start` connects servers concurrently (initialize with protocol
`2025-03-26`, then `tools/list`); a server that fails is reported and skipped,
never fatal. Transports (`transport.rs`) all speak JSON-RPC through a
`Router` that matches responses to requests by id: stdio (child process,
stderr kept for error messages), streamable HTTP (JSON or SSE replies,
`Mcp-Session-Id`), and the legacy SSE transport. Tools are exposed as
`mcp__<server>__<tool>` (≤ 64 chars; server names are validated and may not
contain `__`); `build_agent` registers the manager's tools in every agent it
builds, so `/model`/`/agent` rebuilds reuse live connections (`SharedTool`).
Results are text (images/resources summarized) capped at `MAX_RESULT_BYTES`.
Startup / per-call timeouts: `IO_MCP_STARTUP_TIMEOUT_MS` (20 s),
`IO_MCP_TOOL_TIMEOUT_MS` (60 s).
- **Trust.** `.mcp.json` is untrusted input that can name any command, so
  project-scope servers launch only after approval, remembered per project in
  `~/.io/mcp_trust.json` keyed by `name:fingerprint` (changing a definition
  asks again). `run_interactive` asks on plain stdin *before* `enter_tui`;
  `run_single_shot`, `io mcp list` and `/mcp reconnect` skip unapproved
  servers with a note. User-scope servers are the user's own config.
- **Permissions.** MCP tools default to *Prompt*. The existing
  `permissions.allowed_commands` / `denied_commands` lists accept `mcp__server`,
  `mcp__server__*` or an exact tool name (`sandbox::mcp_rule_covers`); deny
  wins. In plan mode only tools whose server set `readOnlyHint` run
  (advisory: `PermissionChecker::mark_read_only`, called from `build_agent`).
  Hooks (matcher globs) and `/rewind` treat them like any other tool.
- **CLI.** `io mcp add [--scope user|project] [-t transport] [-e K=V]
  [-H "K: V"] <name> <command…|url>` (options before the name), `list`
  (connects and shows status), `get` (secrets masked), `remove`. Files are
  written `0600`. `/mcp` lists servers; `/mcp reconnect` restarts them.

**Agent Loop**:
1. Build message history from session (system prompt, then optional project-context synthetic message pair, then prior turns, then current user input). `user_prompt_submit` hooks run first and may reject the prompt or add context
2. Call LLM with tools
3. Parse response for text and tool calls
4. Run `pre_tool_use` hooks, check permissions, execute permitted tools, run `post_tool_use` hooks
5. Feed tool results back to LLM
6. Repeat until no more tool calls
7. Save turn to session, then run `stop` hooks (`session_start` runs before step 1 of a session's first turn; `session_end` when the session is left)

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

All 13 providers support both `complete` and `complete_stream`. Streaming
implementation notes:
- **Anthropic** — SSE (`text/event-stream`): request body includes `"stream": true`;
  events are parsed by `AnthropicStreamState` (in `anthropic.rs`), which tracks
  tool-use blocks by index and accumulates `input_json_delta` fragments until
  `content_block_stop`.
- **Bedrock** — AWS binary event-stream (`invoke-with-response-stream` endpoint):
  binary frames are decoded by `parse_event_frame` / `parse_headers` in
  `bedrock.rs`; each `chunk` event base64-decodes to the same Anthropic SSE
  event JSON, which is then processed by the shared `AnthropicStreamState`.
- **OpenAI / compat providers** — OpenAI SSE protocol with `stream_options.include_usage`.

**Thinking and images per provider**: Anthropic/Bedrock send
`thinking: {type: enabled, budget_tokens}` (`anthropic::thinking_param` clamps to
`1024 <= budget < max_tokens`, and skips thinking when there is no room), stream
`thinking_delta`/`signature_delta` through `AnthropicStreamState` (a signed block
is emitted at `content_block_stop`; unsigned ones are dropped), and replay signed
`thinking`/`redacted_thinking` blocks in `build_messages`. Gemini 2.5+ sends
`generationConfig.thinkingConfig {thinkingBudget, includeThoughts}` and treats
`thought: true` parts as display-only; its stream converter now returns a `Vec` of
events per chunk (thoughts, concatenated text, each function call, then stop/usage)
so parallel calls and multi-part text are no longer overwritten. OpenAI o-series /
GPT-5 (`openai::is_reasoning_model`, also looks through `provider/` prefixes) get
`reasoning_effort` (≤4K low, ≤12K medium, else high) and `max_completion_tokens`
instead of `max_tokens`; DeepSeek-style `reasoning_content`/`reasoning` deltas
stream as thinking. Other providers ignore `thinking`. Images: Anthropic/Bedrock
`image` source blocks, OpenAI `image_url` data URLs, Gemini `inlineData`.

**System prompt delivery**: the agent supplies its system prompt (plus any
compaction summary) as a `Role::System` message with `system_prompt: None`.
OpenAI-protocol providers read it from the message list; Anthropic, Bedrock
and Gemini keep the system prompt in a separate wire field, so they must use
`CompletionRequest::system_text()` (explicit `system_prompt` + all system
messages). Reading `request.system_prompt` alone silently drops the prompt —
this was a bug on those three providers before `system_text()` existed.

**Prompt caching (Anthropic)**: with `provider.anthropic.prompt_cache = true`
(default) `AnthropicProvider::build_body` — shared by `complete` and
`complete_stream` so both always send identical markers — sends `system` as a
single text block with `cache_control: ephemeral` (the request order is
tools → system → messages, so that breakpoint also caches the tool
definitions) and puts a second breakpoint on the last content block of the
last message. Within a turn each tool-loop request then reuses everything
before the newest tool result; across turns the replayed history (tool
traffic dropped) is a stable prefix. Cached tokens come back as
`Usage.cache_read_tokens` / `cache_write_tokens` (`AnthropicStreamState`
reads them from `message_start`, and `message_delta` when present). Prompts
under the model's minimum cacheable size are silently not cached by the API.
Bedrock sends no `cache_control` (support varies by model and a rejected
marker fails the request).

**Implicit caching (OpenAI, Gemini)**: these cache long repeated prefixes
server-side with no request markup, so there is nothing to enable — we only
report it. OpenAI (and compatible providers that mirror it) return
`usage.prompt_tokens_details.cached_tokens`; Gemini returns
`usageMetadata.cachedContentTokenCount`. In both, the cached count is a
*subset* of the prompt total, so `Usage::from_prompt_total` splits it out
(`input_tokens` = prompt − cached, `cache_read_tokens` = cached) — keeping
`input_tokens` uncached-only everywhere and `context_tokens()` equal to the
real prompt size. Providers that don't send the field (e.g. DeepSeek, which
uses `prompt_cache_hit_tokens`) simply report 0 cached.

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
- `ReadTool` - Read file contents. Dispatches on the bytes: images (magic-number
  sniffed; ≤ 5 MB; returned as `ToolOutput.images` plus a text line with
  dimensions), PDFs (`pdf-extract` on the blocking pool inside `catch_unwind`;
  page-tagged text, `pages` like `"2-5"`, first 20 pages and 100K characters by
  default), `.ipynb` notebooks (`notebook::render`: cells with ids and text
  outputs), otherwise line-numbered text. Binary non-image files are an error.
- `WriteTool` - Write/create files (overwriting needs a prior read). Captures the file's pre-write content (or
  its absence, for a new file) as a `FileSnapshot` on `ToolOutput`, skipping
  files over `MAX_SNAPSHOT_BYTES` (2 MB) so `/rewind` doesn't bloat the
  session's persisted JSON blob
- `EditTool` - String replacement in files (first occurrence, or all with
  `replace_all`). Captures the same pre-edit `FileSnapshot` as `WriteTool`
- `MultiEditTool` - Several edits to one file, applied in order to the running
  result and written once; any failing edit leaves the file untouched
- `NotebookEditTool` - Replace/insert/delete a notebook cell by `cell_id` or
  `cell_number`; serializes like Jupyter (sorted keys, 1-space indent), gives new
  cells an id when the notebook uses them, clears outputs of replaced code cells
- `TodoWriteTool` - Replaces the whole task list. Stateless: the TUI renders the
  checklist from the call's input (`render::tool_body_lines`), so it also
  shows up in scrollback
- `ExitPlanModeTool` - Presents a plan; the approval lives in the agent loop
  (see Working modes)
- `BashTool` - Execute shell commands with resource limits (512 MB VM, 60 s CPU,
  200 MB file size) applied at spawn. Foreground runs support a timeout (ms)
  that kills the **whole process group** (no orphaned grandchildren) and
  returns partial output. Background jobs (`action: start` → job id, then
  `status`/`output`/`kill`/`list`) keep running across turns and are killed if
  the owning tool is dropped. Esc (the shared cancel flag) also kills a
  running command via the process group.
- `GlobTool` - File pattern matching
- `GrepTool` - Search file contents
- `WebSearchTool` - `web_search`: backend chosen from the environment
  (`BRAVE_API_KEY` → Brave, `TAVILY_API_KEY` → Tavily, else DuckDuckGo's keyless
  HTML endpoint; `IO_SEARCH_BACKEND` forces one). `allowed_domains` /
  `blocked_domains` are applied to the results here (subdomains match, block
  wins), so they behave the same on every backend. Permission-wise it follows
  the same `allow_network_fetch` rule as `fetch`.
- `FetchTool` - HTTP(S) GET-only; HTML is converted to readable text
  (`html::to_text`), other text is returned as is (capped
  at 100 KB, truncated beyond that). With a `prompt` it answers that question
  by running one completion over the page text on an extraction model
  (`FetchTool::with_extractor`; `build_agent` passes the session's provider,
  the page is framed as untrusted data) and falls back to the page text if the
  call fails or no extractor is attached. Blocks localhost/private/link-local
  hosts and cloud-metadata IPs by hostname/literal-IP string check (not full
  SSRF protection — a public hostname resolving to a private address at
  request time is not caught); the same check is re-applied to every
  redirect hop. Permission-wise it's treated exactly like the `curl`/`wget`
  network-fetch lenience: gated by `permissions.allow_network_fetch`, only
  auto-runs in `agent` mode with that flag set, otherwise always prompts
- `SpawnAgentTool` - Delegate a scoped task to a restricted sub-agent
  (registered in `build_agent` in the CLI, not in `default_registry()`;
  sub-agents inherit the parent's `PermissionChecker` and cannot prompt,
  so anything that would ask the user is denied; `project_context` is
  forwarded so sub-agents have the same project guidance as the parent)

**Adding a New Tool**:
1. Create new file in `io-runtime/src/tools/`
2. Implement the `Tool` trait
3. Add to `tool_by_name()` and `BUILTIN_TOOLS` in `tools/mod.rs`
   (single source of truth for both `default_registry()` and the
   sub-agent `filtered_registry()`). File tools take the shared
   `Arc<FileTracker>` there. A tool that edits files must also be added to
   `sandbox::FILE_WRITE_TOOLS` (plan mode, accept-edits mode, sensitive-path /
   project-root guards, per-path session approval, `/rewind`, and the
   `written_this_turn` check all key off it) and use the `path` argument name
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

The permission checker supports four modes:

```rust
pub enum PermissionLevel {
    Allow,   // Always allow
    Prompt,  // Ask user
    Deny,    // Always deny
}
```

Mode is configured via `permissions.default` (a string):
- `allow` — everything allowed
- `agent` — the agent decides (default): read-only **and** caution-level bash
  commands auto-run; destructive and network commands still prompt. The user's
  `allowed_commands`/`denied_commands` always win. `spawn_agent` is delegated
  to the agent (sub-agents inherit the same checker, run restricted, and fail
  closed — no escalation). With `permissions.allow_network_fetch = true`,
  read-only fetches (`curl URL`, `wget -O- URL`) and the `fetch` tool also
  auto-run; file-writing, upload, and custom-method network commands still
  prompt.
- `prompt` — strict: only read-only commands auto-run; everything else asks
- `deny` — everything denied

**Permission Checking** (`PermissionChecker::decide_tool`):
- Tool-level permissions via allow/deny lists (deny wins)
- `with_allowed_tools(&[&str]) -> Self` builder: adds tool names to the static
  allowlist so they are auto-approved without prompting (used by `build_agent`
  to pre-approve `write` and `edit` for agents with `auto_allow_writes: true`)
- Working modes (`InteractionMode`, Shift+Tab) sit on top of all of this: plan
  mode restricts first and accept-edits relaxes last (see Working modes under
  Agent System).
- In `prompt`/`agent` modes, read-only tools (read, glob, grep, and todo_write,
  which only edits the model's own task list) run without
  asking; bash commands are matched against
  `allowed_commands`/`denied_commands`; everything else asks the user via an
  arrow picker modal: **Yes once / Always this session / No**
- Bash matching (`check_command`): the denylist is authoritative (deny wins);
  the explicit allowlist wins next — a command runs when *every* command head
  is allowlisted, even network commands or caution-classified ones; then the
  semantic analyzer classifies: Safe → auto-allowed, Caution → auto-allowed in
  `agent` mode / prompted in `prompt` mode, Destructive → always prompted.
  Network commands prompt unless allowlisted, or unless the opt-in
  `permissions.allow_network_fetch` is on in `agent` mode — then GET-style
  fetches that only write to stdout (`curl URL`, `wget -O- URL`) auto-run;
  file-writing, upload, and custom-method variants still prompt. Privilege
  escalation (`sudo`/`su`/`pkexec`) always prompts unless explicitly
  allowlisted, and `sudo` is classified by the command it elevates
  (`sudo rm -rf /` is destructive, not caution). Shell
  expansions (`$`, backtick, `(`) prompt unless every head (including inside
  `$(...)`) is statically read-only in `agent` mode.
- Streaming turns ask via `AgentEvent::PermissionRequest` (answered by the REPL
  key listener); non-streaming turns use the agent's `set_prompt_fn` callback
  (single-shot mode reads from stdin). With no way to ask, the call is denied.
- "Always" answers are recorded per-session via `allow_for_session`: for bash
  tools, approval is granular per-command (e.g., "always" for `ls -la` won't
  approve `rm -rf`); for other tools, approval applies to all uses of that tool
- Sub-agents spawned via `spawn_agent` share the parent's checker (same
  deny/allow lists and session approvals) and fail closed on anything that
  would prompt. In `agent` mode the spawn itself is delegated to the agent —
  no user prompt — since sub-agents cannot do more than the parent could.

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
- Semantic auto-allow: `command_safety.rs` classifies every bash command as `Safe / Caution / Destructive`; provably-safe simple commands (`ls`, `git status`, `npm test`, `go build`, `pip list`, …) are auto-allowed in prompt mode without needing an explicit allowlist entry
- Expansion guard: semantic auto-allow is suppressed for any command containing `$`, backtick, or `(` — those require runtime evaluation that static analysis cannot classify safely

**Semantic Safety Tiers** (`command_safety.rs`):
- `Safe` → auto-allowed: read-only filesystem ops, system info, build-tool test/check/lint/doc subcommands across all ecosystems (cargo, npm/yarn/pnpm/bun, go, pip, mvn/gradle, dotnet, …), read-only git subcommands
- `Caution` → auto-allowed in `agent` mode / prompted in `prompt` mode: file mutations (mv/cp/rm), package installs, git write ops, ecosystem deploy/publish subcommands
- `Destructive` → always prompts: `rm -rf`, `dd`, `git reset --hard`, `git push --force`, `mkfs.*`, `shred`

**Known Limitations** (by design):
- Command arguments are not validated (e.g., allowing `ls` permits `ls -rf /`)
- Single-quoted strings don't trigger denial (correct shell behavior)
- Session approvals are exact-match only (prevents command variation bypasses)
- `make`/`cmake`/`ninja` targets are always Caution — Makefile rules are opaque to static analysis

See `SECURITY_ANALYSIS.md` for comprehensive test coverage and detailed findings.

### Configuration System (io-runtime/src/config.rs)

Configuration is loaded from TOML files with the following hierarchy:

1. Global config: `~/.io/config.toml`
2. Project config: `.io/config.toml` (if exists)
3. API keys: `~/.io/keys.toml` (chmod 600)
4. OAuth tokens: `~/.io/oauth.toml` (chmod 600)

**Schema auto-upgrade**: when the global config file lags the current `Config`
schema (a key is missing — e.g. a new option added in a release), `Config::load`
rewrites the file in place, filling missing keys from the defaults. The rewrite
is strictly additive: existing values (including inline `api_key` entries) are
never overwritten, unknown keys survive, and optional provider sections the
user omitted are not resurrected. OAuth tokens and the key store live in
separate files and are never touched by a config rewrite. A current-schema
file is never rewritten (comments stay intact).

**Config Structure**:
```toml
[provider]
default = "anthropic"

[provider.anthropic]
model = "claude-sonnet-4-20250514"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"
prompt_cache = true           # mark system prompt + conversation prefix cacheable

[session]
auto_compact = true
memory_enabled = true
max_turns = 100

[thinking]
enabled = false               # reason before answering on every turn (/think, Alt+T)
budget_tokens = 8000          # token budget for that reasoning (min 1024)
keywords = true               # think / think hard / ultrathink raise it for one turn

[permissions]
default = "agent"   # "allow" | "agent" | "prompt" | "deny"
allowed_commands = []
denied_commands = ["rm", "sudo"]
allow_network_fetch = false   # agent mode: auto-run read-only curl/wget fetches

# Hooks — global config only. Edit the file by hand (not settable via `io config set`).
[[hooks.pre_tool_use]]
matcher = "bash"              # tool names joined by "|"; omit or "*" = all tools
command = "./scripts/guard.sh"   # exit 2 blocks the call; stderr is shown to the model
timeout_ms = 10000            # optional, default 30000

[[hooks.post_tool_use]]
matcher = "write|edit"
command = "cargo fmt"         # stdout is appended to the tool result

[[hooks.user_prompt_submit]]
command = "git status --short"   # stdout is added to the prompt as context; exit 2 rejects it

[[hooks.stop]]
command = "notify-send 'io finished'"

# Also: session_start, session_end, notification, pre_compact (same shape).
```

### Theme System (io/src/render.rs + io/src/theme.rs)

The terminal has 12 built-in themes with configurable accent colors and dark/light
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
| `dawn` | light | DarkRed |
| `sand` | light | DarkYellow |
| `mint` | light | DarkGreen |
| `dusk` | light | DarkMagenta |

The active theme is persisted in `~/.io/config.toml` via the `theme` key.
Switch with `/theme` in the REPL or `io config set theme <name>`.

**Theme-aware rendering**: `render_thoughts(thoughts, theme)` uses `theme.accent` for
the `(thought):` prefix and `theme.muted` for the body. The `ansi_fg(Color) -> String`
helper in `render.rs` converts any crossterm `Color` to its ANSI foreground escape
sequence — used to embed theme colors into pre-rendered lines stored in the scroll buffer.

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
  `session.max_turns`, `session.max_tokens`, `permissions.default`,
  `permissions.allow_network_fetch`, `permissions.allowed_commands`,
  `permissions.denied_commands`, `thinking.enabled`, `thinking.budget_tokens`,
  `thinking.keywords`, `theme`
- `io mcp {add|list|get|remove}` - MCP server management (see **MCP** above)
- `io init` - Initialize project-level config

**TUI slash commands**: `/help`, `/new`, `/agent`, `/connect`, `/model`, `/theme`, `/cost`, `/compact`, `/mcp` (`/mcp reconnect`), `/rewind` (also triggered by pressing Esc twice within 500 ms at the idle prompt), `/plan` (toggle plan mode), `/think [on|off|low|medium|high|<tokens>]` (persists to `[thinking]`), `/exit` (also `/quit`, `/q`).
Key bindings: Shift+Tab cycles the working mode, Alt+T toggles extended thinking,
Ctrl+V attaches a clipboard image (`pngpaste`, `wl-paste` or `xclip`), Tab at the
empty prompt cycles agents. A pasted path to an image file (drag-and-drop) is
turned into an `@path` mention.
Switching agent, provider, or model mid-conversation keeps the current session
(`SessionChoice::Existing` in `build_agent`) — history is preserved.

`build_agent` in `io/src/tui.rs` takes `&io_agents::AgentConfig` (not a string), calls
`load_project_context()` from the project root, and applies `with_allowed_tools(&["write", "edit"])`
to the `PermissionChecker` when `agent_config.auto_allow_writes` is `true`.

**@file mentions**: Typing `@path/to/file` expands the file or directory contents inline
before sending to the LLM. Supports text files (up to 100KB) and directories (lists entries).
`@"path with spaces"` is accepted. Image files are not inlined: `resolve_at_mentions_with_images`
returns them separately and they are attached to the message as vision input.

### TUI Loop (io/src/tui.rs + input.rs + stream.rs)

`tui.rs` is the REPL orchestrator — agent construction, slash command dispatch,
the streaming turn loop, and `run_interactive` / `run_single_shot`. It is split
into two focused helpers:

- **`input.rs`** — `tui_read_line` (REPL) and `splash_read_line` (splash screen):
  key event loops, cursor movement, bracketed paste, @file popups, slash-command
  popups, `PasteBlock`, `resolve_at_mentions`.
- **`stream.rs`** — `blink_and_print`, `process_ev`, `ThinkParser`, `push_line`,
  and the shared `LineBuf` / `PendingPermission` types.

The interactive REPL:

1. Load/create session
2. Initialize agent with provider, tools, permissions
3. Enter read-eval-print loop:
   - Read user input via `tui_read_line` / `splash_read_line`
   - Handle slash commands (/help, /connect, /model, /cost, /exit)
   - Execute agent turn with streaming via `blink_and_print`
   - Display tool calls and results inline
   - Save session after each turn

Tab key at the empty prompt cycles through available full agents (build, plan,
debug, refactor) without needing the `/agent` command.

### Full-Screen TUI

The interactive REPL uses an alternate-screen TUI with:

- **Splash screen**: Centered logo, input box with placeholder, agent/model/provider
  status line, commands reference, cwd/version footer. Agent cycling with Tab.
  The input box smart-resizes: `splash_box_width` grows it with the typed
  input (up to the terminal width, minus margin) instead of truncating —
  `input.rs`'s `sync_splash_input` does a full `draw_splash` redraw whenever
  the required width crosses the current box width, otherwise a cheap
  single-line `splash_update_input`. Only once content still doesn't fit at
  the terminal-width cap does it fall back to `…` truncation;
  `splash_cursor` accounts for that truncation offset.
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
- If `project_context` is set: a synthetic user message containing the
  `<project-context>` block followed by a synthetic assistant acknowledgement —
  these are injected once per turn so the model always has project guidance
  available without it consuming the system prompt
- Prior turns replayed as user/assistant text only (see the replay policy
  under Agent System — tool traffic is not replayed across turns)
- The current user input

### Project Context Loading

`load_project_context(root: &Path) -> Option<String>` (public, in `io-runtime/src/lib.rs`):
- Reads `AGENTS.md` and `CLAUDE.md` from the project root (current working directory)
- Combines non-empty files into a single `<project-context>` block
- Returns `None` if neither file exists or both are empty
- Called by `build_agent` in `io/src/tui.rs` and forwarded to `SpawnAgentTool`
  so sub-agents see the same context as the parent
- All 12 built-in agent system prompts include the instruction: _"If you are
  unsure about project conventions or what exactly to do, read AGENTS.md or
  CLAUDE.md at the project root for guidance."_

### Error Recovery

- Session load failures create new sessions
- Tool execution failures return error messages
- Provider failures propagate with context
- Configuration errors provide actionable messages

## Testing

The project has 434 unit tests (393 in `io-runtime`, 31 in `io`, 9 in `io-tui`, 1 in
`io-agents`) plus 66 integration tests (14 in `io-runtime/tests/mcp.rs`, described below, and 52 in
`io-runtime/tests/agent_loop.rs` — full agent-loop runs
against a scripted mock provider, covering tool execution, permission
prompting/denial, streaming events, usage tracking, session resumption,
sub-agent permission inheritance, partial-progress persistence on
mid-turn provider failure, auto-compact triggering — including a second
compaction pass folding the prior summary into the new one, and cache reads
counting toward the compaction threshold — a rewind that restores a file and
truncates the session, and hooks: pre-tool veto that permissions can't
override, fail-open on a crashing hook, post-tool feedback, prompt rejection
and context injection, stop-hook firing only on success, and sub-agents
inheriting tool hooks, plus the Phase 2 hooks (session_start once per session with context, session_end, notification on permission prompts, pre_compact cancel/guidance, JSON `ask`/`block` decisions, `allow` being a no-op); and for the Phase 1 features: edit-before-read
and stale-file refusals, multi_edit rewind, todo_write, the plan-mode flow —
edits blocked until the plan is approved, approve-once vs accept-edits, rejection,
read-only research, exit_plan_mode outside plan mode — accept-edits mode, tool-returned
and attached images, and extended thinking: budgets, keywords, signed-block replay).
Run with `cargo test`.

| Module | Tests |
|---|---|
| `command_safety.rs` | Safe/Caution/Destructive classification for rm, find, git, cargo, sed, chmod, mkfs.*; ecosystem-agnostic build tools (npm, yarn, pnpm, bun, go, pip, mvn, gradle, dotnet, …); compound pipeline worst-case; expansion-free guard |
| `tools/read.rs` | missing arg, nonexistent file, content, offset, limit |
| `tools/write.rs` | missing args, create new file, overwrite + diff, pre-edit snapshot capture (new/overwrite/oversized) |
| `tools/edit.rs` | missing args, nonexistent file, not found, replace first, pre-edit snapshot capture |
| `rewind.rs` | restore to an earlier turn, delete a file that didn't exist before the reverted range, report a file with no snapshot as skipped, out-of-range turn index |
| `tools/bash.rs` | missing arg, stdout, nonzero exit, timeout |
| `tools/glob.rs` | missing arg, finds files, no matches, invalid pattern |
| `tools/grep.rs` | missing arg, invalid regex, matches with line numbers, no matches |
| `tools/fetch.rs` | missing url, invalid URL, unsupported scheme, blocked localhost/private/link-local/metadata hosts, response truncation |
| `sandbox.rs` | allow/deny/prompt modes, denylist, allowlist requires every command head, env-assignment skipping, safe-command auto-allow, decide_tool prompting, session approvals, fetch-tool network lenience |
| `provider/mod.rs` | retry classification by HTTP status, unknown-provider rejection, compat provider resolution, context-window tuning, `system_text()` collection, `Usage::context_tokens()`, `Usage::from_prompt_total` (split, no-cache, clamped) |
| `provider/anthropic.rs` | system prompt sent from a system message, cache markers (system + last message block, none elsewhere), caching off = no markers, streaming/blocking bodies identical, last-tool breakpoint when no system, cache usage parsing (stream + blocking, nulls) |
| `provider/gemini.rs` | system message sent as `systemInstruction`; `cachedContentTokenCount` split out of the prompt count (blocking + streaming, absent = uncached) |
| `provider/openai.rs` | `cached_tokens` split out of `prompt_tokens` (blocking + streaming); missing/null/empty details = uncached |
| `hooks.rs` | matcher, TOML parsing, exit-2 block (stderr reason), fail-open on other exits/timeouts, JSON stdin + env, cwd, post/prompt feedback, output cap, large-stdin no-deadlock, sub-agent hook subset |
| `hooks.rs` (events) | session gate (fires once, reset), session_end, notify, pre_compact exit-2/guidance, JSON decisions (`ask`, `deny`, `allow`, `continue:false`), `*` glob matchers incl. `mcp__server__*`, config merge |
| `mcp/*` | config parsing + inference, `${VAR}` expansion, name validation, 0600 writes, trust store (per project + fingerprint), SSE parser, JSON-RPC router, result parsing + size cap, schema normalisation, trust partition |
| `io-runtime/tests/mcp.rs` | against a Python mock MCP server (needs `python3`): stdio, env, HTTP JSON/SSE replies, headers + 401, legacy SSE, multi-server, slow-tool timeout, server crash, startup failures with stderr, agent sees tools, allow/deny rules, plan mode honouring `readOnlyHint` |
| `io/src/mcp_cmd.rs` | `--env`/`--header` parsing, building stdio/http/sse configs from CLI args, rejecting mismatched options, secret masking |
| `sandbox.rs` (MCP) | `mcp__server`, `mcp__server__*` and exact rules; deny wins; default prompt; plan mode + `readOnlyHint` |
| `config.rs` | default config, roundtrip serialization, schema upgrade (incl. `prompt_cache` / `[hooks]` / `[thinking]`), hooks + prompt_cache + thinking parsing |
| `tools/file_state.rs` | unread / unchanged / stale / re-recorded / forgotten paths |
| `tools/{edit,write,multi_edit,notebook}.rs` | read-before-edit and stale-file refusals, `replace_all`, all-or-nothing multi-edit, notebook replace/insert/delete by id and index, Jupyter-style serialization |
| `tools/read.rs` | image sniffing + dimensions + size limit, PDF text/page ranges (fixture PDF built in the test), notebooks, binary files, tracker recording |
| `tools/{html,fetch,web_search}.rs` | HTML→text, extraction via a canned model (success, failure fallback, no extractor), DuckDuckGo/Brave/Tavily parsing, domain filters, backend selection |
| `tools/{todo,plan}.rs`, `tools/mod.rs` | todo parsing/summary, plan validation, registry contents, tracker sharing within (not across) registries |
| `thinking.rs` | keyword tiers, bare-`think` heuristics, embedded-file exclusion, turn budget combination, `/think` argument parsing |
| `sandbox.rs` (modes) | mode cycle, plan-mode denials under every permission mode, read-only shell commands, `exit_plan_mode` prompting, accept-edits with guards, new tools' path rules, `web_search` knob |
| `provider/{anthropic,openai,gemini,bedrock}.rs` (thinking/images) | `thinking` parameter + clamping, signed-block replay, streamed thinking/signature events, reasoning effort + `max_completion_tokens`, image parts, Gemini thought parts and multi-part chunks |
| `io/src/{status,clipboard,input,tui}.rs` | badge text, pasted-image-path forms, `@image` / quoted mentions, permission-dialog reply mapping |
| `pricing.rs` | cost calculation (incl. cache read/write rates), per-provider cache rates (Anthropic/Bedrock-Claude/OpenAI/Gemini/unknown), known models, free/subscription/passthrough providers |
| `types.rs` | `TurnUsage` cost with cache traffic (Anthropic and OpenAI rates, unknown-model fallback), serde of cache fields (omitted at 0, old records load) |
| `io/src/stream.rs` | ThinkParser: plain text, think blocks, tags split across deltas, unterminated blocks, multibyte boundaries |
| `io-tui/src/render.rs` | `splash_box_width`: default width for short input, growth for long input, clamping at the terminal-width cap, narrow-terminal floor |

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
4. Register it in `tool_by_name()` / `BUILTIN_TOOLS` in `tools/mod.rs`
5. Export in `tools/mod.rs`
6. Test tool execution in agent context

### Modifying System Prompt

The system prompt comes from the active agent's `AgentConfig` in the `io-agents` crate (selected in `io/src/tui.rs`). It includes:
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

### Runtime-Specific Dependencies
- `pdf-extract` - PDF text extraction for the `read` tool

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
- `BRAVE_API_KEY` / `TAVILY_API_KEY` - optional `web_search` backends (DuckDuckGo needs none);
  `IO_SEARCH_BACKEND=duckduckgo|brave|tavily` forces one

Debug logging:
```bash
RUST_LOG=debug cargo run --bin io
RUST_LOG=io_runtime=trace cargo run --bin io
```

## License

MIT License - See LICENSE file for details.

## Maintaning the codebase

When maintaining the codebase, always:
1. Never make surgical edits; always read the whole file and then apply changes
2. Never follow conventional commits; always read past Git history and find a pattern to follow while committing
3. Always run the CI suite before reporting that the task is done
## Contributing

When contributing to io:
1. Follow existing code conventions
2. Add tests for new functionality
3. Update documentation as needed
4. Test with multiple providers when applicable
5. Ensure terminal UI interactions work correctly
