//! Command rewrite engine (#43)
//!
//! Rewrites common developer commands into skim equivalents using a two-layer
//! rule system:
//!
//! **Layer 1 — Declarative prefix-swap table**: Ordered longest-prefix-first.
//! Each rule maps a command prefix (e.g. `["cargo", "test"]`) to a skim
//! equivalent (e.g. `["skim", "test", "cargo"]`), with optional skip-flags
//! that suppress the rewrite when present.
//!
//! **Layer 2 — Custom handlers**: For commands requiring argument inspection
//! (cat, head, tail) where simple prefix matching is insufficient.

use std::io::{self, BufRead, IsTerminal, Read};
use std::process::ExitCode;

use serde::Serialize;

// ============================================================================
// Data structures
// ============================================================================

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum RewriteCategory {
    Test,
    Build,
    Git,
    Read,
}

struct RewriteRule {
    prefix: &'static [&'static str],
    rewrite_to: &'static [&'static str],
    skip_if_flag_prefix: &'static [&'static str],
    category: RewriteCategory,
}

#[derive(Debug)]
struct RewriteResult {
    tokens: Vec<String>,
    category: RewriteCategory,
}

// ---- Compound command types (#45) ----

/// Result of splitting a shell command string at compound operators.
#[derive(Debug)]
enum CompoundSplitResult {
    /// No compound operators found — treat as a simple command.
    Simple(Vec<String>),
    /// Found compound operators — segments separated by `&&`, `||`, `;`, `|`.
    Compound(Vec<CommandSegment>),
    /// Unsupported shell syntax (heredocs, subshells, backticks, unmatched quotes).
    Bail,
}

/// A single command within a compound expression.
#[derive(Debug)]
struct CommandSegment {
    tokens: Vec<String>,
    trailing_operator: Option<CompoundOp>,
}

/// Shell compound operators.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CompoundOp {
    And,       // &&
    Or,        // ||
    Semicolon, // ;
    Pipe,      // |
}

impl CompoundOp {
    fn as_str(self) -> &'static str {
        match self {
            CompoundOp::And => "&&",
            CompoundOp::Or => "||",
            CompoundOp::Semicolon => ";",
            CompoundOp::Pipe => "|",
        }
    }
}

/// Quote-tracking state for the compound splitter.
#[derive(Debug, Clone, Copy, PartialEq)]
enum QuoteState {
    None,
    SingleQuote,
    DoubleQuote,
}

#[derive(Serialize)]
struct SuggestOutput<'a> {
    version: u8,
    #[serde(rename = "match")]
    is_match: bool,
    original: &'a str,
    rewritten: &'a str,
    #[serde(serialize_with = "serialize_category")]
    category: Option<RewriteCategory>,
    confidence: &'a str,
    compound: bool,
    skim_hook_version: &'a str,
}

fn serialize_category<S: serde::Serializer>(
    cat: &Option<RewriteCategory>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match cat {
        Some(c) => c.serialize(serializer),
        None => serializer.serialize_str(""),
    }
}

// ============================================================================
// Rule table (15 rules, ordered longest-prefix-first within same leading token)
// ============================================================================

const REWRITE_RULES: &[RewriteRule] = &[
    // cargo (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "nextest", "run"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["cargo", "test"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["cargo", "clippy"],
        rewrite_to: &["skim", "build", "clippy"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    RewriteRule {
        prefix: &["cargo", "build"],
        rewrite_to: &["skim", "build", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // python (longest prefix first)
    RewriteRule {
        prefix: &["python3", "-m", "pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["python", "-m", "pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    // npx
    RewriteRule {
        prefix: &["npx", "vitest"],
        rewrite_to: &["skim", "test", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["npx", "tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // bare commands
    RewriteRule {
        prefix: &["pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["vitest"],
        rewrite_to: &["skim", "test", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["go", "test"],
        rewrite_to: &["skim", "test", "go"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    // git
    RewriteRule {
        prefix: &["git", "status"],
        rewrite_to: &["skim", "git", "status"],
        skip_if_flag_prefix: &["--porcelain", "--short", "-s"],
        category: RewriteCategory::Git,
    },
    RewriteRule {
        prefix: &["git", "diff"],
        rewrite_to: &["skim", "git", "diff"],
        skip_if_flag_prefix: &["--stat", "--name-only", "--name-status", "--check"],
        category: RewriteCategory::Git,
    },
    RewriteRule {
        prefix: &["git", "log"],
        rewrite_to: &["skim", "git", "log"],
        skip_if_flag_prefix: &["--format", "--pretty", "--oneline"],
        category: RewriteCategory::Git,
    },
    // tsc bare
    RewriteRule {
        prefix: &["tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
];

// ============================================================================
// Entry point
// ============================================================================

/// Run the `rewrite` subcommand. Returns the process exit code.
///
/// Exit code semantics:
/// - 0: rewrite found, printed to stdout
/// - 1: no rewrite match (or compound command, or invalid input)
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Check for --suggest flag (must be first non-help flag)
    let suggest_mode = args.first().is_some_and(|a| a == "--suggest");

    // Collect command tokens: skip leading --suggest if present
    let positional_start = if suggest_mode { 1 } else { 0 };
    let positional_args: Vec<&str> = args[positional_start..]
        .iter()
        .map(|s| s.as_str())
        .collect();

    // Get command tokens from positional args or stdin
    let tokens: Vec<String> = if positional_args.is_empty() {
        // Try reading from stdin if it's piped
        if io::stdin().is_terminal() {
            return emit_result(suggest_mode, "", None, false);
        }
        // Read one line from stdin, capped at 4 KiB to prevent unbounded allocation.
        // Uses take() to bound memory before reading, so even input without a newline
        // cannot cause unbounded allocation.
        let mut line = String::new();
        io::BufReader::new(io::stdin().lock().take(4096)).read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return emit_result(suggest_mode, "", None, false);
        }
        trimmed.split_whitespace().map(String::from).collect()
    } else {
        positional_args.iter().map(|s| s.to_string()).collect()
    };

    if tokens.is_empty() {
        return emit_result(suggest_mode, "", None, false);
    }

    let original = tokens.join(" ");

    // Fast path: if no compound operator chars are present, skip split_compound
    // entirely and avoid the second tokenization pass.
    let has_operator_chars = original.contains("&&")
        || original.contains("||")
        || original.contains(';')
        || original.contains('|');
    if !has_operator_chars {
        let token_refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();
        let result = try_rewrite(&token_refs);
        let rewritten = result.as_ref().map(|r| r.tokens.join(" "));
        let match_info = result
            .as_ref()
            .zip(rewritten.as_ref())
            .map(|(r, s)| (s.as_str(), r.category));
        return emit_result(suggest_mode, &original, match_info, false);
    }

    // Split into compound segments (or simple if no operators found)
    match split_compound(&original) {
        CompoundSplitResult::Bail => emit_result(suggest_mode, &original, None, false),
        CompoundSplitResult::Simple(simple_tokens) => {
            let token_refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
            let result = try_rewrite(&token_refs);
            let rewritten = result.as_ref().map(|r| r.tokens.join(" "));
            let match_info = result
                .as_ref()
                .zip(rewritten.as_ref())
                .map(|(r, s)| (s.as_str(), r.category));
            emit_result(suggest_mode, &original, match_info, false)
        }
        CompoundSplitResult::Compound(segments) => {
            let result = try_rewrite_compound(&segments);
            let rewritten = result.as_ref().map(|r| r.tokens.join(" "));
            let match_info = result
                .as_ref()
                .zip(rewritten.as_ref())
                .map(|(r, s)| (s.as_str(), r.category));
            emit_result(suggest_mode, &original, match_info, true)
        }
    }
}

/// Emit the final result of a rewrite attempt.
///
/// In suggest mode, always prints JSON and returns SUCCESS.
/// In normal mode, prints the rewritten command on match (SUCCESS) or
/// returns FAILURE silently on no match.
fn emit_result(
    suggest_mode: bool,
    original: &str,
    result: Option<(&str, RewriteCategory)>,
    compound: bool,
) -> anyhow::Result<ExitCode> {
    if suggest_mode {
        print_suggest(original, result, compound);
        return Ok(ExitCode::SUCCESS);
    }
    match result {
        Some((rewritten, _)) => {
            println!("{rewritten}");
            Ok(ExitCode::SUCCESS)
        }
        None => Ok(ExitCode::FAILURE),
    }
}

// ============================================================================
// Core rewrite algorithm
// ============================================================================

/// Attempt to rewrite a tokenized command. Returns `Some(RewriteResult)` on
/// match, `None` if no rewrite applies.
fn try_rewrite(tokens: &[&str]) -> Option<RewriteResult> {
    if tokens.is_empty() {
        return None;
    }

    // Step 1: Strip leading env vars (KEY=VALUE pairs before the command)
    let env_split = strip_env_vars(tokens);
    let env_vars = &tokens[..env_split];
    let command_tokens = &tokens[env_split..];

    if command_tokens.is_empty() {
        return None;
    }

    // Step 2: Strip cargo toolchain prefix (+nightly etc.)
    let (toolchain, match_tokens) = strip_cargo_toolchain(command_tokens);

    // Step 3: Split at `--` separator
    let sep_pos = split_at_separator(&match_tokens);
    let before_sep = &match_tokens[..sep_pos];
    let separator_and_after = &match_tokens[sep_pos..];

    // Step 4: Try declarative table match, then custom handlers (cat/head/tail)
    try_table_match(env_vars, before_sep, separator_and_after, toolchain)
        .or_else(|| try_custom_handlers(env_vars, command_tokens))
}

/// Return the index of the first non-env-var token.
///
/// Env vars match pattern: contains `=` and everything before `=` is
/// `[A-Z0-9_]+` (all uppercase letters, digits, underscores).
/// Callers can slice `tokens[..index]` for env vars and `tokens[index..]`
/// for the command, avoiding a Vec allocation.
fn strip_env_vars(tokens: &[&str]) -> usize {
    let mut count = 0;

    for token in tokens {
        if let Some(eq_pos) = token.find('=') {
            let key = &token[..eq_pos];
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                count += 1;
                continue;
            }
        }
        break;
    }

    count
}

/// Strip cargo toolchain prefix (e.g., `+nightly`).
///
/// If tokens[0] is "cargo" and tokens[1] starts with '+', strip tokens[1]
/// for matching but preserve it for output reconstruction.
fn strip_cargo_toolchain<'a>(tokens: &[&'a str]) -> (Option<&'a str>, Vec<&'a str>) {
    if tokens.len() >= 2 && tokens[0] == "cargo" && tokens[1].starts_with('+') {
        let toolchain = Some(tokens[1]);
        let mut match_tokens = vec![tokens[0]];
        match_tokens.extend_from_slice(&tokens[2..]);
        (toolchain, match_tokens)
    } else {
        (None, tokens.to_vec())
    }
}

/// Find the index of the first `--` separator.
///
/// Returns the position of `--` if found, or `tokens.len()` if absent.
/// Callers can slice `tokens[..index]` for before and `tokens[index..]`
/// for separator-and-after, avoiding a Vec allocation.
fn split_at_separator(tokens: &[&str]) -> usize {
    tokens
        .iter()
        .position(|t| *t == "--")
        .unwrap_or(tokens.len())
}

/// Try matching against the declarative rule table.
fn try_table_match(
    env_vars: &[&str],
    before_sep: &[&str],
    separator_and_after: &[&str],
    toolchain: Option<&str>,
) -> Option<RewriteResult> {
    for rule in REWRITE_RULES {
        // Check if prefix matches
        if before_sep.len() < rule.prefix.len() {
            continue;
        }
        if before_sep[..rule.prefix.len()] != *rule.prefix {
            continue;
        }

        // Middle args: everything between prefix and separator
        let middle = &before_sep[rule.prefix.len()..];

        // Check skip_if_flag_prefix: if any middle arg starts with a skip prefix
        if !rule.skip_if_flag_prefix.is_empty()
            && middle.iter().any(|arg| {
                rule.skip_if_flag_prefix
                    .iter()
                    .any(|skip| arg.starts_with(skip))
            })
        {
            return None;
        }

        // Build output: env_vars ++ rewrite_to ++ toolchain ++ middle ++ separator_and_after
        let output: Vec<String> = env_vars
            .iter()
            .chain(rule.rewrite_to.iter())
            .map(|s| s.to_string())
            .chain(toolchain.map(String::from))
            .chain(
                middle
                    .iter()
                    .chain(separator_and_after.iter())
                    .map(|s| s.to_string()),
            )
            .collect();

        return Some(RewriteResult {
            tokens: output,
            category: rule.category,
        });
    }

    None
}

/// Try custom handlers for cat, head, tail.
fn try_custom_handlers(env_vars: &[&str], command_tokens: &[&str]) -> Option<RewriteResult> {
    if command_tokens.is_empty() {
        return None;
    }

    let result = match command_tokens[0] {
        "cat" => try_rewrite_cat(&command_tokens[1..]),
        "head" => try_rewrite_head(&command_tokens[1..]),
        "tail" => try_rewrite_tail(&command_tokens[1..]),
        _ => None,
    };

    result.map(|mut r| {
        // Prepend env vars if present
        if !env_vars.is_empty() {
            let mut with_env: Vec<String> = env_vars.iter().map(|s| s.to_string()).collect();
            with_env.extend(r.tokens);
            r.tokens = with_env;
        }
        r
    })
}

// ============================================================================
// Compound command splitting (#45)
// ============================================================================

/// Split a shell command string at compound operators (`&&`, `||`, `;`, `|`).
///
/// Uses a character-by-character state machine tracking quotes and paren depth.
/// Only splits at operators when outside quotes and at paren depth 0.
///
/// Bail conditions (returns `Bail`): heredocs `<<`, subshells `$(`, backticks,
/// unmatched quotes at end of input.
fn split_compound(input: &str) -> CompoundSplitResult {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();

    let mut segments: Vec<CommandSegment> = Vec::new();
    let mut current_start: usize = 0; // byte offset into input for current segment
    let mut quote_state = QuoteState::None;
    let mut paren_depth: usize = 0;
    let mut found_operator = false;
    let mut i: usize = 0;
    // Precompute byte offsets for each char index
    let byte_offsets: Vec<usize> = {
        let mut offsets = Vec::with_capacity(len + 1);
        let mut bo = 0;
        for ch in &chars {
            offsets.push(bo);
            bo += ch.len_utf8();
        }
        offsets.push(bo); // sentinel for end-of-string
        offsets
    };

    while i < len {
        let ch = chars[i];

        // Handle quote state transitions
        match quote_state {
            QuoteState::SingleQuote => {
                if ch == '\'' {
                    quote_state = QuoteState::None;
                }
                i += 1;
                continue;
            }
            QuoteState::DoubleQuote => {
                if ch == '\\' && i + 1 < len {
                    i += 2; // skip escaped char (e.g., \")
                    continue;
                }
                if ch == '"' {
                    quote_state = QuoteState::None;
                }
                i += 1;
                continue;
            }
            QuoteState::None => {}
        }

        // Bail on backticks
        if ch == '`' {
            return CompoundSplitResult::Bail;
        }

        // Enter quotes
        if ch == '\'' {
            quote_state = QuoteState::SingleQuote;
            i += 1;
            continue;
        }
        if ch == '"' {
            quote_state = QuoteState::DoubleQuote;
            i += 1;
            continue;
        }

        // Track parens
        if ch == '(' {
            paren_depth += 1;
            i += 1;
            continue;
        }
        if ch == ')' {
            paren_depth = paren_depth.saturating_sub(1);
            i += 1;
            continue;
        }

        // Bail on heredoc: << (but not <<< which is a here-string — still bail)
        if ch == '<' && i + 1 < len && chars[i + 1] == '<' {
            return CompoundSplitResult::Bail;
        }

        // Bail on subshell $( and variable expansion ${
        if ch == '$' && i + 1 < len && (chars[i + 1] == '(' || chars[i + 1] == '{') {
            return CompoundSplitResult::Bail;
        }

        // Only check operators at paren_depth == 0
        if paren_depth == 0 {
            // Check for &&
            if ch == '&' && i + 1 < len && chars[i + 1] == '&' {
                // Guard against >&N redirect patterns (e.g., 2>&1).
                // When '>' immediately precedes '&', this is a file descriptor
                // redirect, not the start of '&&'.
                if i > 0 && chars[i - 1] == '>' {
                    i += 1;
                    continue;
                }
                let seg_text = &input[current_start..byte_offsets[i]];
                let tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
                if !tokens.is_empty() {
                    segments.push(CommandSegment {
                        tokens,
                        trailing_operator: Some(CompoundOp::And),
                    });
                }
                found_operator = true;
                i += 2; // skip both &
                current_start = byte_offsets[i.min(len)];
                continue;
            }

            // Check for ||
            if ch == '|' && i + 1 < len && chars[i + 1] == '|' {
                let seg_text = &input[current_start..byte_offsets[i]];
                let tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
                if !tokens.is_empty() {
                    segments.push(CommandSegment {
                        tokens,
                        trailing_operator: Some(CompoundOp::Or),
                    });
                }
                found_operator = true;
                i += 2;
                current_start = byte_offsets[i.min(len)];
                continue;
            }

            // Check for single | (pipe, not ||)
            if ch == '|' {
                let seg_text = &input[current_start..byte_offsets[i]];
                let tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
                if !tokens.is_empty() {
                    segments.push(CommandSegment {
                        tokens,
                        trailing_operator: Some(CompoundOp::Pipe),
                    });
                }
                found_operator = true;
                i += 1;
                current_start = byte_offsets[i.min(len)];
                continue;
            }

            // Check for ;
            if ch == ';' {
                let seg_text = &input[current_start..byte_offsets[i]];
                let tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
                if !tokens.is_empty() {
                    segments.push(CommandSegment {
                        tokens,
                        trailing_operator: Some(CompoundOp::Semicolon),
                    });
                }
                found_operator = true;
                i += 1;
                current_start = byte_offsets[i.min(len)];
                continue;
            }
        }

        i += 1;
    }

    // Bail on unmatched quotes
    if quote_state != QuoteState::None {
        return CompoundSplitResult::Bail;
    }

    if !found_operator {
        // No compound operators found — return as simple
        let tokens: Vec<String> = input.split_whitespace().map(String::from).collect();
        return CompoundSplitResult::Simple(tokens);
    }

    // Push the final segment (after the last operator)
    let seg_text = &input[current_start..];
    let tokens: Vec<String> = seg_text.split_whitespace().map(String::from).collect();
    if !tokens.is_empty() {
        segments.push(CommandSegment {
            tokens,
            trailing_operator: None,
        });
    }

    CompoundSplitResult::Compound(segments)
}

/// Commands that should NOT have their pipe output rewritten.
/// These are typically output-producing tools where the pipe consumer (head, grep, etc.)
/// is what the user actually wants to control.
const PIPE_EXCLUDED_SOURCES: &[&str] = &["find", "fd", "ls", "rg", "grep", "ag"];

/// Attempt to rewrite a compound command expression.
///
/// For `&&`/`||`/`;`: tries `try_rewrite()` on each segment independently.
/// For `|`: only rewrites the first segment (the output producer).
/// Returns `Some(RewriteResult)` if ANY segment was rewritten, `None` otherwise.
fn try_rewrite_compound(segments: &[CommandSegment]) -> Option<RewriteResult> {
    if segments.is_empty() {
        return None;
    }

    // Check if this is a pipe expression (any segment has a Pipe operator)
    let has_pipe = segments
        .iter()
        .any(|s| s.trailing_operator == Some(CompoundOp::Pipe));

    if has_pipe {
        return try_rewrite_compound_pipe(segments);
    }

    // For &&/||/; — try rewriting each segment independently
    let mut any_rewritten = false;
    let mut first_category: Option<RewriteCategory> = None;
    let mut parts: Vec<String> = Vec::new();

    for seg in segments {
        let token_refs: Vec<&str> = seg.tokens.iter().map(|s| s.as_str()).collect();
        let rewrite = try_rewrite(&token_refs);

        let segment_text = match &rewrite {
            Some(r) => {
                any_rewritten = true;
                if first_category.is_none() {
                    first_category = Some(r.category);
                }
                r.tokens.join(" ")
            }
            None => seg.tokens.join(" "),
        };

        parts.push(segment_text);

        // Add the operator between segments (not after the last one)
        if let Some(op) = seg.trailing_operator {
            parts.push(op.as_str().to_string());
        }
    }

    if !any_rewritten {
        return None;
    }

    Some(RewriteResult {
        tokens: parts,
        category: first_category.unwrap_or(RewriteCategory::Build),
    })
}

/// Rewrite a pipe expression. Only the first segment (output producer) is rewritten.
fn try_rewrite_compound_pipe(segments: &[CommandSegment]) -> Option<RewriteResult> {
    if segments.is_empty() {
        return None;
    }

    let first = &segments[0];

    // Skip env vars to find the actual command name, reusing the canonical
    // strip_env_vars logic (all-uppercase key before '=').
    let token_refs: Vec<&str> = first.tokens.iter().map(|s| s.as_str()).collect();
    let env_split = strip_env_vars(&token_refs);
    let first_cmd = first.tokens.get(env_split);
    if let Some(cmd) = first_cmd {
        if PIPE_EXCLUDED_SOURCES.contains(&cmd.as_str()) {
            return None;
        }
    }

    let token_refs: Vec<&str> = first.tokens.iter().map(|s| s.as_str()).collect();
    let rewrite = try_rewrite(&token_refs)?;

    // Reconstruct: rewritten first segment | rest unchanged
    let mut parts: Vec<String> = Vec::new();
    parts.push(rewrite.tokens.join(" "));

    for (idx, seg) in segments.iter().enumerate() {
        if idx == 0 {
            // Already handled the first segment; add its operator
            if let Some(op) = seg.trailing_operator {
                parts.push(op.as_str().to_string());
            }
            continue;
        }
        parts.push(seg.tokens.join(" "));
        if let Some(op) = seg.trailing_operator {
            parts.push(op.as_str().to_string());
        }
    }

    Some(RewriteResult {
        tokens: parts,
        category: rewrite.category,
    })
}

// ============================================================================
// Custom handlers (cat, head, tail)
// ============================================================================

/// Check if a file path has a known code extension.
///
/// Extracts the extension from the path and checks against `Language::from_extension`.
/// Does NOT check if the file exists on disk — this is pure string analysis.
fn is_code_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(rskim_core::Language::from_extension)
        .is_some()
}

/// Rewrite `cat` command.
///
/// Rules:
/// - `cat file.ts` → `skim file.ts --mode=pseudo`
/// - `cat -s file.ts` → `skim file.ts --mode=pseudo` (-s squeeze blanks: pseudo is better)
/// - `cat -n file.ts` → None (line numbers)
/// - `cat -b/-v/-e/-t/-A` → None (display flags)
/// - `cat file1.ts file2.py` → `skim file1.ts file2.py --mode=pseudo --no-header`
/// - `cat` (no file arg) → None
/// - `cat non-code.txt` → None
fn try_rewrite_cat(args: &[&str]) -> Option<RewriteResult> {
    if args.is_empty() {
        return None;
    }

    let mut files: Vec<&str> = Vec::new();
    let mut has_unsupported_flag = false;

    for arg in args {
        if arg.starts_with('-') && *arg != "-" {
            // Allow -s (squeeze blank lines), reject everything else
            if *arg == "-s" {
                continue;
            }
            has_unsupported_flag = true;
            break;
        }
        files.push(arg);
    }

    if has_unsupported_flag || files.is_empty() {
        return None;
    }

    // All files must be code files
    if !files.iter().all(|f| is_code_file(f)) {
        return None;
    }

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push("--mode=pseudo".to_string());
    if files.len() > 1 {
        tokens.push("--no-header".to_string());
    }

    Some(RewriteResult {
        tokens,
        category: RewriteCategory::Read,
    })
}

/// Parse a line count from head/tail -N or -n N or -nN style arguments.
///
/// Returns `Some((count, files))` on success, `None` if no files found or
/// an unrecognized flag is encountered.
fn parse_line_count_and_files<'a>(args: &[&'a str]) -> Option<(Option<u64>, Vec<&'a str>)> {
    if args.is_empty() {
        return None;
    }

    let mut count: Option<u64> = None;
    let mut files: Vec<&'a str> = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = args[i];

        if arg == "-n" {
            // -n N form: next arg is the count
            i += 1;
            if i >= args.len() {
                return None;
            }
            count = Some(args[i].parse::<u64>().ok()?);
        } else if let Some(rest) = arg.strip_prefix("-n") {
            // -nN form: rest is the count
            count = Some(rest.parse::<u64>().ok()?);
        } else if arg.starts_with('-') && arg != "-" {
            // Check for -N (bare number) like -20
            let potential_num = &arg[1..];
            if let Ok(n) = potential_num.parse::<u64>() {
                count = Some(n);
            } else {
                // Unknown flag
                return None;
            }
        } else {
            files.push(arg);
        }

        i += 1;
    }

    if files.is_empty() {
        return None;
    }

    Some((count, files))
}

/// Shared rewrite logic for head/tail commands.
///
/// Parses line count and file arguments, validates all files are code files,
/// and builds the skim command with the appropriate line-limit flag.
fn try_rewrite_head_tail(args: &[&str], line_flag: &str) -> Option<RewriteResult> {
    let (count, files) = parse_line_count_and_files(args)?;

    if !files.iter().all(|f| is_code_file(f)) {
        return None;
    }

    let mut tokens: Vec<String> = vec!["skim".to_string()];
    tokens.extend(files.iter().map(|f| f.to_string()));
    tokens.push("--mode=pseudo".to_string());
    if let Some(n) = count {
        tokens.push(line_flag.to_string());
        tokens.push(n.to_string());
    }

    Some(RewriteResult {
        tokens,
        category: RewriteCategory::Read,
    })
}

/// Rewrite `head` command.
///
/// Rules:
/// - `head -20 file.ts` → `skim file.ts --mode=pseudo --max-lines 20`
/// - `head -n 20 file.ts` → `skim file.ts --mode=pseudo --max-lines 20`
/// - `head -n20 file.ts` → `skim file.ts --mode=pseudo --max-lines 20`
/// - `head file.ts` → `skim file.ts --mode=pseudo`
/// - `head -20 data.csv` → None (not code file)
fn try_rewrite_head(args: &[&str]) -> Option<RewriteResult> {
    try_rewrite_head_tail(args, "--max-lines")
}

/// Rewrite `tail` command.
///
/// Rules:
/// - `tail -20 file.rs` → `skim file.rs --mode=pseudo --last-lines 20`
/// - `tail -n 20 file.rs` → `skim file.rs --mode=pseudo --last-lines 20`
/// - `tail file.rs` → `skim file.rs --mode=pseudo`
/// - `tail -20 data.csv` → None (not code file)
fn try_rewrite_tail(args: &[&str]) -> Option<RewriteResult> {
    try_rewrite_head_tail(args, "--last-lines")
}

// ============================================================================
// Suggest mode output
// ============================================================================

fn print_suggest(original: &str, result: Option<(&str, RewriteCategory)>, compound: bool) {
    let output = SuggestOutput {
        version: 1,
        is_match: result.is_some(),
        original,
        rewritten: result.map_or("", |(r, _)| r),
        category: result.map(|(_, c)| c),
        confidence: if result.is_some() { "exact" } else { "" },
        compound,
        skim_hook_version: "1.0.0",
    };
    // Struct contains only primitive types (&str, u8, bool) — serialization cannot fail.
    let json = serde_json::to_string(&output)
        .expect("BUG: SuggestOutput serialization failed — struct contains only primitive types");
    println!("{json}");
}

// ============================================================================
// Clap Command definition (shared with completions.rs)
// ============================================================================

/// Build the clap `Command` definition for the rewrite subcommand.
///
/// Used by `completions.rs` to generate accurate shell completions without
/// duplicating the argument definitions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("rewrite")
        .about("Rewrite common developer commands into skim equivalents")
        .arg(
            clap::Arg::new("suggest")
                .long("suggest")
                .action(clap::ArgAction::SetTrue)
                .help("Output JSON suggestion instead of plain text"),
        )
        .arg(
            clap::Arg::new("command")
                .value_name("COMMAND")
                .num_args(1..)
                .help("Command to rewrite"),
        )
}

// ============================================================================
// Help text
// ============================================================================

fn print_help() {
    println!("skim rewrite");
    println!();
    println!("  Rewrite common developer commands into skim equivalents");
    println!();
    println!("Usage: skim rewrite [--suggest] <COMMAND>...");
    println!("       echo \"cargo test\" | skim rewrite [--suggest]");
    println!();
    println!("Options:");
    println!("  --suggest    Output JSON suggestion instead of plain text");
    println!("  --help, -h   Print help information");
    println!();
    println!("Examples:");
    println!("  skim rewrite cargo test -- --nocapture");
    println!("  skim rewrite git status");
    println!("  skim rewrite cat src/main.rs");
    println!("  echo \"pytest -v\" | skim rewrite --suggest");
    println!();
    println!("Exit codes:");
    println!("  0  Rewrite found (or --suggest mode)");
    println!("  1  No rewrite match");
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Prefix rule matches (all 15 rules)
    // ========================================================================

    #[test]
    fn test_cargo_test() {
        let result = try_rewrite(&["cargo", "test"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo"]);
    }

    #[test]
    fn test_cargo_test_with_trailing_args() {
        let result = try_rewrite(&["cargo", "test", "--", "--nocapture"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", "--", "--nocapture"]
        );
    }

    #[test]
    fn test_cargo_nextest_run() {
        let result = try_rewrite(&["cargo", "nextest", "run"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo"]);
    }

    #[test]
    fn test_cargo_clippy() {
        let result = try_rewrite(&["cargo", "clippy"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "clippy"]);
    }

    #[test]
    fn test_cargo_build() {
        let result = try_rewrite(&["cargo", "build"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "cargo"]);
    }

    #[test]
    fn test_python3_m_pytest() {
        let result = try_rewrite(&["python3", "-m", "pytest"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "pytest"]);
    }

    #[test]
    fn test_python_m_pytest() {
        let result = try_rewrite(&["python", "-m", "pytest"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "pytest"]);
    }

    #[test]
    fn test_npx_vitest() {
        let result = try_rewrite(&["npx", "vitest"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "vitest"]);
    }

    #[test]
    fn test_npx_tsc() {
        let result = try_rewrite(&["npx", "tsc"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "tsc"]);
    }

    #[test]
    fn test_bare_pytest() {
        let result = try_rewrite(&["pytest"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "pytest"]);
    }

    #[test]
    fn test_bare_pytest_with_flag() {
        let result = try_rewrite(&["pytest", "-v"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "pytest", "-v"]);
    }

    #[test]
    fn test_bare_vitest() {
        let result = try_rewrite(&["vitest"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "vitest"]);
    }

    #[test]
    fn test_go_test() {
        let result = try_rewrite(&["go", "test"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "go"]);
    }

    #[test]
    fn test_go_test_with_path() {
        let result = try_rewrite(&["go", "test", "./..."]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "go", "./..."]);
    }

    #[test]
    fn test_git_status() {
        let result = try_rewrite(&["git", "status"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "git", "status"]);
    }

    #[test]
    fn test_git_diff() {
        let result = try_rewrite(&["git", "diff"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "git", "diff"]);
    }

    #[test]
    fn test_git_log() {
        let result = try_rewrite(&["git", "log"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "git", "log"]);
    }

    #[test]
    fn test_bare_tsc() {
        let result = try_rewrite(&["tsc"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "tsc"]);
    }

    // ========================================================================
    // Skip-flag behavior (git rules)
    // ========================================================================

    #[test]
    fn test_git_status_with_porcelain_skipped() {
        assert!(try_rewrite(&["git", "status", "--porcelain"]).is_none());
    }

    #[test]
    fn test_git_status_with_short_skipped() {
        assert!(try_rewrite(&["git", "status", "--short"]).is_none());
    }

    #[test]
    fn test_git_status_with_s_skipped() {
        assert!(try_rewrite(&["git", "status", "-s"]).is_none());
    }

    #[test]
    fn test_git_diff_with_stat_skipped() {
        assert!(try_rewrite(&["git", "diff", "--stat"]).is_none());
    }

    #[test]
    fn test_git_diff_with_name_only_skipped() {
        assert!(try_rewrite(&["git", "diff", "--name-only"]).is_none());
    }

    #[test]
    fn test_git_diff_with_name_status_skipped() {
        assert!(try_rewrite(&["git", "diff", "--name-status"]).is_none());
    }

    #[test]
    fn test_git_diff_with_check_skipped() {
        assert!(try_rewrite(&["git", "diff", "--check"]).is_none());
    }

    #[test]
    fn test_git_log_with_format_skipped() {
        assert!(try_rewrite(&["git", "log", "--format=%H"]).is_none());
    }

    #[test]
    fn test_git_log_with_pretty_skipped() {
        assert!(try_rewrite(&["git", "log", "--pretty=oneline"]).is_none());
    }

    #[test]
    fn test_git_log_with_oneline_skipped() {
        assert!(try_rewrite(&["git", "log", "--oneline"]).is_none());
    }

    // ========================================================================
    // Env var stripping
    // ========================================================================

    #[test]
    fn test_env_var_stripping() {
        let result = try_rewrite(&["RUST_LOG=debug", "cargo", "test"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["RUST_LOG=debug", "skim", "test", "cargo"]
        );
    }

    #[test]
    fn test_multiple_env_vars() {
        let result = try_rewrite(&["RUST_LOG=debug", "RUST_BACKTRACE=1", "cargo", "test"]).unwrap();
        assert_eq!(
            result.tokens,
            vec![
                "RUST_LOG=debug",
                "RUST_BACKTRACE=1",
                "skim",
                "test",
                "cargo"
            ]
        );
    }

    #[test]
    fn test_env_var_only_is_no_match() {
        assert!(try_rewrite(&["RUST_LOG=debug"]).is_none());
    }

    // ========================================================================
    // Cargo toolchain stripping
    // ========================================================================

    #[test]
    fn test_cargo_toolchain_nightly() {
        let result = try_rewrite(&["cargo", "+nightly", "test"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo", "+nightly"]);
    }

    #[test]
    fn test_cargo_toolchain_stable() {
        let result = try_rewrite(&["cargo", "+stable", "build"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "build", "cargo", "+stable"]);
    }

    #[test]
    fn test_cargo_toolchain_with_env_var() {
        let result = try_rewrite(&["RUST_LOG=debug", "cargo", "+nightly", "test"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["RUST_LOG=debug", "skim", "test", "cargo", "+nightly"]
        );
    }

    // ========================================================================
    // -- separator preservation
    // ========================================================================

    #[test]
    fn test_separator_preserved() {
        let result = try_rewrite(&["cargo", "test", "--", "--nocapture"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", "--", "--nocapture"]
        );
    }

    #[test]
    fn test_separator_with_middle_args() {
        let result = try_rewrite(&["cargo", "test", "my_test", "--", "--nocapture"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", "my_test", "--", "--nocapture"]
        );
    }

    // ========================================================================
    // Compound operators passed through try_rewrite (#45)
    //
    // try_rewrite() no longer rejects compound operators — that logic
    // moved to split_compound(). When compound tokens leak into try_rewrite()
    // they are treated as regular arguments (this is by design).
    // ========================================================================

    #[test]
    fn test_pipe_as_token_passed_through() {
        // try_rewrite sees "|" and "head" as extra args after "cargo test"
        let result = try_rewrite(&["cargo", "test", "|", "head"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "test", "cargo", "|", "head"]);
    }

    #[test]
    fn test_and_and_as_token_passed_through() {
        let result = try_rewrite(&["cargo", "test", "&&", "cargo", "build"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", "&&", "cargo", "build"]
        );
    }

    #[test]
    fn test_or_or_as_token_passed_through() {
        let result = try_rewrite(&["cargo", "test", "||", "echo", "fail"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", "||", "echo", "fail"]
        );
    }

    #[test]
    fn test_semicolon_as_token_passed_through() {
        let result = try_rewrite(&["cargo", "test", ";", "echo", "done"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "test", "cargo", ";", "echo", "done"]
        );
    }

    // ========================================================================
    // cat handler
    // ========================================================================

    #[test]
    fn test_cat_single_code_file() {
        let result = try_rewrite(&["cat", "file.ts"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "file.ts", "--mode=pseudo"]);
    }

    #[test]
    fn test_cat_squeeze_blanks() {
        let result = try_rewrite(&["cat", "-s", "file.ts"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "file.ts", "--mode=pseudo"]);
    }

    #[test]
    fn test_cat_multi_code_files() {
        let result = try_rewrite(&["cat", "file1.ts", "file2.py"]).unwrap();
        assert_eq!(
            result.tokens,
            vec![
                "skim",
                "file1.ts",
                "file2.py",
                "--mode=pseudo",
                "--no-header"
            ]
        );
    }

    #[test]
    fn test_cat_line_numbers_rejected() {
        assert!(try_rewrite(&["cat", "-n", "file.ts"]).is_none());
    }

    #[test]
    fn test_cat_bare_rejected() {
        assert!(try_rewrite(&["cat"]).is_none());
    }

    #[test]
    fn test_cat_non_code_rejected() {
        assert!(try_rewrite(&["cat", "data.csv"]).is_none());
    }

    #[test]
    fn test_cat_non_code_txt_rejected() {
        assert!(try_rewrite(&["cat", "readme.txt"]).is_none());
    }

    #[test]
    fn test_cat_mixed_code_and_non_code_rejected() {
        assert!(try_rewrite(&["cat", "file.ts", "data.csv"]).is_none());
    }

    #[test]
    fn test_cat_b_flag_rejected() {
        assert!(try_rewrite(&["cat", "-b", "file.ts"]).is_none());
    }

    #[test]
    fn test_cat_v_flag_rejected() {
        assert!(try_rewrite(&["cat", "-v", "file.ts"]).is_none());
    }

    #[test]
    fn test_cat_e_flag_rejected() {
        assert!(try_rewrite(&["cat", "-e", "file.ts"]).is_none());
    }

    #[test]
    fn test_cat_t_flag_rejected() {
        assert!(try_rewrite(&["cat", "-t", "file.ts"]).is_none());
    }

    #[test]
    fn test_cat_upper_a_flag_rejected() {
        assert!(try_rewrite(&["cat", "-A", "file.ts"]).is_none());
    }

    // ========================================================================
    // head handler
    // ========================================================================

    #[test]
    fn test_head_dash_n() {
        let result = try_rewrite(&["head", "-20", "file.ts"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "file.ts", "--mode=pseudo", "--max-lines", "20"]
        );
    }

    #[test]
    fn test_head_n_space() {
        let result = try_rewrite(&["head", "-n", "20", "file.ts"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "file.ts", "--mode=pseudo", "--max-lines", "20"]
        );
    }

    #[test]
    fn test_head_n_no_space() {
        let result = try_rewrite(&["head", "-n20", "file.ts"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "file.ts", "--mode=pseudo", "--max-lines", "20"]
        );
    }

    #[test]
    fn test_head_no_count() {
        let result = try_rewrite(&["head", "file.ts"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "file.ts", "--mode=pseudo"]);
    }

    #[test]
    fn test_head_non_code_rejected() {
        assert!(try_rewrite(&["head", "-20", "data.csv"]).is_none());
    }

    // ========================================================================
    // tail handler
    // ========================================================================

    #[test]
    fn test_tail_dash_n() {
        let result = try_rewrite(&["tail", "-20", "file.rs"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "file.rs", "--mode=pseudo", "--last-lines", "20"]
        );
    }

    #[test]
    fn test_tail_n_space() {
        let result = try_rewrite(&["tail", "-n", "20", "file.rs"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["skim", "file.rs", "--mode=pseudo", "--last-lines", "20"]
        );
    }

    #[test]
    fn test_tail_no_count() {
        let result = try_rewrite(&["tail", "file.rs"]).unwrap();
        assert_eq!(result.tokens, vec!["skim", "file.rs", "--mode=pseudo"]);
    }

    #[test]
    fn test_tail_non_code_rejected() {
        assert!(try_rewrite(&["tail", "-20", "data.csv"]).is_none());
    }

    // ========================================================================
    // Empty input and no-match cases
    // ========================================================================

    #[test]
    fn test_empty_input() {
        assert!(try_rewrite(&[]).is_none());
    }

    #[test]
    fn test_unknown_command() {
        assert!(try_rewrite(&["ls", "-la"]).is_none());
    }

    #[test]
    fn test_cd_not_rewritten() {
        assert!(try_rewrite(&["cd", "src"]).is_none());
    }

    // ========================================================================
    // Suggest mode output format
    // ========================================================================

    #[test]
    fn test_suggest_match_json_format() {
        let output = SuggestOutput {
            version: 1,
            is_match: true,
            original: "cargo test",
            rewritten: "skim test cargo",
            category: Some(RewriteCategory::Test),
            confidence: "exact",
            compound: false,
            skim_hook_version: "1.0.0",
        };
        let json = serde_json::to_string(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["match"], true);
        assert_eq!(parsed["original"], "cargo test");
        assert_eq!(parsed["rewritten"], "skim test cargo");
        assert_eq!(parsed["category"], "test");
        assert_eq!(parsed["confidence"], "exact");
        assert_eq!(parsed["compound"], false);
    }

    #[test]
    fn test_suggest_no_match_json_format() {
        let output = SuggestOutput {
            version: 1,
            is_match: false,
            original: "ls -la",
            rewritten: "",
            category: None,
            confidence: "",
            compound: false,
            skim_hook_version: "1.0.0",
        };
        let json = serde_json::to_string(&output).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["match"], false);
        assert_eq!(parsed["rewritten"], "");
        assert_eq!(parsed["category"], "");
        assert_eq!(parsed["compound"], false);
    }

    // ========================================================================
    // Category assignment
    // ========================================================================

    #[test]
    fn test_test_category_for_cargo_test() {
        let result = try_rewrite(&["cargo", "test"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Test));
    }

    #[test]
    fn test_build_category_for_cargo_build() {
        let result = try_rewrite(&["cargo", "build"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Build));
    }

    #[test]
    fn test_git_category_for_git_status() {
        let result = try_rewrite(&["git", "status"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Git));
    }

    #[test]
    fn test_read_category_for_cat() {
        let result = try_rewrite(&["cat", "file.ts"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Read));
    }

    #[test]
    fn test_read_category_for_head() {
        let result = try_rewrite(&["head", "-20", "file.ts"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Read));
    }

    #[test]
    fn test_read_category_for_tail() {
        let result = try_rewrite(&["tail", "file.rs"]).unwrap();
        assert!(matches!(result.category, RewriteCategory::Read));
    }

    // ========================================================================
    // is_code_file checks various extensions
    // ========================================================================

    #[test]
    fn test_is_code_file_ts() {
        assert!(is_code_file("file.ts"));
    }

    #[test]
    fn test_is_code_file_py() {
        assert!(is_code_file("file.py"));
    }

    #[test]
    fn test_is_code_file_rs() {
        assert!(is_code_file("src/main.rs"));
    }

    #[test]
    fn test_is_code_file_go() {
        assert!(is_code_file("main.go"));
    }

    #[test]
    fn test_is_code_file_java() {
        assert!(is_code_file("Main.java"));
    }

    #[test]
    fn test_is_code_file_json() {
        assert!(is_code_file("config.json"));
    }

    #[test]
    fn test_is_not_code_file_csv() {
        assert!(!is_code_file("data.csv"));
    }

    #[test]
    fn test_is_not_code_file_txt() {
        assert!(!is_code_file("readme.txt"));
    }

    #[test]
    fn test_is_not_code_file_no_extension() {
        assert!(!is_code_file("Makefile"));
    }

    // ========================================================================
    // Git skip-flag prefix matching (starts_with behavior)
    // ========================================================================

    #[test]
    fn test_git_log_format_with_value_skipped() {
        // --format=%H starts with --format
        assert!(try_rewrite(&["git", "log", "--format=%H"]).is_none());
    }

    #[test]
    fn test_git_log_pretty_with_value_skipped() {
        // --pretty=oneline starts with --pretty
        assert!(try_rewrite(&["git", "log", "--pretty=oneline"]).is_none());
    }

    // ========================================================================
    // Env var edge cases
    // ========================================================================

    #[test]
    fn test_lowercase_key_not_env_var() {
        // lowercase=value is not an env var (must be uppercase)
        assert!(try_rewrite(&["foo=bar", "cargo", "test"]).is_none());
    }

    #[test]
    fn test_env_var_with_numbers() {
        let result = try_rewrite(&["VAR_123=abc", "cargo", "test"]).unwrap();
        assert_eq!(result.tokens[0], "VAR_123=abc");
    }

    // ========================================================================
    // Env var preservation for cat/head/tail handlers
    // ========================================================================

    #[test]
    fn test_env_var_with_cat() {
        let result = try_rewrite(&["PAGER=less", "cat", "file.ts"]).unwrap();
        assert_eq!(
            result.tokens,
            vec!["PAGER=less", "skim", "file.ts", "--mode=pseudo"]
        );
    }

    #[test]
    fn test_env_var_with_head() {
        let result = try_rewrite(&["RUST_LOG=debug", "head", "-20", "file.ts"]).unwrap();
        assert_eq!(
            result.tokens,
            vec![
                "RUST_LOG=debug",
                "skim",
                "file.ts",
                "--mode=pseudo",
                "--max-lines",
                "20"
            ]
        );
    }

    #[test]
    fn test_env_var_with_tail() {
        let result = try_rewrite(&["VAR=value", "tail", "-10", "file.rs"]).unwrap();
        assert_eq!(
            result.tokens,
            vec![
                "VAR=value",
                "skim",
                "file.rs",
                "--mode=pseudo",
                "--last-lines",
                "10"
            ]
        );
    }

    // ========================================================================
    // parse_line_count_and_files edge cases
    // ========================================================================

    #[test]
    fn test_head_n_without_value() {
        // -n expects a number, but "file.ts" is not a number
        assert!(parse_line_count_and_files(&["-n", "file.ts"]).is_none());
    }

    #[test]
    fn test_head_n_non_numeric() {
        assert!(parse_line_count_and_files(&["-n", "abc", "file.ts"]).is_none());
    }

    #[test]
    fn test_head_unknown_flag_c() {
        assert!(parse_line_count_and_files(&["-c", "100", "file.ts"]).is_none());
    }

    #[test]
    fn test_tail_unknown_flag_f() {
        assert!(parse_line_count_and_files(&["-f", "file.rs"]).is_none());
    }

    #[test]
    fn test_head_long_flag_bytes() {
        assert!(parse_line_count_and_files(&["--bytes", "100", "file.ts"]).is_none());
    }

    // ========================================================================
    // split_compound state machine (#45)
    // ========================================================================

    #[test]
    fn test_split_compound_simple() {
        match split_compound("cargo test") {
            CompoundSplitResult::Simple(tokens) => {
                assert_eq!(tokens, vec!["cargo", "test"]);
            }
            other => panic!("Expected Simple, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_and_and() {
        match split_compound("cargo test && cargo build") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].tokens, vec!["cargo", "build"]);
                assert_eq!(segments[1].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_or_or() {
        match split_compound("cargo test || echo fail") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::Or));
                assert_eq!(segments[1].tokens, vec!["echo", "fail"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_semicolon() {
        match split_compound("cargo test ; echo done") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::Semicolon));
                assert_eq!(segments[1].tokens, vec!["echo", "done"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_pipe() {
        match split_compound("cargo test | head") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::Pipe));
                assert_eq!(segments[1].tokens, vec!["head"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_mixed_operators() {
        match split_compound("cargo test && cargo build ; echo done") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 3);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].trailing_operator, Some(CompoundOp::Semicolon));
                assert_eq!(segments[2].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    // ---- Quotes prevent splitting ----

    #[test]
    fn test_split_compound_double_quoted_operators_not_split() {
        match split_compound(r#"echo "a && b" test"#) {
            CompoundSplitResult::Simple(tokens) => {
                // Operators inside quotes should NOT split
                assert!(tokens.contains(&r#""a"#.to_string()));
            }
            CompoundSplitResult::Compound(_) => panic!("Should not split inside double quotes"),
            CompoundSplitResult::Bail => panic!("Should not bail"),
        }
    }

    #[test]
    fn test_split_compound_single_quoted_operators_not_split() {
        match split_compound("echo 'a && b' test") {
            CompoundSplitResult::Simple(tokens) => {
                assert!(tokens.contains(&"'a".to_string()));
            }
            CompoundSplitResult::Compound(_) => panic!("Should not split inside single quotes"),
            CompoundSplitResult::Bail => panic!("Should not bail"),
        }
    }

    // ---- Bail conditions ----

    #[test]
    fn test_split_compound_heredoc_bails() {
        match split_compound("cat <<EOF && echo done") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for heredoc, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_subshell_bails() {
        match split_compound("$(command) && cargo test") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for subshell, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_backtick_bails() {
        match split_compound("`command` && cargo test") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for backtick, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_unmatched_quote_bails() {
        match split_compound("echo \"unclosed && cargo test") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for unmatched quote, got {:?}", other),
        }
    }

    // ---- Redirect not treated as separator ----

    #[test]
    fn test_split_compound_redirect_2_ampersand_1_not_separator() {
        // 2>&1 contains & but should NOT be treated as &&
        match split_compound("cargo test 2>&1") {
            CompoundSplitResult::Simple(tokens) => {
                assert_eq!(tokens, vec!["cargo", "test", "2>&1"]);
            }
            other => panic!("Expected Simple (redirect not separator), got {:?}", other),
        }
    }

    // ========================================================================
    // Compound rewrite logic (#45)
    // ========================================================================

    #[test]
    fn test_compound_both_rewritten() {
        // Both cargo test and cargo build should be rewritten
        let segments = vec![
            CommandSegment {
                tokens: vec!["cargo".into(), "test".into()],
                trailing_operator: Some(CompoundOp::And),
            },
            CommandSegment {
                tokens: vec!["cargo".into(), "build".into()],
                trailing_operator: None,
            },
        ];
        let result = try_rewrite_compound(&segments).unwrap();
        let joined = result.tokens.join(" ");
        assert!(joined.contains("skim test cargo"));
        assert!(joined.contains("&&"));
        assert!(joined.contains("skim build cargo"));
    }

    #[test]
    fn test_compound_one_rewritten() {
        // cargo test rewritten, echo done not rewritten
        let segments = vec![
            CommandSegment {
                tokens: vec!["cargo".into(), "test".into()],
                trailing_operator: Some(CompoundOp::And),
            },
            CommandSegment {
                tokens: vec!["echo".into(), "done".into()],
                trailing_operator: None,
            },
        ];
        let result = try_rewrite_compound(&segments).unwrap();
        let joined = result.tokens.join(" ");
        assert!(joined.contains("skim test cargo"));
        assert!(joined.contains("&&"));
        assert!(joined.contains("echo done"));
    }

    #[test]
    fn test_compound_none_rewritten() {
        // Neither ls nor echo is rewritable
        let segments = vec![
            CommandSegment {
                tokens: vec!["ls".into()],
                trailing_operator: Some(CompoundOp::And),
            },
            CommandSegment {
                tokens: vec!["echo".into(), "done".into()],
                trailing_operator: None,
            },
        ];
        assert!(try_rewrite_compound(&segments).is_none());
    }

    // ---- Pipe rewrite ----

    #[test]
    fn test_compound_pipe_first_rewritten() {
        let segments = vec![
            CommandSegment {
                tokens: vec!["cargo".into(), "test".into()],
                trailing_operator: Some(CompoundOp::Pipe),
            },
            CommandSegment {
                tokens: vec!["head".into()],
                trailing_operator: None,
            },
        ];
        let result = try_rewrite_compound(&segments).unwrap();
        let joined = result.tokens.join(" ");
        assert!(joined.contains("skim test cargo"));
        assert!(joined.contains("|"));
        assert!(joined.contains("head"));
    }

    #[test]
    fn test_compound_pipe_excluded_source() {
        // find is in PIPE_EXCLUDED_SOURCES, so no rewrite
        let segments = vec![
            CommandSegment {
                tokens: vec!["find".into(), ".".into()],
                trailing_operator: Some(CompoundOp::Pipe),
            },
            CommandSegment {
                tokens: vec!["head".into()],
                trailing_operator: None,
            },
        ];
        assert!(try_rewrite_compound(&segments).is_none());
    }

    // ---- Env vars with compound ----

    #[test]
    fn test_compound_env_vars_preserved() {
        let segments = vec![
            CommandSegment {
                tokens: vec!["RUST_LOG=debug".into(), "cargo".into(), "test".into()],
                trailing_operator: Some(CompoundOp::And),
            },
            CommandSegment {
                tokens: vec!["cargo".into(), "build".into()],
                trailing_operator: None,
            },
        ];
        let result = try_rewrite_compound(&segments).unwrap();
        let joined = result.tokens.join(" ");
        assert!(joined.contains("RUST_LOG=debug"));
        assert!(joined.contains("skim test cargo"));
        assert!(joined.contains("&&"));
        assert!(joined.contains("skim build cargo"));
    }

    // ========================================================================
    // Operators without spaces (#77)
    // ========================================================================

    #[test]
    fn test_split_compound_and_and_no_spaces() {
        match split_compound("cargo test&&cargo build") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].tokens, vec!["cargo", "build"]);
                assert_eq!(segments[1].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    // ========================================================================
    // Escaped quotes in double-quoted strings (#77)
    // ========================================================================

    #[test]
    fn test_split_compound_escaped_double_quotes_not_split() {
        // echo "say \"hello\"" && cargo test — the escaped quotes inside the
        // double-quoted string should NOT end the quote, so && outside is the
        // real operator.
        match split_compound(r#"echo "say \"hello\"" && cargo test"#) {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 2);
                // First segment includes the entire echo with escaped quotes
                assert!(segments[0].tokens.join(" ").contains("echo"));
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].tokens, vec!["cargo", "test"]);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    // ========================================================================
    // Mixed pipe + sequential operators (#77)
    // ========================================================================

    #[test]
    fn test_split_compound_mixed_pipe_and_sequential() {
        // cargo test && cargo build | head — has both && and |
        match split_compound("cargo test && cargo build | head") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 3);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
                assert_eq!(segments[1].tokens, vec!["cargo", "build"]);
                assert_eq!(segments[1].trailing_operator, Some(CompoundOp::Pipe));
                assert_eq!(segments[2].tokens, vec!["head"]);
                assert_eq!(segments[2].trailing_operator, None);
            }
            other => panic!("Expected Compound, got {:?}", other),
        }
    }

    // ========================================================================
    // Empty segments from leading/trailing operators (#77)
    // ========================================================================

    #[test]
    fn test_split_compound_trailing_and_and_no_empty_segment() {
        // Trailing && should not produce an empty final segment
        match split_compound("cargo test &&") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                assert_eq!(segments[0].trailing_operator, Some(CompoundOp::And));
            }
            other => panic!("Expected Compound with 1 segment, got {:?}", other),
        }
    }

    #[test]
    fn test_split_compound_leading_and_and_no_empty_segment() {
        // Leading && should not produce an empty first segment
        match split_compound("&& cargo test") {
            CompoundSplitResult::Compound(segments) => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].tokens, vec!["cargo", "test"]);
                // Last segment has no trailing operator
                assert_eq!(segments[0].trailing_operator, None);
            }
            other => panic!("Expected Compound with 1 segment, got {:?}", other),
        }
    }

    // ========================================================================
    // Variable expansion bail (#77)
    // ========================================================================

    #[test]
    fn test_split_compound_variable_expansion_bails() {
        match split_compound("${CARGO:-cargo} test && echo done") {
            CompoundSplitResult::Bail => {}
            other => panic!("Expected Bail for variable expansion, got {:?}", other),
        }
    }
}
