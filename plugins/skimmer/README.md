# Skimmer

Codebase orientation agent for [Claude Code](https://docs.anthropic.com/en/docs/claude-code) — powered by [rskim](https://github.com/dean0x/skim).

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## What it does

Skimmer is a Claude Code plugin that orients you in unfamiliar codebases. It uses `rskim` (tree-sitter) to extract code structure without implementation noise, then searches for task-relevant files and patterns. The result is a structured orientation report: project type, directory layout, relevant files, key functions, integration points, and a suggested approach.

## Install

1. **Add the marketplace** (registers the plugin catalog):

   ```
   /plugin marketplace add dean0x/skimmer
   ```

2. **Install the plugin**:

   ```
   /plugin install skimmer@dean0x-skimmer
   ```

## Usage

```
# Orient for a specific task
/skim add JWT authentication

# General codebase orientation
/skim
```

Claude also auto-invokes Skimmer when it detects you need codebase orientation — no command needed.

**With a task description**, Skimmer focuses its search on files, functions, and patterns relevant to that task and suggests where to integrate.

**Without arguments**, it produces a general orientation: project type, directory structure, key modules, and entry points.

## What you get

Skimmer generates a structured report:

| Section | Content |
|---------|---------|
| **Project Type** | Language, framework, dependencies |
| **Token Statistics** | Original vs skimmed token counts |
| **Directory Structure** | Source directories and their purpose |
| **Relevant Files** | Files matching the task with key exports |
| **Key Functions/Types** | Specific signatures related to the task |
| **Integration Points** | Where new code connects to existing code |
| **Patterns Observed** | Existing conventions to follow |
| **Suggested Approach** | Recommendation based on codebase structure |

## How it works

1. **Verify rskim** — checks `npx rskim --version` before starting
2. **Detect project type** from manifest files (`package.json`, `Cargo.toml`, etc.)
3. **Map source directories** and skim them with `npx rskim --mode structure`
4. **Search for task-relevant code** using Grep/Glob for file discovery, then `npx rskim --mode signatures` for structure extraction
5. **Identify integration points** — entry points, public exports, configuration patterns
6. **Generate the orientation report**

All source code reading goes through `rskim` — the agent never falls back to `cat` or `Read` for source files. If `rskim` isn't available, the agent reports the error and falls back to Glob/Grep for manual exploration.

## Requirements

`rskim` is invoked via `npx rskim` (auto-downloads if not installed). Alternatives:

- **npm** (global): `npm install -g rskim`
- **Cargo**: `cargo install rskim`

No configuration needed — the agent handles tool detection and fallback automatically.

## Links

- [skim](https://github.com/dean0x/skim) — main project (issues, contributions, full documentation)
- [rskim on npm](https://www.npmjs.com/package/rskim)
- [rskim on crates.io](https://crates.io/crates/rskim)

## License

[MIT](LICENSE)
