use std::process::ExitCode;

use crate::cmd::user_has_flag;
use crate::output::ParseResult;
use crate::output::canonical::{PkgOperation, PkgResult};
use crate::runner::CommandOutput;

use super::combine_output;

pub(super) fn run_ls(
    args: &[String],
    show_stats: bool,
    json_output: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    super::run_pkg_subcommand(
        super::PkgSubcommandConfig {
            program: "npm",
            subcommand: "ls",
            expected_exit_codes: &[1],
            forward_stderr: false,
            env_overrides: &[("NO_COLOR", "1")],
            install_hint: "Install Node.js from https://nodejs.org",
        },
        args,
        show_stats,
        rec,
        |cmd_args| {
            if json_output && !user_has_flag(cmd_args, &["--json"]) {
                cmd_args.push("--json".to_string());
            }
            if json_output && !user_has_flag(cmd_args, &["--depth"]) {
                cmd_args.push("--depth=0".to_string());
            }
        },
        parse_ls,
    )
}

fn parse_ls(output: &CommandOutput) -> ParseResult<PkgResult> {
    // Tier 1: JSON
    if let Some(result) = try_parse_ls_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    // Tier 2: Regex (count package lines)
    let combined = combine_output(output);
    if let Some(result) = try_parse_ls_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["npm ls: JSON parse failed, using regex".to_string()],
        );
    }

    // Tier 3: Passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_ls_json(stdout: &str) -> Option<PkgResult> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let deps = value.get("dependencies")?.as_object()?;

    let total = deps.len();
    let mut flagged: usize = 0;
    let mut details: Vec<String> = Vec::new();

    for (name, dep) in deps {
        let version = dep.get("version").and_then(|v| v.as_str()).unwrap_or("?");
        let details_before = details.len();

        if let Some(problems) = dep.get("problems").and_then(|v| v.as_array())
            && !problems.is_empty()
        {
            flagged += 1;
            for problem in problems {
                if let Some(msg) = problem.as_str() {
                    details.push(format!("{name}@{version}: {msg}"));
                }
            }
        }

        // F3: every listed dependency names itself exactly once. Before this,
        // `version` was read above and then referenced only inside the
        // `problems` branch, so a healthy dependency contributed to `total`
        // and nothing else — `npm ls ms` answered a question about
        // `ms@2.1.3` with `1 total 0 flagged`, dropping the identity the
        // reader ran the command for rather than compressing it. The problem
        // branch already carries `{name}@{version}` in each message line, so
        // the bare form is pushed only when nothing else named this
        // dependency — which also covers a non-empty `problems` array whose
        // entries are not strings.
        if details.len() == details_before {
            details.push(format!("{name}@{version}"));
        }
    }

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::List { total, flagged },
        true,
        details,
    ))
}

/// Tree-drawing characters `npm ls` draws its dependency tree with, in both
/// the Unicode form (`├── ms@2.1.3`, `└─┬ a@1.0.0`, `│ └── b@2.0.0`) and the
/// ASCII fallback npm emits under `--no-unicode` (`+-- ms@2.1.3`, `| +-- b@2.0.0`,
/// and a backtick-rooted `-- ` for a last child). Both forms were measured
/// against `npm` 10 on a real dependency tree rather than read off the docs.
///
/// The Unicode members are U+251C, U+2514, U+2502, U+2500 and U+252C; the
/// ASCII members are `+`, a backtick, `|` and `-`. Several of these are easy
/// to mistake for one another on screen, hence the explicit codepoint list.
///
/// Stripping this set off the front of a line cannot eat the identity it is
/// there to expose: an npm package name may only begin with a letter, a digit
/// or `@`, so no character listed here can be the first byte of a
/// `name@version` token.
const NPM_TREE_GLYPHS: &[char] = &['├', '└', '│', '─', '┬', '+', '`', '|', '-'];

/// Strip npm's tree-drawing prefix off one `npm ls` text line, leaving the
/// `name@version` token and any `UNMET DEPENDENCY` / `invalid` marker intact.
///
/// Only the *prefix* is stripped of glyphs — the tail is trimmed of
/// whitespace alone — so a trailing marker such as ` deduped` survives and no
/// glyph character is removed from anywhere it could be content.
///
/// Known narrowing, stated rather than left to be discovered: nesting *depth*
/// is flattened, so `│ └── b@2.0.0` and `└── b@2.0.0` both render as
/// `b@2.0.0`. The package's identity survives; its position in the tree does
/// not, and `PkgOperation::List` carries no tree to place it in. `npm ls`
/// lists only top-level dependencies unless `--all` or `--depth` is passed,
/// so the flattened case is the exception rather than the rule.
fn strip_tree_prefix(line: &str) -> &str {
    line.trim()
        .trim_start_matches(|c: char| c.is_whitespace() || NPM_TREE_GLYPHS.contains(&c))
        .trim_end()
}

fn try_parse_ls_regex(text: &str) -> Option<PkgResult> {
    // npm ls text output is a tree: lines starting with non-empty package refs
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    // First line is project name, rest are dependencies
    let total = lines.len().saturating_sub(1);
    if total == 0 {
        return None;
    }

    // Count lines with "invalid" or "UNMET" markers
    let flagged = lines
        .iter()
        .filter(|l| l.contains("invalid") || l.contains("UNMET"))
        .count();

    // F3: keep each dependency's `name@version`. This tier never looked at the
    // token it was counting, so the whole render was a count: raw
    // `npm ls ms` says `ms@2.1.3` and skim said `1 total 0 flagged`. Reporting
    // the identity costs one short line per entry and leaves the summary a
    // summary — npm's tree art is deliberately not reproduced, because the
    // defect is the loss of identifying information, not the absence of the
    // tree. `total` is left as the line count it has always been rather than
    // rederived from `details.len()`, so this commit changes what the reader
    // is told and not how much of it there is.
    let details: Vec<String> = lines
        .iter()
        .skip(1)
        .copied()
        .map(strip_tree_prefix)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    Some(PkgResult::new(
        "npm".to_string(),
        PkgOperation::List { total, flagged },
        true,
        details,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::{load_fixture, make_output, make_output_full};

    // ========================================================================
    // npm ls: JSON
    // ========================================================================

    #[test]
    fn test_ls_json_parse() {
        let input = load_fixture("pkg", "npm_ls.json");
        let result = try_parse_ls_json(&input);
        assert!(result.is_some());
        let result = result.unwrap();
        let display = format!("{result}");
        assert!(display.contains("npm list"));
        assert!(display.contains("4 total"));
        assert!(display.contains("1 flagged"));
        assert!(display.contains("debug@4.3.4"));
    }

    /// F3: on a clean tree every dependency's `name@version` must reach the
    /// reader. Before this, `version` was read out of the JSON and referenced
    /// only inside the `problems` branch, so the three healthy dependencies in
    /// this fixture contributed to `total` and nothing else.
    ///
    /// The assertions name the exact `name@version` token rather than a proxy
    /// such as "details is non-empty": a proxy that survives the defect is not
    /// a detector (PF-025).
    #[test]
    fn test_ls_json_keeps_versions_on_clean_tree() {
        let input = load_fixture("pkg", "npm_ls.json");
        let result = try_parse_ls_json(&input).expect("JSON tier must parse the fixture");
        let display = format!("{result}");

        assert!(
            display.contains("npm list 4 total 1 flagged"),
            "the summary line must survive unchanged:\n{display}"
        );
        for expected in ["express@4.18.2", "lodash@4.17.21", "typescript@5.3.3"] {
            assert!(
                display.contains(expected),
                "healthy dependency {expected} lost its name@version:\n{display}"
            );
        }
        assert!(
            display.contains("debug@4.3.4: invalid: debug@4.3.4 expected: >=4.3.5"),
            "the flagged dependency must keep its name@version: msg shape:\n{display}"
        );
        assert_eq!(
            display.matches("\n debug@4.3.4").count(),
            1,
            "a dependency with problems must name itself exactly once, not twice:\n{display}"
        );
    }

    // ========================================================================
    // npm ls: regex (text tree) — the tier a plain `skim npm ls <pkg>` serves
    // ========================================================================

    /// F3, regex tier. This is the live path: `--json` is injected into npm's
    /// argv only when the user passes `--json` to skim, so a plain
    /// `skim npm ls ms` gets npm's text tree, `try_parse_ls_json` fails, and
    /// this tier renders. The input is the measured shape of `npm ls` on a
    /// two-dependency project (npm 10), tree glyphs included.
    #[test]
    fn test_ls_regex_keeps_versions() {
        let input = "f3-lab@1.0.0 /tmp/proj\n├── is-number@7.0.0\n└── ms@2.1.3\n";
        let result = try_parse_ls_regex(input).expect("regex tier must parse a tree");
        let display = format!("{result}");

        assert!(
            display.contains("npm list 2 total 0 flagged"),
            "the summary line must survive unchanged:\n{display}"
        );
        assert!(
            display.contains("ms@2.1.3"),
            "`ms@2.1.3` is the whole answer to `npm ls ms` and must reach the reader:\n{display}"
        );
        assert!(
            display.contains("is-number@7.0.0"),
            "every listed dependency must keep its name@version:\n{display}"
        );
        assert!(
            !display.contains('─'),
            "this is a compression layer: npm's tree art must not be reproduced:\n{display}"
        );
    }

    /// The ASCII fallback npm emits under `--no-unicode`, plus the `UNMET`
    /// marker. The marker is semantic, not decoration, so it must survive the
    /// prefix strip alongside the identity it qualifies — and it must still be
    /// counted as flagged.
    #[test]
    fn test_ls_regex_keeps_versions_ascii_tree_and_unmet() {
        let input = "f3-lab@1.0.0 /tmp/proj\n+-- UNMET DEPENDENCY is-number@7.0.0\n`-- ms@2.1.3\n";
        let result = try_parse_ls_regex(input).expect("regex tier must parse an ASCII tree");
        let display = format!("{result}");

        assert!(
            display.contains("npm list 2 total 1 flagged"),
            "the UNMET line must still be counted as flagged:\n{display}"
        );
        assert!(
            display.contains("UNMET DEPENDENCY is-number@7.0.0"),
            "the UNMET marker and the package it qualifies must both survive:\n{display}"
        );
        assert!(
            display.contains("ms@2.1.3"),
            "the healthy sibling must keep its name@version:\n{display}"
        );
        assert!(
            !display.contains("+--") && !display.contains("`--"),
            "the ASCII tree art must not be reproduced either:\n{display}"
        );
    }

    /// Pins the narrowing `strip_tree_prefix` documents: a nested entry keeps
    /// its identity and loses its depth. Asserted here rather than left in a
    /// comment, so a later reader finds the behaviour pinned instead of
    /// inferring it from output.
    #[test]
    fn test_ls_regex_flattens_nesting_but_keeps_identity() {
        let input = "f3-lab@1.0.0 /tmp/proj\n├─┬ a@1.0.0\n│ └── b@2.0.0\n└── ms@2.1.3\n";
        let result = try_parse_ls_regex(input).expect("regex tier must parse a nested tree");
        let display = format!("{result}");

        for expected in ["a@1.0.0", "b@2.0.0", "ms@2.1.3"] {
            assert!(
                display.contains(expected),
                "nested dependency {expected} lost its name@version:\n{display}"
            );
        }
        assert!(
            !display.contains('│'),
            "nesting is flattened, so no continuation glyph should be emitted:\n{display}"
        );
    }

    // ========================================================================
    // Three-tier integration
    // ========================================================================

    #[test]
    fn test_ls_json_produces_full() {
        let input = load_fixture("pkg", "npm_ls.json");
        let output = make_output(&input);
        let result = parse_ls(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_ls_garbage_produces_passthrough() {
        let output = make_output_full("completely unparseable output", "", Some(1));
        let result = parse_ls(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
