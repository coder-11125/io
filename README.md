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
- **13 built-in tools** — `read`, `write`, `edit`, `multi_edit`, `notebook_edit`, `bash`, `glob`, `grep`, `fetch`, `web_search`, `todo_write`, `exit_plan_mode`, `spawn_agent` for full codebase interaction
- **Task list** — the model plans multi-step work with `todo_write`; the checklist renders live in the scrollback
- **Plan mode** — Shift+Tab cycles *default → accept edits → plan*. Plan mode is read-only; the model researches, then proposes a plan with `exit_plan_mode` for you to approve
- **Safe editing** — files must be read before they are edited, and edits are refused if the file changed on disk since the read (by you, a formatter, another process)
- **Images & PDFs** — `read` shows images to vision models and extracts PDF text; attach images with `@shot.png`, drag-and-drop, or Ctrl+V from the clipboard
- **Web search** — `web_search` (DuckDuckGo out of the box; Brave or Tavily with an API key) with domain allow/block lists; `fetch` turns HTML into readable text and can answer a `prompt` about a page
- **Extended thinking** — `/think`, Alt+T or `[thinking]` config, plus `think` / `think hard` / `ultrathink` keywords, on Anthropic, Bedrock, Gemini and OpenAI reasoning models
- **MCP servers** — connect any Model Context Protocol server (stdio, streamable HTTP or SSE); its tools appear as `mcp__<server>__<tool>`. Claude-compatible `.mcp.json`, `io mcp add|list|get|remove`, `/mcp`
- **Hooks** — run your own shell commands around tool calls, prompts, turns, sessions, notifications and compaction; they can veto a call, ask for confirmation, or feed context back to the model
- **Rule-based permissions** — `allow` / `ask` / `deny` rules such as `Bash(npm run test:*)`, `Read(./.env)` and `WebFetch(domain:docs.rs)` in layered `settings.json` files (user, project, local, managed), plus `/permissions` and `--add-dir`
- **Memory** — `CLAUDE.md` / `AGENTS.md` at user, project, nested and local levels, `@import`s, `# note` quick-add, `/memory` and `/init`
- **OS sandbox** — optional bubblewrap (Linux) / Seatbelt (macOS) confinement for `bash`: read-only filesystem, hidden credentials, no network
- **Output styles & status line** — `/output-style` (concise, explanatory, learning, or your own) and a user-supplied status-line command
- **Headless mode** — `io -p "prompt"` with stdin piping, `--output-format text|json|stream-json`, `--max-turns`, `--allowedTools`, `--permission-mode`
- **Sessions** — `/resume`, `--resume <id>`, `/rename`, `/fork`, `/export`, `/clear`; git helpers `/review`, `/commit`, `/pr` and `io --worktree`
- **Prompt editing** — history (Up/Down, Ctrl+R), multi-line input, Vim mode, Ctrl+O transcript, Ctrl+B to background a command, messages queued while the model works
- **Cost tracking** — Built-in API cost calculation with `/cost` command for supported providers
- **Interactive & single-shot modes** — Full-screen REPL with splash screen, prompt bar, and scrollback
- **Color themes** — 12 built-in themes (dark/light) with live `/theme` switcher
- **Permission sandbox** — Allow/deny/prompt modes with granular bash command control and security-hardened command validation
- **Session persistence** — SQLite-backed conversation history, resume anytime
- **Smart rewind** — `/rewind` (or press Esc twice) restores files to an earlier point in the chat and truncates the session to match
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

# Opt into read-only network fetches in agent mode (curl to stdout, wget -O-,
# and the fetch tool). File-writing, upload, and custom-method network
# commands still prompt.
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
| `/new`, `/clear` | Start a new session |
| `/resume` | Go back to an earlier session (shows its last turns) |
| `/rename <name>` | Name this session |
| `/fork` | Branch this session into a copy and continue on it |
| `/export [file]` | Write this session to a Markdown file |
| `/agent` | Switch agent mode (build, plan, debug, refactor) |
| `/connect` | Set up a provider interactively (with live model fetching) |
| `/login` | Sign in with OAuth (ChatGPT / Claude) |
| `/model` | Switch between configured providers |
| `/theme` | Switch UI color theme (12 themes) |
| `/cost` | Show API cost summary for the current session |
| `/compact` | Summarize and compress conversation history |
| `/status` | Version, model, mode, settings files, rules, memory files, MCP, sandbox |
| `/context` | Where the context window is going (`/context refresh` re-fetches model limits) |
| `/todos` | The model's current task list |
| `/hooks` | Configured hooks |
| `/agents` | Built-in agents and sub-agents |
| `/permissions [allow\|ask\|deny <rule> \| remove <rule>]` | Show or edit permission rules |
| `/add-dir <dir>` | Let file tools reach another directory this session |
| `/memory` | Open a memory file (`CLAUDE.md`…) in `$VISUAL` / `$EDITOR` |
| `/init` | Have the agent analyze the project and write a `CLAUDE.md` |
| `/output-style [name]` | Change how the agent talks |
| `/review [PR number\|branch]` | Review uncommitted changes, a PR or a branch |
| `/commit [note]`, `/pr [note]` | Commit in this repository's own style; open a pull request |
| `/doctor` | Check the installation and environment |
| `/vim` | Toggle Vim-style prompt editing |
| `/logout [provider]` | Sign out of an OAuth login |
| `/usage` | Same as `/cost` |
| `/terminal-setup` | Multi-line input and image paste setup |
| `/bug` | How to report a problem |
| `/mcp` (`/mcp reconnect`) | List MCP servers and their status; restart them |
| `/rewind` (or press Esc twice) | Restore files to an earlier point in the chat |
| `/plan` | Enter or leave plan mode |
| `/think [on\|off\|low\|medium\|high\|<tokens>]` | Toggle extended thinking, or set its token budget |
| `/exit`, `/quit`, `/q` | Exit the session |
| `!<cmd>` | Run a shell command directly |

| Key | Action |
|---|---|
| Tab (empty prompt) | Cycle through the full agents |
| Shift+Tab | Cycle the working mode: default → accept edits → plan |
| Alt+T | Toggle extended thinking |
| Ctrl+V | Attach an image from the clipboard (needs `pngpaste`, `wl-paste` or `xclip`) |
| Esc Esc | Open `/rewind` |
| Up / Down | Browse earlier prompts |
| Ctrl+R | Search earlier prompts |
| Alt+Enter, Ctrl+J, `\` at end of line | New line (Shift+Enter too, in terminals with the kitty keyboard protocol) |
| Ctrl+O | Full transcript with tool calls (scrollable) |
| Ctrl+B | Move the running `bash` command to a background job |
| `# note` | Save a note to memory |

Anything typed while the model is working is queued and sent, in order, when
the turn finishes (Esc cancels the turn and drops the queue).

Type `@path/to/file` to expand file or directory contents inline; image files
(`@shot.png`, or `@"path with spaces.png"`) are attached to the message
instead. Dragging an image file into the terminal does the same.

**Working modes.** The mode shows in the status row. *Accept edits* approves
file changes (`write`, `edit`, `multi_edit`, `notebook_edit`) without asking —
the sensitive-path and project-root guards still apply. *Plan mode* is
read-only: file changes and anything but provably read-only shell commands are
refused, whatever `permissions.default` says. When the plan is ready the model
calls `exit_plan_mode` and you choose between *approve and accept edits*,
*approve and keep the edit prompts*, or *keep planning*.

**Extended thinking.** `/think` (or Alt+T, or `thinking.enabled = true`) makes
the model reason before answering, with a token budget (`/think 12000`,
`thinking.budget_tokens`). Independently, a keyword raises thinking for one
turn: `think` (4K tokens, only as an instruction such as "think about…"),
`think hard` / `megathink` (10K), `ultrathink` (32K). Turn the keywords off with
`thinking.keywords = false`. Anthropic and Bedrock use the `thinking` parameter,
Gemini 2.5+ `thinkingConfig`, and OpenAI o-series / GPT-5 `reasoning_effort`
(low / medium / high by budget); other providers ignore it. Reasoning is shown
as a dimmed `(thought)` block.

The TUI features a splash screen with centered logo and command reference,
a fixed prompt bar at the bottom showing agent/model/provider and context usage,
mouse scrollback through session history, and streaming tool call visualization
with syntax-colored diffs. The splash input box grows to fit longer prompts
(up to the terminal width) instead of truncating; only content that still
doesn't fit at that width falls back to `…` truncation. Thought blocks are
rendered in the active theme's accent and muted colors.

### Single-shot

```bash
io "summarize the changes in src/"
```

Runs one turn and prints the response, then exits.

### Headless

```bash
io -p "summarize the changes in src/"                  # prompt as an argument
git diff | io -p "review this diff"                    # piped input comes first
io -p "run the tests" --allowedTools "Bash(cargo test:*) Read" --max-turns 5
io -p "list the TODOs" --output-format json            # one JSON object with the result
io -p "fix the lint errors" --output-format stream-json # one JSON event per line
```

Nothing can answer a permission prompt in headless mode, so anything that would
ask is denied: pre-approve what the run needs with `--allowedTools`,
`settings.json` rules, or `--permission-mode acceptEdits|bypassPermissions`.
`json` output has `type`, `subtype` (`success`, `error_max_turns`,
`error_during_execution`), `is_error`, `result`, `session_id`, `num_tool_calls`,
`total_cost_usd` and `usage`; `stream-json` emits `system`/`init`, `text`,
`thinking`, `tool_use`, `tool_result`, `permission_denied`, `usage` events and a
final `result`. The exit code is 1 on error. With a prompt argument and a
silent open stdin (cron, CI) io stops waiting for piped input after 3 seconds.

### Flags

```bash
io --new                         # Start a fresh session (ignore history)
io --continue                    # Resume the last session
io --resume <id>                 # Resume a session by id or unique prefix
io --model anthropic             # Override the default provider
io --add-dir ../shared           # Let file tools reach another directory
io --permission-mode plan        # default | acceptEdits | plan | bypassPermissions
io --allowedTools "Bash(git log:*) Read"   # pre-approve rules
io --disallowedTools "Bash(rm:*)"          # refuse rules
io --append-system-prompt "Answer in French."
io --max-turns 10                # cap model calls per turn (default 20)
io --worktree [name]             # start in a new git worktree on its own branch
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

[thinking]
enabled = false         # reason before answering on every turn (/think, Alt+T)
budget_tokens = 8000    # token budget for that reasoning
keywords = true         # "think" / "think hard" / "ultrathink" raise it for one turn

# Hooks and MCP servers are described in their own sections below.

# Optional: theme = "ocean"  (default, ocean, rose, forest, sunset, mono, breeze, ink, dawn, sand, mint, dusk)
```

### Web search backend

`web_search` needs no setup (it uses DuckDuckGo's keyless HTML endpoint, which
may rate-limit). For a dependable backend export `BRAVE_API_KEY` (Brave Search)
or `TAVILY_API_KEY` (Tavily); `IO_SEARCH_BACKEND=duckduckgo|brave|tavily`
forces one. Like `fetch`, it prompts unless `permissions.allow_network_fetch`
is on in agent mode.

### Hooks

Hooks are shell commands that run (with `sh -c`, in the project root) at points
in the agent's life. They are read from the **global** `~/.io/config.toml` only —
a cloned repo can never plant a command that runs on your machine.

```toml
[[hooks.pre_tool_use]]
matcher = "bash|mcp__github__*"   # tool names joined by "|", `*` globs; omit = all tools
command = "./scripts/guard.sh"    # exit 2 blocks the call; stderr is shown to the model
timeout_ms = 10000                # optional, default 30000

[[hooks.post_tool_use]]
matcher = "write|edit"
command = "cargo fmt"             # stdout is appended to the tool result

[[hooks.user_prompt_submit]]
command = "git status --short"    # stdout becomes extra context; exit 2 rejects the prompt

[[hooks.stop]]                    # after a turn completes
command = "notify-send 'io finished'"

[[hooks.session_start]]           # once per session; stdout becomes context for the first prompt
command = "git log --oneline -5"

[[hooks.session_end]]             # /new, exit, or the end of a single-shot run
command = "./scripts/cleanup.sh"

[[hooks.notification]]            # fire-and-forget: io is waiting for a permission answer
command = "notify-send 'io needs you'"

[[hooks.pre_compact]]             # before /compact or auto-compact; exit 2 cancels,
command = "echo 'Keep the list of failing tests'"   # stdout guides the summary
```

Each hook receives a JSON description of the event on stdin (plus `IO_HOOK_EVENT`
and `IO_TOOL_NAME` in the environment). Exit `0` is success, exit `2` blocks, and
any other failure (crash, timeout) is logged and ignored, so a broken guard fails
open. A hook may instead print JSON on stdout:

| Field | Effect |
|---|---|
| `"decision": "block"` (or `"deny"`) + `"reason"` | Block, as with exit 2 |
| `"decision": "ask"` | Force a permission prompt even if the call would auto-run |
| `"decision": "allow"` | No effect — a hook can never grant permission the sandbox withholds |
| `"additionalContext"` | Extra text for the model |
| `"continue": false` + `"stopReason"` | Stop the turn |

`pre_tool_use` runs before the permission check and can only restrict. Sub-agents
inherit the parent's tool hooks, so delegating can't sidestep a guard.

### MCP servers

[Model Context Protocol](https://modelcontextprotocol.io) servers add tools from
outside io. Servers are configured in the Claude-compatible `mcpServers` format:

```json
{
  "mcpServers": {
    "github": { "type": "http", "url": "https://api.example.com/mcp",
                "headers": { "Authorization": "Bearer ${GITHUB_TOKEN}" } },
    "files":  { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
                "env": { "LOG": "${LOG_LEVEL:-info}" } }
  }
}
```

- **User scope** — `~/.io/mcp.json`; **project scope** — `.mcp.json` in the project root.
- `type` is `stdio`, `http` or `sse` (inferred from `command` / `url`). `${VAR}` and `${VAR:-default}` are expanded in commands, args, env, URLs and headers.
- **Project servers need approval.** `.mcp.json` is untrusted input that can name any command, so io asks once per server at startup (approve and remember / skip / never). Changing a server's definition asks again. Non-interactive runs skip unapproved servers.

```bash
io mcp add files npx -y @modelcontextprotocol/server-filesystem .   # stdio (options go before the name)
io mcp add -t http --scope project -H "Authorization: Bearer ${TOKEN}" api https://x.dev/mcp
io mcp add -e API_KEY=abc server ./server-binary
io mcp list      # connects and shows status / tool counts
io mcp get files # configuration, secrets masked
io mcp remove files
```

Tools are named `mcp__<server>__<tool>`. They prompt by default; control them with
`permissions.allowed_commands` / `denied_commands` — `mcp__github` or `mcp__github__*` covers a whole
server, `mcp__github__search_issues` one tool, and deny wins. In plan mode only
tools their server marks `readOnlyHint` run. Hooks see MCP calls like any other
tool. Timeouts: `IO_MCP_STARTUP_TIMEOUT_MS` (20 s) and `IO_MCP_TOOL_TIMEOUT_MS`
(60 s). `/mcp` lists servers; `/mcp reconnect` restarts them.

### settings.json and permission rules

Permission rules, the default working mode, output style, status line and the
bash sandbox live in `settings.json` files, merged lowest to highest precedence:

| Scope | File |
|---|---|
| user | `~/.io/settings.json` |
| project (shared) | `.io/settings.json` |
| local (personal) | `.io/settings.local.json` — add it to `.gitignore` |
| managed | `$IO_MANAGED_SETTINGS`, else `/etc/io/managed-settings.json` (`/Library/Application Support/io/managed-settings.json` on macOS) |

```json
{
  "permissions": {
    "allow": ["Bash(npm run test:*)", "Bash(git status)", "Edit(src/**)", "WebFetch(domain:docs.rs)"],
    "ask":   ["Bash(git push:*)"],
    "deny":  ["Read(./.env)", "Read(**/*.pem)", "Bash(rm:*)", "mcp__github__delete_repo"],
    "additionalDirectories": ["../shared"],
    "defaultMode": "acceptEdits"
  },
  "outputStyle": "concise",
  "editorMode": "vim",
  "statusLine": { "type": "command", "command": "~/.io/statusline.sh" },
  "sandbox": { "enabled": true, "network": false, "writablePaths": ["~/.cargo"] }
}
```

- **Rules** are `Tool` or `Tool(specifier)`. `Bash(prefix:*)` matches a command and its arguments at a word boundary, `Bash(exact command)` matches exactly, `*` is a wildcard. `Read(...)` covers `read`/`glob`/`grep` and `Edit(...)` (alias `Write`) covers `write`/`edit`/`multi_edit`/`notebook_edit`; paths are `./rel` or `/rel` (project root), `~/…`, `//absolute`, or a bare pattern like `*.env` that matches at any depth. `WebFetch(domain:x.com)` covers the domain and its subdomains. MCP tools: `mcp__server`, `mcp__server__*`, or an exact name.
- **Precedence is deny > ask > allow**, in every permission mode — `permissions.default = "allow"` still honours deny and ask rules. A deny or ask rule fires if it matches *any* part of a compound command (`a && b`, pipes, `$(…)`, backticks, env-prefixed or `/bin/`-qualified forms); an allow rule must cover *every* part and never applies to a command with expansions or redirections. Allow rules never lift plan mode, the sensitive-path or project-root guards, or `denied_commands`.
- **The shared project file is untrusted** (a cloned repository can ship one), so it may only *restrict*: `deny`/`ask` rules, an output style and `sandbox.enabled: true`. `allow` rules, `additionalDirectories`, `defaultMode`, `statusLine` and the rest of `sandbox` come from the user, local and managed files only. The managed file can also set `"allowManagedPermissionRulesOnly": true` to make its rules the only ones in force.
- `/permissions allow|ask|deny <rule>` and `/permissions remove <rule>` edit `settings.local.json`; `--add-dir` and `/add-dir` add directories for one run.

### Memory

io starts each conversation with your instructions: `~/.io/CLAUDE.md` (or
`AGENTS.md`), the project root's `AGENTS.md`, `CLAUDE.md` and `.io/CLAUDE.md`,
the same files in each directory down to where you started io, and
`CLAUDE.local.md` for personal notes (keep it out of version control). Files in
directories *below* the working directory are surfaced the first time the model
reads or edits a file there. A line like `@docs/style.md` imports another file
(five levels deep; code blocks and cycles are skipped); imports are confined —
project files can only import from the project, user files from your home
directory, and key/token/env files never. `# always run clippy` saves a note
(you pick which file), `/memory` opens one in your editor and `/init` has the
agent write a `CLAUDE.md` for the project.

### OS sandbox

With `"sandbox": {"enabled": true}` the `bash` tool (and sub-agents' bash) runs
under bubblewrap on Linux (`bwrap` must be installed) or Seatbelt on macOS: the
filesystem is read-only except the project, the temp directory and
`sandbox.writablePaths`; `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.io` and similar are
hidden; and there is no network unless `sandbox.network` is true. If the sandbox
is enabled but unavailable, bash refuses to run rather than running unconfined.
`io doctor` shows which.

### Output styles and status line

`/output-style` switches between `default`, `concise`, `explanatory` and
`learning`, or your own: a markdown file in `~/.io/output-styles/` or
`.io/output-styles/` (optional `name:`/`description:` front matter, the body is
added to the system prompt). `statusLine` runs a command before each prompt with
a JSON description of the session on stdin (`model`, `mode`, `cwd`, `context`, …);
its first output line replaces the `agent · model · provider` part of the status
row (escape sequences are stripped; two-second timeout).

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

# Health check (non-zero exit if something is broken), updates, completions
io doctor
io update [--install]
io completions bash|zsh|fish|powershell|elvish
io logout [openai|anthropic]

# Manage MCP servers
io mcp add <name> <command-or-url> …
io mcp list

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

# Name, export (Markdown), fork or delete one — <id> may be a unique prefix
io session rename <id> "refactor of the parser"
io session export <id> [file.md]
io session fork <id>
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
│       ├── status.rs            # Working mode + thinking state, status-row badge, /think
│       ├── clipboard.rs         # Clipboard image paste (Ctrl+V) and dropped image paths
│       ├── cost.rs              # /cost report
│       ├── config_cmd.rs        # `io config …` and `io init` handlers
│       ├── mcp_cmd.rs           # `io mcp …`, shared MCP manager, project-server trust prompt
│       ├── headless.rs          # `io -p`: one turn without the TUI, text/json/stream-json
│       ├── run_opts.rs          # Per-run flags (--allowedTools, --append-system-prompt, …)
│       ├── permissions.rs       # settings.json rules → PermissionChecker, /permissions, --add-dir
│       ├── memory_cmd.rs        # `# note` quick-add and /memory
│       ├── style_cmd.rs         # /output-style
│       ├── statusline.rs        # the user's status-line command
│       ├── session_cmd.rs       # /resume, /export, short ids, replay and transcript lines
│       ├── info_cmd.rs          # /status /context /hooks /agents /todos /bug reports
│       ├── doctor.rs            # `io doctor` / /doctor checks
│       ├── git_prompts.rs       # /review /commit /pr prompts
│       ├── worktree.rs          # `io --worktree`
│       ├── update.rs            # `io update`
│       ├── history.rs           # prompt history (Up/Down, Ctrl+R)
│       ├── vim.rs               # Vim-style prompt editing
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
│       ├── pager.rs             # Scrollable full-screen viewer (Ctrl+O transcript)
│       └── theme.rs             # Interactive theme picker
│
├── io-runtime/                  # Core engine (library crate)
│   ├── Cargo.toml
│   ├── tests/
│   │   ├── agent_loop.rs        # integration tests against a scripted mock provider
│   │   ├── mcp.rs               # MCP integration tests against a Python mock server
│   │   └── fixtures/            # mock_mcp_server.py
│   └── src/
│       ├── lib.rs               # Crate root — re-exports public API + load_project_context()
│       ├── agent.rs             # Agent loop: LLM completion + tool execution (sync & streaming)
│       ├── compact.rs           # /compact + auto-compact summarization
│       ├── rewind.rs            # /rewind — restore files to an earlier point in the chat
│       ├── hooks.rs             # Lifecycle hooks: shell commands around tool calls, prompts, sessions
│       ├── settings.rs          # Layered settings.json (user/project/local/managed)
│       ├── rules.rs             # allow/ask/deny rules: Bash(…), Read(…), Edit(…), WebFetch(domain:…)
│       ├── instructions.rs      # Memory files: CLAUDE.md/AGENTS.md discovery, @imports, nested loading
│       ├── os_sandbox.rs        # bubblewrap / Seatbelt wrapper for bash
│       ├── output_style.rs      # Output styles (built-in and custom markdown)
│       ├── export.rs            # Session → Markdown
│       ├── mcp/                 # MCP client: config + trust, stdio/HTTP/SSE transports, tools
│       ├── config.rs            # Config/schema (TOML), KeyStore, provider config structs
│       ├── types.rs             # Core data types — Session, Turn, ToolCallRecord, TurnUsage
│       ├── memory.rs            # SQLite-backed session persistence (CRUD)
│       ├── sandbox.rs           # Permission checker (allow/deny/prompt modes) + working modes (plan/accept edits)
│       ├── thinking.rs          # Extended thinking: per-turn budget, keywords, /think parsing
│       ├── command_safety.rs    # Semantic command classifier (Safe/Caution/Destructive)
│       ├── pricing.rs           # Per-token cost calculation for supported providers
│       ├── tools/               # Built-in tools (each with unit tests)
│       │   ├── mod.rs           # Tool trait, ToolRegistry, default_registry()
│       │   ├── file_state.rs    # FileTracker: read-before-edit + external-change detection
│       │   ├── read.rs          # Read text (offset/limit), images, PDFs, notebooks
│       │   ├── write.rs         # Write/create files, returns unified diff
│       │   ├── edit.rs          # Replace text (first or all occurrences), returns unified diff
│       │   ├── multi_edit.rs    # Several edits to one file, all-or-nothing
│       │   ├── notebook.rs      # .ipynb rendering + notebook_edit (replace/insert/delete cells)
│       │   ├── bash.rs          # Execute shell commands with timeout & workdir
│       │   ├── glob.rs          # Find files by glob pattern (sorted by mtime)
│       │   ├── grep.rs          # Search file contents with regex
│       │   ├── fetch.rs         # HTTP(S) GET → readable text, optional extraction prompt
│       │   ├── html.rs          # HTML → text converter used by fetch and web_search
│       │   ├── web_search.rs    # Web search (DuckDuckGo / Brave / Tavily) with domain filters
│       │   ├── todo.rs          # todo_write — the model's task list
│       │   ├── plan.rs          # exit_plan_mode — present a plan for approval
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
| **`read`** | Read a file with optional `offset` (line number) and `limit`. Images (PNG/JPEG/GIF/WebP) are shown to the model; PDFs return page-tagged text (`pages` picks a range); `.ipynb` notebooks return cell by cell with outputs |
| **`write`** | Write or create a file. Existing files must have been read first. Returns a unified diff |
| **`edit`** | Replace the first (or, with `replace_all`, every) occurrence of `old_string` with `new_string`. The file must have been read and unchanged since. Returns a unified diff |
| **`multi_edit`** | Apply several edits to one file in order, all-or-nothing, in one write |
| **`notebook_edit`** | Replace, insert or delete a Jupyter notebook cell by id or index |
| **`bash`** | Execute shell commands with configurable `timeout` (ms) and `workdir` |
| **`glob`** | Find files by glob pattern, sorted by modification time (newest first) |
| **`grep`** | Search file contents with regex, optional file type filtering via `include` glob |
| **`fetch`** | Fetch a URL via HTTP(S) GET. HTML becomes readable text; pass `prompt` to get just the answer to a question about the page. Truncated past 100 KB. Blocks localhost/private/link-local hosts |
| **`web_search`** | Search the web; `allowed_domains` / `blocked_domains` filter results |
| **`todo_write`** | Create and update the task list shown live in the TUI |
| **`exit_plan_mode`** | Present a plan for approval while plan mode is active |
| **`spawn_agent`** | Delegate a scoped task to a restricted sub-agent |

MCP servers contribute further tools named `mcp__<server>__<tool>`.

All tools implement the `Tool` trait with a JSON input schema exposed to the LLM, allowing autonomous discovery and use of parameters.

## Development

```bash
# Build
cargo build

# Run in development
cargo run -- "your prompt"
cargo run --

# Run tests (551 unit tests + 69 integration tests; the MCP tests need `python3`)
cargo test

# Add a new provider
# 1. Add a config struct in io-runtime/src/config.rs
# 2. Create a provider module in io-runtime/src/provider/
# 3. Add it to ProviderKind enum and create_provider() in provider/mod.rs
# 4. Add it to PROVIDERS array and match block in connect.rs
```

### Adding a New Tool

1. Create `io-runtime/src/tools/<name>.rs` implementing the `Tool` trait
2. Add it to `tool_by_name()` and `BUILTIN_TOOLS` in `tools/mod.rs`
3. The LLM will discover it automatically via tool specs

## License

MIT
