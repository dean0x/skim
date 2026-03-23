# Multi-Agent Hook Integration & Session Research

**Date:** 2026-03-23
**Issue:** #58
**Status:** Research Complete

---

## Part 1: Hook Integration

### Agent Catalog

Nine AI coding agents evaluated for hook integration with skim. Each agent assessed for hook mechanism, configuration format, MCP support, and integration priority.

#### 1. Claude Code

| Property | Value |
|----------|-------|
| Hook mechanism | Native `PreToolUse` / `PostToolUse` lifecycle hooks |
| Config format | JSON (`~/.claude/settings.json` or `.claude/settings.json`) |
| MCP support | Yes (full) |
| Priority | **P0 (launch)** |
| `skim init` recommendation | `skim init` (default, already implemented) |

**Details:** Claude Code's hook system fires shell commands at specific lifecycle events. `PreToolUse` hooks run before every Bash tool invocation, receiving JSON on stdin with `{ "tool_input": { "command": "..." } }`. The hook can modify the command via `updatedInput` in stdout JSON, or block execution by exiting with code 2. `PostToolUse` hooks run after tool completion for cleanup, formatting, or logging.

Configuration supports two scopes:
- **User-global:** `~/.claude/settings.json` (applies to all projects)
- **Project-local:** `.claude/settings.json` (version-controllable, shared with team)

Exit codes: `0` = allow/success, `2` = block (PreToolUse only, stderr message sent to Claude), other non-zero = non-blocking error shown to user.

**Current skim status:** Fully implemented. `skim init` installs `skim-rewrite.sh` as a PreToolUse hook. Security invariant: skim NEVER sets `permissionDecision` -- only `updatedInput`, letting Claude Code's permission system evaluate independently (unlike GRANITE which auto-approves with `permissionDecision: "allow"`).

---

#### 2. Gemini CLI

| Property | Value |
|----------|-------|
| Hook mechanism | Native `BeforeTool` / `AfterTool` lifecycle hooks |
| Config format | JSON (`~/.gemini/settings.json` or `.gemini/settings.json`) |
| MCP support | Yes (full) |
| Priority | **P0 (launch)** |
| `skim init` recommendation | `skim init --agent gemini` |

**Details:** Gemini CLI's hook system is architecturally identical to Claude Code's, with different event names. Hooks are scripts executed at predefined lifecycle points. Configuration merges from project (`.gemini/settings.json`) and user (`~/.gemini/settings.json`) layers, with project taking precedence.

Hook events:
- **`BeforeTool`** (equivalent to Claude's `PreToolUse`): Fires before tool execution. Can validate, modify (`hookSpecificOutput.tool_input`), or block (`decision: "deny"`) tool calls.
- **`AfterTool`** (equivalent to Claude's `PostToolUse`): Fires after tool execution. Can hide results, append context (`hookSpecificOutput.additionalContext`), or chain additional tool calls (`hookSpecificOutput.tailToolCallRequest`).

Input JSON includes `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `timestamp`, plus tool-specific fields (`tool_name`, `tool_input`, `mcp_context`).

Exit codes: `0` = success (stdout parsed as JSON), `2` = system block (stderr is rejection reason), other = warning (non-fatal).

Environment variables provided: `GEMINI_PROJECT_DIR`, `GEMINI_SESSION_ID`, `GEMINI_CWD`.

**Key difference from Claude Code:** Gemini uses `decision: "deny"` + `reason` instead of exit code 2 for blocking. The `matcher` field compares against tool names (e.g., `run_shell_command`, `read_file`) rather than Claude's broader `Bash` matcher.

---

#### 3. GitHub Copilot CLI

| Property | Value |
|----------|-------|
| Hook mechanism | Native `preToolUse` / `postToolUse` / `sessionStart` / `sessionEnd` hooks |
| Config format | JSON (`.github/hooks/*.json`) |
| MCP support | Yes (full -- ships with GitHub MCP server built-in) |
| Priority | **P1 (fast-follow)** |
| `skim init` recommendation | `skim init --agent copilot` |

**Details:** Copilot CLI (GA since February 2026) has a comprehensive hook system with six event types: `sessionStart`, `sessionEnd`, `userPromptSubmitted`, `preToolUse`, `postToolUse`, and `errorOccurred`. Hooks are stored as JSON files in `.github/hooks/*.json` within the repository.

Hook definition format:
```json
{
  "type": "command",
  "bash": "./scripts/hook-name.sh",
  "powershell": "./scripts/hook-name.ps1",
  "cwd": "scripts",
  "timeoutSec": 30
}
```

`preToolUse` hooks receive `toolName`, `toolArgs`, `timestamp`, `cwd` and can output `{"permissionDecision": "deny", "permissionDecisionReason": "..."}`. Multiple hooks of the same type execute sequentially in defined order.

**Notable:** Copilot CLI supports both bash and powershell hooks natively, unlike Claude Code and Gemini which are Unix-only. This is the only agent with Windows-native hook support.

---

#### 4. Codex CLI (OpenAI)

| Property | Value |
|----------|-------|
| Hook mechanism | Experimental hooks engine (`SessionStart`, `Stop`, `userPromptSubmit`) + notify events |
| Config format | TOML (`~/.codex/config.toml`) |
| MCP support | Yes (STDIO and streaming HTTP MCP servers) |
| Priority | **P1 (fast-follow)** |
| `skim init` recommendation | `skim init --agent codex` |

**Details:** Codex CLI has an experimental hooks engine with `SessionStart`, `Stop`, and `userPromptSubmit` events. The `userPromptSubmit` hook can block or augment prompts before execution. Additionally, the `notify` feature triggers external programs on `agent-turn-complete` events.

Configuration via `~/.codex/config.toml`. MCP servers can be added in config or managed with `codex mcp` CLI commands -- Codex launches them automatically when a session starts.

**Limitation:** No `preToolUse` equivalent exists yet. The hooks engine is marked experimental. The best integration path is instruction-based (via `AGENTS.md`) rather than command interception. GRANITE confirmed this limitation: their Codex adapter PR uses `AGENTS.md` injection, not hooks.

---

#### 5. Windsurf (Codeium / Cognition AI)

| Property | Value |
|----------|-------|
| Hook mechanism | Limited -- `on_model_response` hooks for auditing; `.windsurfrules` for instructions |
| Config format | `.windsurfrules` (plaintext at project root), `~/.codeium/windsurf/` for config |
| MCP support | Partial (via extensions) |
| Priority | **P2 (community request)** |
| `skim init` recommendation | `skim init --agent windsurf` |

**Details:** Windsurf's Cascade engine supports configuration hooks on model response (primarily for logging/auditing) but does not expose a `preToolUse`-equivalent hook for command interception. The primary customization mechanism is the `.windsurfrules` file at the project root, which tells Cascade about stack details, conventions, anti-patterns, and architecture.

Cascade is an agentic system that indexes the entire project, maintains codebase understanding, and can take multi-step actions autonomously. Enterprise deployments can place rules and workflows files on users' machines via MDM policies.

**Integration strategy:** Instruction track only. The `.windsurfrules` file can instruct Cascade to prefer `skim` commands, but there is no way to intercept and rewrite commands before execution. GRANITE's claimed Windsurf support is also instruction-based only.

---

#### 6. Cline / Roo Code

| Property | Value |
|----------|-------|
| Hook mechanism | No native hooks; MCP-first architecture |
| Config format | `.clinerules` (project root), MCP server configuration via settings panel |
| MCP support | **Yes (primary integration mechanism)** |
| Priority | **P1 (fast-follow)** |
| `skim init` recommendation | `skim init --agent cline` |

**Details:** Cline and its fork Roo Code are VS Code extensions with deep MCP integration. Cline has a dedicated MCP Marketplace for discovering and installing MCP servers. Roo Code supports MCP but requires manual configuration.

`.clinerules` files define granular permissions: which directories the AI can touch, which tools it can invoke, and which actions require human approval. For Roo Code specifically, `.roo-code/` stores task data.

**Integration strategy:** MCP track. The ideal skim integration for Cline/Roo Code is an MCP server that exposes skim's transformation capabilities as MCP tools. This avoids the fragile shell-hook approach and leverages the agent's native extensibility model.

**Why P1:** Cline has strong developer adoption and MCP-first architecture makes integration cleaner than instruction-based approaches. An MCP server also benefits any other MCP-native agent.

---

#### 7. OpenCode

| Property | Value |
|----------|-------|
| Hook mechanism | Plugin system with event hooks (JavaScript/TypeScript modules) |
| Config format | `~/.config/opencode/AGENTS.md` (global), `CLAUDE.md` fallback, TOML for config |
| MCP support | Yes (via plugins) |
| Priority | **P2 (community request)** |
| `skim init` recommendation | `skim init --agent opencode` |

**Details:** OpenCode uses a plugin-based architecture. Plugins are JavaScript/TypeScript modules in `.opencode/plugins/` or `~/.config/opencode/plugins/` that export hook functions. Each plugin receives a context object and returns a hooks object subscribing to events.

OpenCode supports Claude Code's file conventions as fallbacks: project rules in `CLAUDE.md`, global rules in `~/.claude/CLAUDE.md`. Permissions support `allow`, `ask`, and `deny` rules (last match wins).

**Integration strategy:** Plugin track (TypeScript module). GRANITE has an existing OpenCode TypeScript plugin (using the `tool.execute.before` hook). Skim can follow the same pattern with a `skim-opencode.ts` plugin.

---

#### 8. OpenClaw

| Property | Value |
|----------|-------|
| Hook mechanism | TypeScript hooks with `HOOK.md` manifest files |
| Config format | JSON (`openclaw.json`), hooks in `workspace/hooks/` or `~/.openclaw/hooks/` |
| MCP support | Yes |
| Priority | **P3 (research only)** |
| `skim init` recommendation | `skim init --agent openclaw` (deferred) |

**Details:** OpenClaw has 150+ CLI commands with a hook system that scans `workspace/hooks/` and `~/.openclaw/hooks/` for custom TypeScript hooks. Each hook requires a `HOOK.md` file. Built-in hooks include `boot-md`, `bootstrap-extra-files`, `command-logger`, and `session-memory`.

Hooks can be enabled/disabled via CLI commands (`openclaw hooks enable/disable <name>`). Configuration in JSON format specifies enabled status and file paths.

**Integration strategy:** Hook track (TypeScript). However, OpenClaw's market share is small and the effort to maintain a separate hook is not justified at launch. Defer to P3 and evaluate community demand.

---

#### 9. Aider

| Property | Value |
|----------|-------|
| Hook mechanism | No hook system |
| Config format | `.aider.conf.yml` (YAML), `.env` for environment variables |
| MCP support | No |
| Priority | **P3 (research only)** |
| `skim init` recommendation | Not applicable (instruction-only) |

**Details:** Aider is a terminal-based AI pair programming tool that operates through conversational commands. It has no hook system, no MCP support, and no command interception mechanism. Configuration is via `.aider.conf.yml` or environment variables.

**Integration strategy:** None viable. Aider operates in a fundamentally different paradigm (interactive chat) where command interception is not applicable. Users can manually use skim as a pipe tool (`skim file.ts | aider`), but automated integration is not feasible.

---

### Integration Strategy: Three-Track Approach

```
                    skim init --agent <name>
                            |
            +---------------+---------------+
            |               |               |
       Hook Track       MCP Track      Instruction Track
            |               |               |
    +-------+-------+   +--+--+     +------+------+
    |       |       |   |     |     |      |      |
  Claude  Gemini  Copilot Cline  Roo   Cursor  Windsurf
  Code    CLI     CLI           Code
    |                           |
  Codex (hybrid: AGENTS.md     OpenCode (plugin)
   + future hooks)             OpenClaw (deferred)
```

#### Track 1: Hook Track (Native Hook APIs)

**Agents:** Claude Code, Gemini CLI, Copilot CLI
**Mechanism:** Shell scripts registered in agent's settings.json/hooks directory
**Effort:** Low (3 variants of same pattern)
**Reliability:** High (agent-managed lifecycle)

All three agents share nearly identical hook architectures:
1. Agent fires lifecycle event with JSON on stdin
2. Hook script reads JSON, extracts command
3. Hook calls `skim rewrite --hook` with the command
4. Hook outputs JSON with rewritten command (or passes through)

The differences are minor:
- **Claude Code:** `PreToolUse` event, matcher `Bash`, output `updatedInput`
- **Gemini CLI:** `BeforeTool` event, matcher `run_shell_command`, output `hookSpecificOutput.tool_input`
- **Copilot CLI:** `preToolUse` event, stored in `.github/hooks/*.json`, output `updatedInput`

**Implementation:** Single `skim rewrite --hook --format <agent>` flag to emit agent-specific JSON output format. Three thin shell scripts, each ~30 lines.

#### Track 2: MCP Track (MCP-Native Agents)

**Agents:** Cline, Roo Code (primary); any MCP-supporting agent (secondary)
**Mechanism:** MCP server exposing skim tools
**Effort:** Medium (requires MCP server implementation)
**Reliability:** High (standard protocol)

An MCP server for skim would expose tools like:
- `skim_read` -- Read a file with structure/signature/type extraction
- `skim_rewrite` -- Rewrite a shell command for token efficiency
- `skim_stats` -- Show token statistics for a file

This is the most portable track: any agent supporting MCP can use it without agent-specific hooks.

#### Track 3: Instruction Track (Rules Files)

**Agents:** Cursor (`.cursor/rules/*.mdc`), Windsurf (`.windsurfrules`), Codex (`AGENTS.md`)
**Mechanism:** Agent instruction files that recommend skim usage
**Effort:** Minimal (text templates)
**Reliability:** Low (agent may ignore instructions)

For agents without hook APIs, the instruction track provides best-effort integration. `skim init --agent cursor` would create a `.cursor/rules/skim.mdc` file with rules like:

```markdown
---
description: Use skim for reading source files to reduce token usage
alwaysApply: true
---
When reading source code files, prefer using `skim <file>` instead of `cat <file>`.
For structure overview, use `skim <file> --mode=structure`.
For function signatures only, use `skim <file> --mode=signatures`.
```

This approach has ~60-85% adoption (based on GRANITE's suggest mode data) since the agent may choose not to follow instructions.

---

### Hook Format Examples

#### Claude Code Hook (PreToolUse)

**settings.json:**
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/Users/<you>/.claude/hooks/skim-rewrite.sh"
          }
        ]
      }
    ]
  }
}
```

**Input (stdin):**
```json
{
  "tool_input": {
    "command": "cat src/main.rs"
  }
}
```

**Output (stdout):**
```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "updatedInput": {
      "command": "skim src/main.rs"
    }
  }
}
```

#### Gemini CLI Hook (BeforeTool)

**settings.json:**
```json
{
  "hooks": {
    "BeforeTool": [
      {
        "matcher": "run_shell_command",
        "hooks": [
          {
            "type": "command",
            "command": "/Users/<you>/.gemini/hooks/skim-rewrite.sh",
            "name": "skim-rewrite",
            "timeout": 10000,
            "description": "Rewrite shell commands to use skim for token-efficient output"
          }
        ]
      }
    ]
  }
}
```

**Input (stdin):**
```json
{
  "session_id": "abc123",
  "transcript_path": "/Users/<you>/.gemini/tmp/<hash>/chats/session-001.jsonl",
  "cwd": "/path/to/project",
  "hook_event_name": "BeforeTool",
  "timestamp": "2026-03-23T10:00:00Z",
  "tool_name": "run_shell_command",
  "tool_input": {
    "command": "cat src/main.rs"
  }
}
```

**Output (stdout):**
```json
{
  "hookSpecificOutput": {
    "tool_input": {
      "command": "skim src/main.rs"
    }
  }
}
```

#### GitHub Copilot CLI Hook (preToolUse)

**.github/hooks/skim-rewrite.json:**
```json
{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "type": "command",
        "bash": ".github/hooks/skim-rewrite.sh",
        "powershell": ".github/hooks/skim-rewrite.ps1",
        "timeoutSec": 10
      }
    ]
  }
}
```

**Input (stdin):**
```json
{
  "toolName": "shell",
  "toolArgs": {
    "command": "cat src/main.rs"
  },
  "timestamp": "2026-03-23T10:00:00Z",
  "cwd": "/path/to/project"
}
```

**Output (stdout):**
```json
{
  "updatedInput": {
    "command": "skim src/main.rs"
  }
}
```

---

### `skim init` Recommendations

| Agent | Command | What It Creates | Track |
|-------|---------|-----------------|-------|
| Claude Code | `skim init` (default) | `~/.claude/hooks/skim-rewrite.sh` + patches `settings.json` | Hook |
| Gemini CLI | `skim init --agent gemini` | `~/.gemini/hooks/skim-rewrite.sh` + patches `settings.json` | Hook |
| Copilot CLI | `skim init --agent copilot` | `.github/hooks/skim-rewrite.json` + `.github/hooks/skim-rewrite.sh` | Hook |
| Codex CLI | `skim init --agent codex` | `AGENTS.md` section about skim + optional MCP config | Instruction |
| Cline | `skim init --agent cline` | MCP server config for skim tools | MCP |
| Roo Code | `skim init --agent roo` | MCP server config for skim tools | MCP |
| Cursor | `skim init --agent cursor` | `.cursor/rules/skim.mdc` | Instruction |
| Windsurf | `skim init --agent windsurf` | `.windsurfrules` section about skim | Instruction |
| OpenCode | `skim init --agent opencode` | `~/.config/opencode/plugins/skim.ts` | Plugin |
| OpenClaw | Deferred | -- | -- |
| Aider | Not applicable | -- | -- |

---

## Part 2: Session Storage Formats

### Format Matrix

| Agent | Session Path | Format | Tool Invocation Schema | Auto-Detection |
|-------|-------------|--------|----------------------|----------------|
| Claude Code | `~/.claude/projects/<slug>/*.jsonl` | JSONL | `tool_use` / `tool_result` in `message.content[]` | Directory name `projects/`, JSONL with `role`+`content` array |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | JSONL | `codex.tool_decision`, `codex.tool_result` event types | `rollout-` prefix, date-based directory structure |
| Gemini CLI | `~/.gemini/tmp/<hash>/chats/session-*.jsonl` | JSONL (migrating from JSON) | `type: "user" \| "gemini"` messages with tool call content | `session-` prefix under `chats/` directory |
| Copilot CLI | `~/.copilot/session-state/<id>/events.jsonl` | JSONL + YAML metadata | Timeline events with `toolName`, `toolArgs`, `resultType` | `events.jsonl` + `workspace.yaml` co-located |
| Cursor | `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` | SQLite | `composerData:<id>` and `bubbleId:<composerId>:<bubbleId>` keys in `cursorDiskKV` table | `state.vscdb` filename, `cursorDiskKV` table |
| Cline | VS Code globalStorage (task directories) | JSON | `api-conversation-history.json` with tool call blocks | Task directory structure with `cline-messages.json` |
| Roo Code | `.roo-code/tasks/<session-id>/` | JSON | `api-conversation-history.json`, `cline-messages.json`, `metadata.json` | `.roo-code/tasks/` directory pattern |
| Windsurf | `~/.codeium/windsurf/` | Proprietary (not documented) | Unknown | `.codeium/windsurf/` directory |
| OpenCode | `.opencode/` project directories | SQLite | Normalized conversations/messages schema | `.opencode/` directory with SQLite database |
| Aider | `.aider.chat.history.md` (project root) | Markdown | Human-readable chat log, no structured tool invocations | `.aider.chat.history.md` filename |
| OpenClaw | `~/.openclaw/sessions/` | JSONL | Session JSONL with command events | `~/.openclaw/` directory |

**Platform-specific paths for Cursor:**
- macOS: `~/Library/Application Support/Cursor/User/`
- Linux: `~/.config/Cursor/User/`
- Windows: `%APPDATA%\Cursor\User\`

---

### Per-Agent Details

#### Claude Code

**Path:** `~/.claude/projects/<project-slug>/` containing JSONL session files.

**Format:** Each line is a JSON object representing a message in the conversation. Messages have a `role` field (`user`, `assistant`) and a `content` array containing blocks.

**Tool invocation schema:**
```json
{
  "role": "assistant",
  "content": [
    {
      "type": "tool_use",
      "id": "toolu_abc123",
      "name": "Bash",
      "input": {
        "command": "git status"
      }
    }
  ]
}
```

Tool results appear as:
```json
{
  "role": "user",
  "content": [
    {
      "type": "tool_result",
      "tool_use_id": "toolu_abc123",
      "content": "On branch main\nnothing to commit"
    }
  ]
}
```

**Auto-detection:** Look for `~/.claude/projects/` directory. Files are JSONL. Presence of `tool_use` type in content blocks confirms Claude Code origin.

**Parser complexity:** Low. Well-structured JSONL with consistent schema. This is the best-documented and most accessible format.

---

#### Codex CLI (OpenAI)

**Path:** `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (date-partitioned). Also `~/.codex/history.jsonl` for aggregate history.

**Format:** JSONL with three top-level keys per line: `payload`, `timestamp`, and `type`.

**Event types:**
- `thread.started`, `turn.started`, `turn.completed`, `turn.failed`
- `item.*` (content items)
- `codex.tool_decision` -- approved/denied, config vs user decision
- `codex.tool_result` -- duration, success, output snippet
- `token_count` -- cumulative totals

**Auto-detection:** `~/.codex/sessions/` with date-based subdirectories. Files match `rollout-*.jsonl` pattern.

**Parser complexity:** Medium. The event-stream format requires state machine parsing to reconstruct tool invocation pairs (decision + result).

---

#### Gemini CLI

**Path:** `~/.gemini/tmp/<project_hash>/chats/session-*.jsonl` (migrating from `.json` to `.jsonl`).

**Format:** JSONL with typed records:
- `{ type: "session_metadata", ... }` -- session header
- `{ type: "user" | "gemini", id: "...", ... }` -- messages
- `{ type: "message_update", id: "...", ... }` -- granular updates (token counts, tool results)

The system reads existing `.json` files for backward compatibility but creates `.jsonl` for new sessions.

**Auto-detection:** `~/.gemini/tmp/` with hash-named subdirectories. `chats/` subdirectory contains `session-*` files.

**Parser complexity:** Medium. Must handle both old JSON format and new JSONL format. Tool invocations are embedded within message content.

---

#### GitHub Copilot CLI

**Path:** `~/.copilot/session-state/<session-id>/` with:
- `events.jsonl` -- full session history
- `workspace.yaml` -- metadata
- `plan.md` -- implementation plan (if created)
- `checkpoints/` -- compaction history
- `files/` -- persistent artifacts

**Format:** Dual-format storage. `events.jsonl` contains a JSON-based timeline of events. Large tool outputs written to disk files rather than inline (since v0.0.376).

**Auto-detection:** `~/.copilot/session-state/` directory. Each session has `events.jsonl` + `workspace.yaml` co-located.

**Parser complexity:** Medium. The timeline format requires understanding event ordering. External file references for large outputs add complexity.

---

#### Cursor

**Path:** `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` (macOS)

**Format:** SQLite database. Primary table is `cursorDiskKV` (key-value store).

**Key patterns:**
- `composer.composerData` -- primary chat list (current format)
- `composerData:<composerId>` -- individual session metadata
- `bubbleId:<composerId>:<bubbleId>` -- individual messages
- `workbench.panel.aichat.view.aichat.chatdata` -- legacy chat storage
- `aiService.prompts` / `aiService.generations` -- prompt and response history

**Auto-detection:** `state.vscdb` filename with `cursorDiskKV` table. Platform-specific paths.

**Parser complexity:** High. SQLite requires database access. Key-value schema means values are JSON-encoded strings that need secondary parsing. Multiple format versions (legacy vs current). Database can grow to 25GB+.

---

#### Cline / Roo Code

**Path (Cline):** VS Code globalStorage, task-based directories with `api-conversation-history.json` and `cline-messages.json`.

**Path (Roo Code):** `.roo-code/tasks/<session-id>/` containing:
- `api-conversation-history.json`
- `cline-messages.json`
- `metadata.json`

**Format:** JSON files with conversation history and message data.

**Auto-detection (Cline):** VS Code extension storage path with Cline-specific directory structure.
**Auto-detection (Roo Code):** `.roo-code/tasks/` directory in project root.

**Parser complexity:** Medium (Cline -- must locate VS Code extension storage), Low (Roo Code -- project-local files).

---

#### OpenCode

**Path:** `.opencode/` in project directory, containing SQLite database.

**Format:** SQLite with normalized schema: conversations and messages tables.

**Auto-detection:** `.opencode/` directory with SQLite database file.

**Parser complexity:** Medium. SQLite access required, but schema is normalized (unlike Cursor's key-value approach).

---

#### Windsurf

**Path:** `~/.codeium/windsurf/` for configuration; session data storage path not publicly documented.

**Format:** Proprietary. Cascade memories are auto-generated and stored internally. No public API for session access.

**Auto-detection:** `~/.codeium/windsurf/` directory exists.

**Parser complexity:** Very High / Infeasible. Undocumented proprietary format. Not a viable target for session parsing.

---

#### Aider

**Path:** `.aider.chat.history.md` in project root.

**Format:** Markdown. Human-readable chat log with user prompts and assistant responses. No structured tool invocation data.

**Auto-detection:** `.aider.chat.history.md` file in project root.

**Parser complexity:** Low for text extraction, but no structured tool invocation data makes it unsuitable for discover/learn commands that need tool call/result pairs.

---

### Auto-Detection Strategy

Session auto-detection should follow a priority-ordered scan:

```
1. Check well-known paths:
   ~/.claude/projects/         -> Claude Code
   ~/.codex/sessions/          -> Codex CLI
   ~/.gemini/tmp/              -> Gemini CLI
   ~/.copilot/session-state/   -> Copilot CLI

2. Check project-local paths:
   .roo-code/tasks/            -> Roo Code
   .opencode/                  -> OpenCode
   .aider.chat.history.md      -> Aider

3. Check platform-specific paths:
   ~/Library/Application Support/Cursor/User/ -> Cursor (macOS)
   ~/.config/Cursor/User/                     -> Cursor (Linux)
   %APPDATA%\Cursor\User\                     -> Cursor (Windows)

4. Check VS Code extension storage:
   Locate Cline extension globalStorage       -> Cline

5. Check speculative paths:
   ~/.codeium/windsurf/        -> Windsurf (limited data)
   ~/.openclaw/sessions/       -> OpenClaw
```

For each discovered agent, validate format by checking:
- JSONL files: First line parses as valid JSON
- SQLite files: Can open and query expected tables
- JSON files: Valid JSON with expected top-level keys
- Markdown files: Contains expected header patterns

---

### Parser Priority

Recommended implementation order based on format accessibility, user base size, and data richness:

| Priority | Agent | Rationale |
|----------|-------|-----------|
| **P0** | Claude Code | Primary user base, well-documented JSONL, structured tool_use/tool_result |
| **P1** | Codex CLI | Large OpenAI user base, JSONL format, structured events |
| **P1** | Gemini CLI | Growing user base, JSONL format (migrating), Google backing |
| **P1** | Copilot CLI | Massive GitHub user base, JSONL + metadata files |
| **P2** | Roo Code | Project-local JSON (easy access), growing community |
| **P2** | Cursor | Large user base, but SQLite + proprietary KV schema adds complexity |
| **P2** | OpenCode | SQLite but normalized schema, smaller user base |
| **P3** | Cline | VS Code extension storage (hard to locate reliably) |
| **P3** | Aider | Markdown only, no structured tool data |
| **P3** | Windsurf | Proprietary format, no public documentation |
| **P3** | OpenClaw | Small user base, limited documentation |

---

### CASS Tool Analysis

**CASS** (Coding Agent Session Search) by Dicklesworthstone is an existence proof of unified multi-agent session parsing. Key findings:

**What it does:** Rust-based TUI/CLI that indexes and searches local coding agent session history across 15+ agents. Normalizes all formats into a common schema: Conversation > Message > Snippet.

**Agents supported:** Claude Code, Codex CLI, Gemini CLI, Cursor, Cline, OpenCode, Amp, Aider, ChatGPT, Clawdbot, Vibe (Mistral), Pi-Agent, Factory (Droid), Copilot.

**Architecture:**
- "Universal connectors" normalize disparate formats into common schema
- Dual storage: SQLite for structured data, BM25 + semantic search for indexing
- JSONL connectors extract conversation/message structures directly
- SQLite connectors query state databases (Cursor's `state.vscdb`)
- Encrypted format handling for ChatGPT v2/v3

**Companion project:** CASS Memory System -- transforms scattered session history into persistent, cross-agent memory so every agent learns from every other.

**Relevance to skim:** CASS validates the feasibility of multi-agent session parsing. Its connector architecture (per-agent parser modules normalizing to common schema) maps directly to the `SessionProvider` trait pattern proposed for skim. Key lessons:
1. The common schema (Conversation > Message > Snippet) is proven workable
2. Format resilience matters: handle legacy formats (integer vs ISO timestamps)
3. Auto-discovery of 15+ agents is achievable
4. SQLite-based agents (Cursor, OpenCode) require more complex parsers
5. Some agents (ChatGPT) have encrypted formats that may not be worth supporting

---

## Part 3: GRANITE Competitive Analysis

### Multi-Agent Architecture

GRANITE's multi-agent support evolved reactively rather than architecturally. Key milestones:

1. **v0.1-v0.22:** Claude Code only. Hook system, rewrite engine, and all parsing hardcoded to Claude Code's PreToolUse format.

2. **v0.23-v0.28:** OpenCode added as second agent via TypeScript plugin (using `tool.execute.before` hook). Known limitation: subagent tool calls not intercepted.

3. **v0.29-v0.30:** Windsurf and Cline/Roo Code support added (March 2026). Both are instruction-based only (`.windsurfrules` / `.clinerules` templates).

4. **v0.31 (PR #704):** 9-tool AI agent support. Added `--agent <name>` flag to init command. Cursor support added with Cursor's preToolUse hook format (functionally identical to Claude Code's but different JSON output structure).

**Architecture pattern:** GRANITE uses a **flag-based agent detection** model. The `--agent` flag on `init` selects which hook script to install and which settings file to patch. There is no runtime agent detection -- the hook format is fixed at install time.

**Session tracking gap:** GRANITE's `discover` and `session` commands only parse Claude Code JSONL sessions. Cursor sessions are tracked via GRANITE's gain command (token savings) but lack structured tool_use/tool_result parsing. Other agents have no session support at all.

---

### Discover Implementation

GRANITE's `discover` module (`src/discover/`) scans Claude Code JSONL session files to identify commands that could have been compressed but were not.

**How it works:**
1. Locate Claude Code sessions at `~/.claude/projects/<slug>/*.jsonl`
2. Parse JSONL to extract `tool_use` blocks where `name == "Bash"`
3. Extract the `command` field from `tool_input`
4. Run each command through `classify_command()` against the 71-rule registry
5. Report: which commands are `Supported` (could be rewritten), `Unsupported` (no match), or `Ignored` (shell builtins)
6. Calculate estimated token savings

**Limitations:**
- Only parses Claude Code sessions (no other agents)
- Counts `cat >` (file writes) as `cat` (file reads), inflating savings estimates by ~16%
- Does not account for Claude Code's native tools (Read, Grep, Glob) which bypass Bash entirely
- No deduplication: same command repeated 100 times counts as 100 optimization opportunities

---

### Learn Implementation

GRANITE's `learn` module (`src/learn/`) detects CLI correction patterns across sessions and generates rules for agent self-improvement.

**Correction detection architecture:**
1. Parse Claude Code JSONL sessions chronologically
2. Identify "correction pairs": a failed command followed by a successful similar command
3. Extract the pattern (e.g., wrong flag, typo, missing argument)
4. Generate correction rules in `.claude/rules/cli-corrections.md`

**Output format:** Human-readable Markdown rules that Claude Code loads as project context. Example: "When running pytest, use `pytest -x` instead of `pytest --stop-on-first-failure`."

**Limitations:**
- Claude Code-centric: only parses Claude Code session format
- Heuristic correction detection: "similar command" matching is regex-based, prone to false positives
- No semantic understanding: cannot distinguish intentional command changes from corrections
- Leaks sensitive data: AWS resource IDs, account IDs, and paths with usernames appear in generated rules (Issue #651)

---

### Weaknesses

#### 1. Reactive Retrofit Architecture

GRANITE's multi-agent support was bolted on after a Claude Code-centric design hardened. Evidence:
- The `discover` and `learn` modules only work with Claude Code sessions
- The `session` command only parses Claude Code JSONL
- GRANITE's gain command tracks all agents but only for token counts, not session analysis
- PR #704 added 9-agent init support but zero session parsing for non-Claude agents

**Skim opportunity:** Design the `SessionProvider` trait from day one. All agents get equal treatment in discover/learn.

#### 2. Claude Code-Centric Session Parsing

GRANITE cannot discover optimization opportunities or learn corrections from Codex, Gemini, Copilot, Cursor, or any other agent. Users switching between agents lose all session intelligence.

**Skim opportunity:** Multi-agent session parsing is the differentiator. A developer using Claude Code at work and Codex at home should have unified discover/learn across both.

#### 3. Flag-Based Agent Detection (Fragile)

The `--agent` flag approach means the installed hook format is fixed. If a user switches agents or uses multiple agents in the same project, they must re-run `init`. There is no runtime detection.

**Skim opportunity:** Auto-detect the calling agent at runtime from environment variables or stdin format. Claude Code sets specific env vars; Gemini CLI sets `GEMINI_SESSION_ID`; Copilot CLI has distinctive JSON format.

#### 4. Chars/4 Token Estimation

GRANITE's token counting uses `ceil(text.len() / 4.0)` -- a crude character-based heuristic. This systematically overestimates savings for ASCII text and underestimates for Unicode-heavy content (CJK, emoji).

**Skim opportunity:** Skim already uses tiktoken (cl100k_base) for real tokenization. This is a clear accuracy advantage.

#### 5. Asymmetric Agent Support

Of 9 "supported" agents, only Claude Code and OpenCode have actual hook integrations. The remaining 7 are instruction-based templates with no command interception.

**Skim opportunity:** Be honest about support tiers. P0/P1/P2/P3 classification is more useful than claiming "9-agent support" when 7 of those are .rules file templates.

---

### What to Adopt

#### 1. Thin Delegator Pattern

GRANITE's hook architecture is well-designed: the shell script is a thin delegator (~30 lines) that extracts the command and calls the Rust binary. All rewrite logic lives in Rust. This pattern is correct and skim already follows it.

#### 2. Strategy Pattern for Rewrite

The rewrite engine's approach -- classify command, look up rule, apply rewrite -- is a clean strategy pattern. Skim's existing rewrite engine follows this same pattern.

#### 3. Telemetry at Boundary

GRANITE tracks token savings in SQLite after every command. This "fire-and-forget telemetry" approach (never blocking the command) is the right pattern. Skim's `--show-stats` feature provides similar data but could benefit from cumulative tracking.

#### 4. Graceful Degradation

GRANITE's three-tier model (Full > Degraded > Passthrough) with explicit markers (e.g., `[DEGRADED]`, `[PASSTHROUGH]`) is excellent UX. Users always know what quality of output they are getting. Skim's test parsers already implement this pattern.

#### 5. Init Uninstall

GRANITE's `init --uninstall` command cleanly removes hooks, config patches, and generated files. Skim should implement this (currently missing).

#### 6. Discover as Viral Adoption

The `discover` command that scans past sessions and shows "you could have saved X tokens" is a clever adoption mechanism. Skim should implement this for multi-agent sessions.

---

### What to Avoid

#### 1. Flag-Based Agent Detection

GRANITE's `--agent <name>` flag on `init` is static. Once installed, the hook format is fixed. This breaks when users switch agents or use multiple agents in the same project. Prefer runtime detection.

#### 2. Chars/4 Token Estimation

Never use character-based token estimation. Always use a real tokenizer. Skim already uses tiktoken -- maintain this advantage.

#### 3. Auto-Approve Permission Decisions

GRANITE's hook sets `permissionDecision: "allow"`, auto-approving rewritten commands without user confirmation. This is a security risk. Skim's approach of only setting `updatedInput` and letting the agent's permission system evaluate independently is the correct design.

#### 4. Over-Aggressive Filtering

GRANITE's biggest user complaint: compressed output that removes critical information, causing agent retry loops that cost MORE tokens than raw output. Issues #617, #618, #620, #582, #690 all stem from this. The iron law: **compressed output must always contain enough information for the agent to proceed. On failure, be more verbose, not less.**

#### 5. Emoji in CLI Output

GRANITE had to remove all emoji from CLI output (PR #704) because emoji consume extra tokens and confuse some LLMs. Skim should never add emoji to output intended for LLM consumption.

#### 6. Marketing Before Implementation

GRANITE claims "9-agent support" but only 2 agents have actual hook integrations. The remaining 7 are .rules file templates. This erodes user trust when expectations are not met. Be honest about support tiers.

---

## Appendix

### References

**Claude Code Hooks:**
- [Official docs: Automate workflows with hooks](https://code.claude.com/docs/en/hooks-guide)
- [Claude Code hooks tutorial](https://blakecrosley.com/blog/claude-code-hooks-tutorial)
- [DataCamp practical guide](https://www.datacamp.com/tutorial/claude-code-hooks)

**Gemini CLI Hooks:**
- [Google Developers Blog: Tailor Gemini CLI with hooks](https://developers.googleblog.com/tailor-gemini-cli-to-your-workflow-with-hooks/)
- [Gemini CLI hooks reference](https://geminicli.com/docs/hooks/reference/)
- [Writing hooks for Gemini CLI](https://geminicli.com/docs/hooks/writing-hooks/)
- [Gemini CLI configuration](https://github.com/google-gemini/gemini-cli/blob/main/docs/get-started/configuration.md)

**GitHub Copilot CLI:**
- [Hooks configuration reference](https://docs.github.com/en/copilot/reference/hooks-configuration)
- [Using hooks with Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/use-hooks)
- [Copilot CLI GA announcement](https://github.blog/changelog/2026-02-25-github-copilot-cli-is-now-generally-available/)

**Codex CLI:**
- [Codex CLI features](https://developers.openai.com/codex/cli/features)
- [Codex advanced configuration](https://developers.openai.com/codex/config-advanced)
- [Custom instructions with AGENTS.md](https://developers.openai.com/codex/guides/agents-md)

**Cursor:**
- [Cursor rules for AI](https://cursor.com/docs/context/rules)
- [cursor-db-mcp](https://github.com/TaylorChen/cursor-db-mcp) -- MCP server for querying Cursor conversation history

**Cline / Roo Code:**
- [Roo Code GitHub](https://github.com/RooCodeInc/Roo-Code)
- [Roo Code vs Cline comparison](https://www.qodo.ai/blog/roo-code-vs-cline/)

**Windsurf:**
- [Windsurf Cascade docs](https://docs.windsurf.com/windsurf/cascade/cascade)
- [Windsurf rules & workflows](https://www.paulmduvall.com/using-windsurf-rules-workflows-and-memories/)

**OpenCode:**
- [OpenCode rules](https://opencode.ai/docs/rules/)
- [OpenCode plugins](https://opencode.ai/docs/plugins/)

**OpenClaw:**
- [OpenClaw hooks docs](https://docs.openclaw.ai/automation/hooks)
- [OpenClaw configuration guide](https://moltfounders.com/openclaw-configuration)

**Aider:**
- [Aider configuration](https://aider.chat/docs/config.html)

**CASS:**
- [CASS GitHub](https://github.com/Dicklesworthstone/coding_agent_session_search)
- [CASS Memory System](https://github.com/Dicklesworthstone/cass_memory_system)

### GRANITE Source Analysis Documents

- `.docs/competitive/granite-full-analysis.md` -- Full competitive analysis
- `.docs/competitive/agent-02-source-code.md` -- Source code deep dive (discover/, learn/, session_cmd.rs)
- `.docs/competitive/agent-05-commit-history.md` -- Multi-agent sprint and PR #704 analysis
- `.docs/competitive/agent-06-hooks-integration.md` -- Hook/rewrite system deep dive
