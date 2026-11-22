//! Limits on the structure of input documents.

use crate::{Language, Result, SkimError};

/// Read Markdown as raw text when its nesting estimate reaches this limit.
const MARKDOWN_NESTING_LIMIT: usize = 200;

#[must_use = "the input limit must pass before the parse"]
pub(crate) fn check_grammar_limits(language: Language, source: &str) -> Result<()> {
    match language {
        Language::Markdown => check_markdown_limits(source),
        Language::TypeScript
        | Language::JavaScript
        | Language::Python
        | Language::Rust
        | Language::Go
        | Language::Java
        | Language::Json
        | Language::Yaml
        | Language::C
        | Language::Cpp
        | Language::Toml
        | Language::CSharp
        | Language::Ruby
        | Language::Sql
        | Language::Kotlin
        | Language::Swift
        | Language::Bash => Ok(()),
    }
}

fn check_markdown_limits(source: &str) -> Result<()> {
    let count = markdown_nesting_estimate(source);
    if count >= MARKDOWN_NESTING_LIMIT {
        return Err(SkimError::ComplexityLimit {
            what: "markdown nesting depth",
            count,
            max: MARKDOWN_NESTING_LIMIT - 1,
        });
    }
    Ok(())
}

/// Estimate the greatest nesting depth. Count every line, also lines in a fence.
/// Use one forward pass, constant space and no allocations.
/// Stop at the limit. A result of 200 or more is a lower bound on the estimate.
fn markdown_nesting_estimate(source: &str) -> usize {
    let mut rest = source.strip_prefix('\u{feff}').unwrap_or(source).as_bytes();
    let mut greatest = 1;
    while !rest.is_empty() {
        let (estimate, tail) = line_estimate(rest);
        if estimate >= MARKDOWN_NESTING_LIMIT {
            return estimate;
        }
        greatest = greatest.max(estimate);
        rest = next_line(tail);
    }
    greatest
}

fn next_line(tail: &[u8]) -> &[u8] {
    let Some(end) = tail.iter().position(|b| matches!(b, b'\r' | b'\n')) else {
        return &[];
    };
    let (through_end, rest) = tail.split_at(end + 1);
    if through_end.ends_with(b"\r") {
        rest.strip_prefix(b"\n").unwrap_or(rest)
    } else {
        rest
    }
}

fn marker_separator(tail: &[u8]) -> bool {
    matches!(tail.first(), None | Some(b' ' | b'\t' | b'\r' | b'\n'))
}

/// Count a leaf, quote and list markers, and half the whitespace columns.
/// Round the columns up. Count each tab as four columns.
fn line_estimate(mut tail: &[u8]) -> (usize, &[u8]) {
    let mut blocks = 1_usize;
    let mut columns = 0_usize;
    let mut estimate = blocks;
    while let Some((&byte, mut rest)) = tail.split_first() {
        match byte {
            b' ' => columns = columns.saturating_add(1),
            b'\t' => columns = columns.saturating_add(4),
            b'>' => blocks = blocks.saturating_add(1),
            b'-' | b'+' | b'*' => {
                blocks = blocks.saturating_add(usize::from(marker_separator(rest)));
            }
            b'0'..=b'9' => {
                rest = rest
                    .iter()
                    .position(|b| !b.is_ascii_digit())
                    .map_or(&[], |end| &rest[end..]);
                if let Some((b'.' | b')', after_marker)) = rest.split_first() {
                    blocks = blocks.saturating_add(usize::from(marker_separator(after_marker)));
                    rest = after_marker;
                }
            }
            b'.' | b')' => {}
            _ => break,
        }
        tail = rest;
        estimate = blocks.saturating_add(columns.div_ceil(2));
        if estimate >= MARKDOWN_NESTING_LIMIT {
            break;
        }
    }
    (estimate, tail)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{Parser, SkimError};

    fn assert_limit_edge(below: &str, at: &str) {
        assert_eq!(markdown_nesting_estimate(below), 199, "{below:?}");
        assert!(check_grammar_limits(Language::Markdown, below).is_ok());
        let result = check_grammar_limits(Language::Markdown, at);
        assert!(
            matches!(result, Err(SkimError::ComplexityLimit { count: 200, .. })),
            "{at:?}: {result:?}"
        );
    }

    #[test]
    fn markdown_quote_limit_includes_one_leaf() {
        assert_limit_edge(&">".repeat(198), &">".repeat(199));
    }

    #[test]
    fn markdown_limit_counts_spaces_and_tabs() {
        assert_limit_edge(&" ".repeat(396), &" ".repeat(397));
        let tabs = "\t".repeat(99);
        assert_limit_edge(&tabs, &format!("{tabs} "));
    }

    #[test]
    fn markdown_limit_counts_list_markers_with_separators() {
        for marker in ["-".into(), "+".into(), "*".into()]
            .into_iter()
            .chain((1..=9).flat_map(|digits| {
                let number = "1".repeat(digits);
                [format!("{number}."), format!("{number})")]
            }))
        {
            // A space adds one column. A tab adds four columns.
            for (separator, quotes) in [(" ", 196), ("\t", 195)] {
                let body = format!("{marker}{separator}text");
                let below = format!("{}{body}", ">".repeat(quotes));
                assert_limit_edge(&below, &format!(">{below}"));
            }

            // The marker before text does not count. The last marker counts.
            let prefix = ">".repeat(198);
            for end in ["", "\n", "\r", "\r\n"] {
                let at = format!("{prefix}{marker}{end}");
                let below = format!("{prefix}{marker}x{end}");
                assert_limit_edge(&below, &at);
            }
        }
    }

    #[test]
    fn markdown_limit_counts_only_the_last_dash_in_a_solid_break() {
        let dashes = "-".repeat(300);
        assert_limit_edge(
            &format!("{}{dashes}", ">".repeat(197)),
            &format!("{}{dashes}", ">".repeat(198)),
        );
    }

    #[test]
    fn markdown_limit_counts_lines_after_a_bom_and_mixed_line_ends() {
        for prefix in [
            "\u{feff}",
            "text\nother\r",
            "text\r\nother\n",
            "text\rother\r",
        ] {
            let below = format!("{prefix}{}", ">".repeat(198));
            assert_limit_edge(&below, &format!("{below}>"));
        }
        assert_limit_edge(
            &format!("text\n\u{feff}{}\n{}", ">".repeat(200), ">".repeat(198)),
            &format!("text\n{}", ">".repeat(199)),
        );
    }

    #[test]
    fn markdown_limit_accepts_large_shallow_text() {
        const SIZE: usize = 5 * 1024 * 1024;
        assert!(check_grammar_limits(Language::Markdown, &"x".repeat(SIZE)).is_ok());
        assert!(check_grammar_limits(Language::Markdown, &"x\n".repeat(SIZE / 2)).is_ok());
    }

    #[test]
    fn markdown_limit_rejects_a_large_deep_line() {
        let source = ">".repeat(5 * 1024 * 1024);
        let result = check_grammar_limits(Language::Markdown, &source);
        assert!(matches!(
            result,
            Err(SkimError::ComplexityLimit { count: 200, .. })
        ));
    }

    #[test]
    fn only_markdown_has_the_markdown_nesting_limit() {
        let source = ">".repeat(200);
        let markdown = Some(tree_sitter_md::LANGUAGE.into());
        for language in [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::Markdown,
            Language::Json,
            Language::Yaml,
            Language::C,
            Language::Cpp,
            Language::Toml,
            Language::CSharp,
            Language::Ruby,
            Language::Sql,
            Language::Kotlin,
            Language::Swift,
            Language::Bash,
        ] {
            let result = check_grammar_limits(language, &source);
            assert_eq!(
                result.is_err(),
                language.to_tree_sitter() == markdown,
                "{language:?}"
            );
            if let Err(error) = result {
                assert!(error.is_complexity_limit());
            }
        }
    }

    fn block_depths(tree: &tree_sitter::Tree) -> (usize, Option<usize>) {
        let mut pending = vec![(tree.root_node(), 0)];
        let mut deepest = 0;
        let mut fence_depth = None;
        while let Some((node, parent_depth)) = pending.pop() {
            let depth =
                parent_depth + usize::from(matches!(node.kind(), "block_quote" | "list_item"));
            deepest = deepest.max(depth);
            if node.kind() == "fenced_code_block" {
                fence_depth = Some(fence_depth.unwrap_or(0).max(depth));
            }
            pending.extend(
                node.named_children(&mut node.walk())
                    .map(|child| (child, depth)),
            );
        }
        (deepest, fence_depth)
    }

    #[test]
    fn markdown_just_below_the_limit_keeps_a_nested_fence() {
        let source = format!("# Report\n\n{}```\n", ">".repeat(198));
        let tree = Parser::new(Language::Markdown)
            .unwrap()
            .parse(&source)
            .unwrap();
        assert_eq!(block_depths(&tree), (198, Some(198)));
        assert_eq!(markdown_nesting_estimate(&source), 199);
    }

    fn pick<'a, T>(seed: &mut u32, choices: &'a [T]) -> &'a T {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        &choices[(*seed >> 16) as usize % choices.len()]
    }

    #[test]
    fn markdown_estimate_covers_the_nesting_of_generated_lines() {
        let mut seed = 631;
        let markers = [">", "-", "+", "*", ".", ")"];
        let spacing = ["", " ", "\t", " \t", "  "];
        let digits = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        let tails = [
            "",
            "text",
            "lazy continuation",
            "```rust",
            "```",
            "<div>",
            "</div>",
            "    code",
            "[id]: url",
            "-",
            "+",
            "*",
            "1.",
            "123456789)",
        ];
        let endings = ["\n", "\r", "\r\n"];
        let mut parser = Parser::new(Language::Markdown).unwrap();
        let mut max_depth = 0;
        for case in 0..512 {
            let mut source = if case % 2 == 0 {
                "\u{feff}".into()
            } else {
                String::new()
            };
            // Each document has at most 144 + 6 * (8 + 1) quote or list markers.
            if case % 4 == 0 {
                source.push_str(&">".repeat(144));
            }
            for _ in 0..6 {
                for _ in 0..*pick(&mut seed, &[0, 1, 4, 8]) {
                    let marker = *pick(&mut seed, &markers);
                    if matches!(marker, "." | ")") {
                        source.push_str(&"1".repeat(*pick(&mut seed, &digits)));
                    }
                    source.push_str(marker);
                    source.push_str(pick(&mut seed, &spacing));
                }
                source.push_str(pick(&mut seed, &tails));
                source.push_str(pick(&mut seed, &endings));
            }
            let tree = parser.parse(&source).unwrap();
            let depth = block_depths(&tree).0;
            assert!(
                markdown_nesting_estimate(&source) > depth,
                "{source:?}: {depth}"
            );
            max_depth = max_depth.max(depth);
        }
        assert!(max_depth >= 144);
    }
}
