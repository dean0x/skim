//! The shape of a guidance file: where the skim block is, whether it is
//! current, and what is left of the file around it.
//!
//! Pure text code. It reads no file, writes no file and prints nothing, so the
//! install gate, the writer, the remover and `skim doctor` can all call it and
//! agree. `guidance.rs` does the file work.
//!
//! - **The block** runs from `<!-- skim-start` to the next `<!-- skim-end -->`.
//! - **Currency** is one predicate, [`block_status`]. "Current" means byte-equal
//!   to what this binary writes for that file.
//! - **An owned file** is one that skim alone writes (a Cursor `.mdc` file). It
//!   starts with frontmatter. A *skim-shaped* owned file holds nothing but
//!   copies of that frontmatter and one block, so skim rewrites it whole. An
//!   owned file that holds other text gets only its block replaced.

// ============================================================================
// Markers and formats
// ============================================================================

pub(super) const GUIDANCE_START: &str = "<!-- skim-start";
pub(super) const GUIDANCE_END: &str = "<!-- skim-end -->";

/// The frontmatter of the Cursor rule file, with the blank line that follows it.
pub(super) const CURSOR_RULE_FRONTMATTER: &str = "---\ndescription: \"skim — context-optimized code reading for AI agents\"\nalwaysApply: true\n---\n\n";

/// The longest marker text kept for display.
const MARKER_TEXT_LIMIT: usize = 64;

/// Whether the text of a guidance file speaks of a skim rewrite hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GuidanceHooks {
    /// Every agent that reads the file has a skim hook.
    Hook,
    /// At least one agent that reads the file has no skim hook.
    NoHook,
}

/// The layout of a guidance file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GuidanceFormat {
    /// The exact text that opens a file skim owns, with its blank line.
    pub(super) frontmatter: Option<&'static str>,
    /// Skim alone writes this file. It is rewritten whole when it is
    /// skim-shaped, and it is never filled in when it has no markers.
    pub(super) owned: bool,
    /// Whether the text speaks of a hook.
    pub(super) hooks: GuidanceHooks,
}

impl GuidanceFormat {
    /// A Markdown file that the user owns: `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`
    /// and the Copilot file.
    pub(super) const fn markdown(hooks: GuidanceHooks) -> Self {
        Self {
            frontmatter: None,
            owned: false,
            hooks,
        }
    }

    /// A Cursor rule file (`.mdc`), which skim owns.
    pub(super) const fn cursor_rule(hooks: GuidanceHooks) -> Self {
        Self {
            frontmatter: Some(CURSOR_RULE_FRONTMATTER),
            owned: true,
            hooks,
        }
    }

    /// The layout of the file at `path`: a `.mdc` extension makes a Cursor rule
    /// file.
    pub(super) fn for_path(path: &std::path::Path, hooks: GuidanceHooks) -> Self {
        if path.extension().is_some_and(|ext| ext == "mdc") {
            Self::cursor_rule(hooks)
        } else {
            Self::markdown(hooks)
        }
    }
}

// ============================================================================
// Finding the block
// ============================================================================

/// Find the skim guidance block in `content`.
///
/// Returns `Some((start_byte, end_byte))` where `end_byte` includes the end
/// marker. The end marker is the first one after the start marker, so an end
/// marker in front of the block does not matter. Returns `None` when there is
/// no start marker, or no end marker after it.
pub(super) fn find_skim_section(content: &str) -> Option<(usize, usize)> {
    let start = content.find(GUIDANCE_START)?;
    let end = content[start..].find(GUIDANCE_END)?;
    Some((start, start + end + GUIDANCE_END.len()))
}

// ============================================================================
// Currency
// ============================================================================

/// The version in a start marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Marker {
    /// `digits.digits.digits`, with an optional ASCII tag.
    Semver(String),
    /// Anything else: the marker text, cut to a fixed length.
    Unrecognized(String),
}

/// How the block in a file compares with the block this binary writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BlockStatus {
    /// The file holds no skim block.
    Absent,
    /// The file holds exactly what this binary writes.
    Current,
    /// The marker names another version (older or newer), or none we know.
    Stale { marker: Marker },
    /// The marker names this version, but the text differs.
    Changed,
    /// A start marker with no end marker after it.
    Broken,
}

/// Compare the block in `text` with `expected_block`, the block this binary
/// writes for the file.
///
/// The version of a marker is compared as a whole, so `v3.0.1` and `v3.0.10`
/// are two versions. A file that skim owns and that is skim-shaped is compared
/// whole. Any other file is compared by its block only, so text around the
/// block does not matter.
pub(super) fn block_status(
    text: &str,
    expected_block: &str,
    format: &GuidanceFormat,
) -> BlockStatus {
    if !text.contains(GUIDANCE_START) {
        return BlockStatus::Absent;
    }
    let Some((start, end)) = find_skim_section(text) else {
        return BlockStatus::Broken;
    };
    let found = marker_of(&text[start..]);
    if found != marker_of(expected_block) {
        return BlockStatus::Stale { marker: found };
    }
    let current = if format.owned && skim_shaped(text, format) {
        text == whole_file(format, expected_block)
    } else {
        &text[start..end] == expected_block
    };
    if current {
        BlockStatus::Current
    } else {
        BlockStatus::Changed
    }
}

/// The version marker at the start of `block`, which begins at a start marker.
fn marker_of(block: &str) -> Marker {
    let line = block.lines().next().unwrap_or("");
    let rest = line.strip_prefix(GUIDANCE_START).unwrap_or(line);
    match rest
        .strip_prefix(" v")
        .and_then(|version| version.strip_suffix(" -->"))
        .filter(|version| is_semver(version))
    {
        Some(version) => Marker::Semver(version.to_string()),
        None => Marker::Unrecognized(rest.trim().chars().take(MARKER_TEXT_LIMIT).collect()),
    }
}

/// `digits.digits.digits`, then an optional `-` or `+` and an ASCII tag.
fn is_semver(version: &str) -> bool {
    if version.len() > MARKER_TEXT_LIMIT {
        return false;
    }
    let (core, tag) = match version.find(['-', '+']) {
        Some(at) => (&version[..at], Some(&version[at + 1..])),
        None => (version, None),
    };
    let mut parts = core.split('.');
    let mut number = || {
        parts
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    let numbers = number() && number() && number();
    let tag_ok = match tag {
        None => true,
        Some(t) => {
            !t.is_empty()
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
        }
    };
    numbers && parts.next().is_none() && tag_ok
}

// ============================================================================
// Skim-shaped files
// ============================================================================

/// The whole text of a file that skim writes: the frontmatter, the block and a
/// final newline.
pub(super) fn whole_file(format: &GuidanceFormat, block: &str) -> String {
    format!("{}{block}\n", format.frontmatter.unwrap_or(""))
}

/// `true` when `text` holds nothing but copies of the frontmatter of `format`,
/// one skim block and blank lines.
///
/// A file of this shape has no text of the user, so skim may rewrite it whole
/// or delete it.
pub(super) fn skim_shaped(text: &str, format: &GuidanceFormat) -> bool {
    let Some((start, end)) = find_skim_section(text) else {
        return false;
    };
    if !text[end..].trim().is_empty() {
        return false;
    }
    let head = &text[..start];
    let rest = match format.frontmatter {
        Some(frontmatter) => strip_leading_copies(head, frontmatter).1,
        None => head,
    };
    rest.trim().is_empty()
}

/// Remove the copies of `frontmatter` at the start of `text`. Returns how many
/// there were and what is left.
fn strip_leading_copies<'a>(text: &'a str, frontmatter: &str) -> (usize, &'a str) {
    if frontmatter.is_empty() {
        return (0, text);
    }
    let mut rest = text;
    let mut count = 0;
    // Every pass removes at least one byte, so this many passes are enough.
    for _ in 0..=text.len() / frontmatter.len() {
        match rest.strip_prefix(frontmatter) {
            Some(after) => {
                rest = after;
                count += 1;
            }
            None => break,
        }
    }
    (count, rest)
}

/// `head` is the text in front of the start marker of an owned file. Drop the
/// copies of `frontmatter` that skim added: all but the first one at the start
/// of the file, and all that sit directly in front of the block.
///
/// Text of the user stays as it is. Only when a copy in front of the block is
/// dropped is the gap before the block set to one blank line.
fn without_duplicate_frontmatter(head: &str, frontmatter: &str) -> String {
    let (lead, after_lead) = strip_leading_copies(head, frontmatter);
    let mut kept = after_lead;
    let mut tail_dropped = false;
    if !frontmatter.is_empty() {
        // Every pass removes at least one byte, so this many passes are enough.
        for _ in 0..=after_lead.len() / frontmatter.len() {
            match kept.strip_suffix(frontmatter) {
                Some(before) => {
                    kept = before;
                    tail_dropped = true;
                }
                None => break,
            }
        }
    }
    let mut out = String::with_capacity(head.len());
    if lead > 0 {
        out.push_str(frontmatter);
    }
    out.push_str(kept);
    if tail_dropped && !kept.is_empty() {
        while !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out
}

// ============================================================================
// Replacing and removing the block
// ============================================================================

/// The text of the file after its block is replaced by `block`.
///
/// A skim-shaped owned file is rewritten whole, so it ends with exactly one
/// frontmatter. In any other file only the block changes, and the text around
/// it stays byte for byte, except for copies of the skim frontmatter that
/// stand in front of the block. Returns `None` when the file has no block.
pub(super) fn splice(text: &str, block: &str, format: &GuidanceFormat) -> Option<String> {
    let (start, end) = find_skim_section(text)?;
    if format.owned && skim_shaped(text, format) {
        return Some(whole_file(format, block));
    }
    let head = match format.frontmatter {
        Some(frontmatter) => without_duplicate_frontmatter(&text[..start], frontmatter),
        None => text[..start].to_string(),
    };
    Some(format!("{head}{block}{}", &text[end..]))
}

/// Remove the skim section from `content`, stripping surrounding blank lines.
///
/// `frontmatter` is the frontmatter of the file when skim owns it; copies of it
/// in front of the block go with the block. Returns `None` if no skim section
/// was found. Returns `Some(cleaned)` where `cleaned` is the trimmed remainder
/// with a trailing newline appended when non-empty.
pub(super) fn strip_section(content: &str, frontmatter: Option<&str>) -> Option<String> {
    let (start, end) = find_skim_section(content)?;
    let head = match frontmatter {
        Some(frontmatter) => without_duplicate_frontmatter(&content[..start], frontmatter),
        None => content[..start].to_string(),
    };
    let before = head.trim_end_matches('\n');
    let combined = format!("{before}{}", &content[end..]);
    let trimmed = combined.trim();
    Some(if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    })
}

/// [`strip_section`] for a file the user owns.
pub(super) fn strip_skim_section(content: &str) -> Option<String> {
    strip_section(content, None)
}

#[cfg(test)]
#[path = "guidance_block_tests.rs"]
mod tests;
