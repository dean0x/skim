//! Pure run-budget selection. The caller owns paths, I/O and the rayon pool.

use crate::output::{ELISION_HINT, RunBudget, run_trailer_line};
use crate::process::raw_line_cut;
use rayon::prelude::*;
use rskim_core::{Language, TruncationCountSource, truncate_with_count_source};
use std::borrow::Cow;

type Counter<'a> = dyn Fn(&str) -> Option<usize> + Sync + 'a;
/// Chosen default, not measured. The conservative 2N stop can reject a BPE join
/// that would fit. It cannot accept an over-budget join.
pub(crate) const RUN_SUM_STOP_FACTOR: usize = 2;
pub(crate) const MAX_DROP_RETRIES: usize = 3;

/// Cuts count hidden view lines, not source lines. A worker pre-cut stores its
/// own final marker, including its newline and any cut-inside clause. A fixed
/// body is all-or-nothing and can carry a pass-1 source-line marker. Borrowed
/// text always refers to the pass-1 result for this file.
#[derive(Clone, Debug)]
pub(crate) enum Body<'a> {
    Whole(Cow<'a, str>),
    PreCut {
        kept: String,
        marker: String,
        total_view_lines: usize,
        marked_count: usize,
    },
    Fixed(Cow<'a, str>),
}

/// One path-ordered file. `count` covers prefix plus body. `floor` is the lesser
/// of that count and prefix plus compact marker; fixed bodies use their full
/// count. Workers compute these costs before the pure planner runs.
#[derive(Debug)]
pub(crate) struct Piece<'a> {
    pub(crate) prefix: String,
    pub(crate) body: Body<'a>,
    pub(crate) cut_language: Option<Language>,
    pub(crate) count: usize,
    pub(crate) floor: usize,
}

#[derive(Debug)]
pub(crate) struct PlannedBody {
    /// None keeps the original pass-1 text in its result.
    pub(crate) replacement: Option<String>,
    pub(crate) cut: bool,
}

#[derive(Debug)]
pub(crate) struct CutPlan {
    pub(crate) output: String,
    pub(crate) bodies: Vec<PlannedBody>,
    pub(crate) omitted: usize,
}

impl Body<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Whole(s) | Self::Fixed(s) => s.len(),
            Self::PreCut { kept, marker, .. } => kept.len().saturating_add(marker.len()),
        }
    }

    fn append_to(&self, output: &mut String) {
        match self {
            Self::Whole(s) | Self::Fixed(s) => output.push_str(s),
            Self::PreCut { kept, marker, .. } => {
                output.push_str(kept);
                output.push_str(marker);
            }
        }
    }

    fn whole(&self) -> PlannedBody {
        self.clone().into_planned()
    }

    pub(crate) fn into_planned(self) -> PlannedBody {
        let cut = matches!(self, Self::PreCut { .. });
        let replacement = match self {
            Self::Whole(Cow::Borrowed(_)) | Self::Fixed(Cow::Borrowed(_)) => None,
            Self::Whole(Cow::Owned(text)) | Self::Fixed(Cow::Owned(text)) => Some(text),
            Self::PreCut { kept, marker, .. } => Some(kept + &marker),
        };
        PlannedBody { replacement, cut }
    }
}

/// One fit rule for pass 1 and every rung. Bytes give a count-free upper bound.
/// Otherwise each piece must fit N, the saturating sum must fit 2N, and the
/// exact joined count must fit N. A missing count fails closed.
pub(crate) fn set_fits(pieces: &[Piece<'_>], n: usize, count: &Counter<'_>) -> Option<String> {
    let bytes = pieces.iter().fold(0usize, |sum, p| {
        sum.saturating_add(p.prefix.len())
            .saturating_add(p.body.len())
    });
    if bytes > n {
        let sum = pieces
            .iter()
            .fold(0usize, |sum, p| sum.saturating_add(p.count));
        if pieces.iter().any(|p| p.count > n) || sum > n.saturating_mul(RUN_SUM_STOP_FACTOR) {
            return None;
        }
    }
    let mut output = String::new();
    for p in pieces {
        output.push_str(&p.prefix);
        p.body.append_to(&mut output);
    }
    if bytes <= n || count(&output).is_some_and(|cost| cost <= n) {
        Some(output)
    } else {
        None
    }
}

/// Keep the largest path prefix whose floors and reserved trailer fit. Floors
/// use saturating sums in one linear pass, not a binary search over BPE counts.
fn kept_prefix(pieces: &[Piece<'_>], n: usize, trailer_cost: usize) -> usize {
    let available = n.saturating_sub(trailer_cost);
    let mut sum = 0usize;
    let mut kept = 0usize;
    for p in pieces {
        let next = sum.saturating_add(p.floor);
        if next > available || trailer_cost > n {
            break;
        }
        sum = next;
        kept = kept.saturating_add(1);
    }
    kept
}

/// Reserve the widest decimal fields once. Token joins need not be monotone;
/// the final exact check, not this reservation, enforces the run bound.
fn trailer_cost(total: usize, count: &Counter<'_>) -> usize {
    run_trailer_line(0, total).map_or(0, |line| {
        count(&line.replacen("(0 of", &format!("({total} of"), 1)).unwrap_or(usize::MAX)
    })
}

/// Each cuttable body gets its marker floor first. A single sorted pass gives
/// small bodies their full demand and splits the rest at one max-min level.
fn shares(
    pieces: &[Piece<'_>],
    n: usize,
    trailer: usize,
    count: &Counter<'_>,
) -> Vec<(usize, usize)> {
    let mut available = n.saturating_sub(trailer);
    let mut result = Vec::with_capacity(pieces.len());
    let mut demands = Vec::with_capacity(pieces.len());
    for (idx, p) in pieces.iter().enumerate() {
        let prefix = count(&p.prefix).unwrap_or(usize::MAX);
        available = available.saturating_sub(prefix);
        let real = match &p.body {
            Body::Whole(s) | Body::Fixed(s) => count(s).unwrap_or(usize::MAX),
            Body::PreCut { marked_count, .. } => *marked_count,
        };
        let floor = if matches!(p.body, Body::Fixed(_)) {
            real
        } else {
            p.floor.saturating_sub(prefix).min(real)
        };
        available = available.saturating_sub(floor);
        result.push((floor, real));
        if !matches!(p.body, Body::Fixed(_)) {
            demands.push((real.saturating_sub(floor), idx));
        }
    }
    demands.sort_unstable();
    for (rank, &(demand, idx)) in demands.iter().enumerate() {
        let level = available / demands.len().saturating_sub(rank);
        if demand <= level {
            result[idx].0 = result[idx].1;
            available = available.saturating_sub(demand);
        } else {
            for &(_, pending) in &demands[rank..] {
                result[pending].0 = result[pending]
                    .0
                    .saturating_add(level)
                    .min(result[pending].1);
            }
            break;
        }
    }
    result
}

/// Core adds the final newline after its fit check. Raw cuts count it directly.
pub(crate) fn core_cut_share(budget: RunBudget, text: &str, share: usize) -> usize {
    share.saturating_sub(usize::from(
        matches!(budget, RunBudget::Chars(_)) && text.ends_with('\n'),
    ))
}

fn core_cut(
    text: &str,
    language: Language,
    share: usize,
    known: usize,
    count: &Counter<'_>,
    source: TruncationCountSource<'_>,
    budget: RunBudget,
) -> Option<String> {
    truncate_with_count_source(
        text,
        language,
        core_cut_share(budget, text, share),
        |s| count(s).unwrap_or(usize::MAX),
        Some(known),
        Some(ELISION_HINT),
        source,
    )
    .ok()
}

fn cut_piece(
    p: &Piece<'_>,
    share: usize,
    real: usize,
    count: &Counter<'_>,
    budget: RunBudget,
) -> Option<PlannedBody> {
    let output = match &p.body {
        Body::Fixed(_) => return Some(p.body.whole()),
        Body::Whole(_) if real <= share => return Some(p.body.whole()),
        Body::PreCut {
            marked_count, kept, ..
        } if *marked_count <= share || kept.is_empty() => return Some(p.body.whole()),
        Body::Whole(s) => match p.cut_language {
            Some(language) => core_cut(
                s,
                language,
                share,
                real,
                count,
                TruncationCountSource::ViewLines,
                budget,
            )?,
            None if matches!(budget, RunBudget::Chars(_)) => {
                raw_line_cut(s, None, 0, |candidate| {
                    count(candidate).is_some_and(|cost| cost <= share)
                })
            }
            None => return None,
        },
        Body::PreCut {
            kept,
            total_view_lines,
            marked_count,
            ..
        } => core_cut(
            kept,
            p.cut_language?,
            share,
            *marked_count,
            count,
            TruncationCountSource::SourceTotal(*total_view_lines),
            budget,
        )?,
    };
    Some(PlannedBody {
        replacement: Some(output),
        cut: true,
    })
}

fn join_plan(pieces: &[Piece<'_>], bodies: &[PlannedBody], total: usize) -> String {
    let mut output = String::new();
    for (p, body) in pieces.iter().zip(bodies) {
        output.push_str(&p.prefix);
        if let Some(text) = &body.replacement {
            output.push_str(text);
        } else {
            p.body.append_to(&mut output);
        }
    }
    if let Some(trailer) = run_trailer_line(bodies.len(), total) {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&trailer);
    }
    output
}

fn trailer_only(total: usize) -> CutPlan {
    CutPlan {
        output: run_trailer_line(0, total).unwrap_or_default(),
        bodies: Vec::new(),
        omitted: total,
    }
}

pub(crate) fn plan_cut(pieces: &[Piece<'_>], n: usize, count: &Counter<'_>) -> CutPlan {
    plan_cut_inner(pieces, RunBudget::Tokens(n), count)
}

pub(crate) fn plan_char_cut(pieces: &[Piece<'_>], n: usize, count: &Counter<'_>) -> CutPlan {
    plan_cut_inner(pieces, RunBudget::Chars(n), count)
}

/// Plan once, cut each over-share body once in the caller's rayon pool, then
/// verify the exact stdout string. At most three last-file drops follow the
/// first exact check. Never return an unverified content candidate. The sole
/// floor exception is the trailer-only result, which can exceed a tiny N.
/// Returned bodies match the accepted string and support final-output stats.
fn plan_cut_inner(pieces: &[Piece<'_>], budget: RunBudget, count: &Counter<'_>) -> CutPlan {
    let (n, trailer_join) = match budget {
        RunBudget::Tokens(n) => (n, 0),
        RunBudget::Chars(n) => (n, 1),
    };
    if let Some(output) = set_fits(pieces, n, count) {
        return CutPlan {
            output,
            bodies: pieces.iter().map(|p| p.body.whole()).collect(),
            omitted: 0,
        };
    }
    let trailer = trailer_cost(pieces.len(), count).saturating_add(trailer_join);
    // Character floors add exactly. If they all fit, no trailer will print.
    // Token joins do not have this property. Keep their existing reserve.
    let k = if matches!(budget, RunBudget::Chars(_)) && kept_prefix(pieces, n, 0) == pieces.len() {
        pieces.len()
    } else {
        kept_prefix(pieces, n, trailer)
    };
    if k == 0 {
        return trailer_only(pieces.len());
    }
    let shares = shares(
        &pieces[..k],
        n,
        if k < pieces.len() { trailer } else { 0 },
        count,
    );
    let Some(mut bodies): Option<Vec<_>> = pieces[..k]
        .par_iter()
        .zip(shares)
        .map(|(p, (share, real))| cut_piece(p, share, real, count, budget))
        .collect()
    else {
        return trailer_only(pieces.len());
    };
    for attempt in 0..=MAX_DROP_RETRIES {
        let output = join_plan(pieces, &bodies, pieces.len());
        if count(&output).is_some_and(|cost| cost <= n) {
            return CutPlan {
                output,
                omitted: pieces.len().saturating_sub(bodies.len()),
                bodies,
            };
        }
        if attempt == MAX_DROP_RETRIES || bodies.len() <= 1 {
            break;
        }
        bodies.pop();
    }
    trailer_only(pieces.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{ELISION_HINT, run_trailer_line};
    use rskim_core::{ElidedSide, elision_marker_line};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn bytes(s: &str) -> Option<usize> {
        Some(s.len())
    }

    fn piece<'a>(prefix: &str, body: Body<'a>) -> Piece<'a> {
        let output = match &body {
            Body::Whole(s) | Body::Fixed(s) => s.to_string(),
            Body::PreCut { kept, marker, .. } => format!("{kept}{marker}"),
        };
        let floor = match &body {
            Body::Fixed(_) => prefix.len().saturating_add(output.len()),
            _ => {
                let marker = elision_marker_line(
                    Some(Language::Rust),
                    output.lines().count(),
                    ElidedSide::Truncated,
                    None,
                );
                prefix
                    .len()
                    .saturating_add(marker.len().saturating_add(1))
                    .min(prefix.len().saturating_add(output.len()))
            }
        };
        Piece {
            prefix: prefix.to_owned(),
            body,
            cut_language: Some(Language::Rust),
            count: prefix.len().saturating_add(output.len()),
            floor,
        }
    }

    fn whole(text: &str) -> Piece<'static> {
        piece("", Body::Whole(text.to_owned().into()))
    }

    fn many_lines(n: usize) -> String {
        (0..n).map(|i| format!("let x{i} = {i};\n")).collect()
    }

    #[test]
    fn char_core_cut_fits_the_final_newline_at_the_exact_share() {
        let chars = |s: &str| Some(s.chars().count());
        let first = "// A résumé with enough characters for this test to reach the core cut";
        let marker = elision_marker_line(
            Some(Language::Rust),
            1,
            ElidedSide::Truncated,
            Some(ELISION_HINT),
        );
        for ending in ["", "\n"] {
            let input = whole(&format!("{first}\n// {}{ending}", "é".repeat(200)));
            let expected = format!("{first}\n{marker}{ending}");
            let share = expected.chars().count();
            let plan = plan_char_cut(&[input], share, &chars);
            assert_eq!(plan.output, expected);
            assert!(plan.bodies[0].cut);
        }
        let input = whole(&format!("{first}\n// {}\n", "é".repeat(200)));
        let share = format!("{first}\n{marker}").chars().count();
        let plan = plan_char_cut(&[input], share, &chars);
        assert_eq!(plan.omitted, 0);
        assert_eq!(plan.output, "// ... (2 lines truncated)\n");
    }

    #[test]
    fn char_raw_cut_uses_the_full_share_and_keeps_the_following_file() {
        let chars = |s: &str| Some(s.chars().count());
        let first = "A line with enough characters to reach the raw cut: élan";
        let mut raw = whole(&format!("{first}\n{}\nlast", "é".repeat(200)));
        raw.cut_language = None;
        let marker = elision_marker_line(None, 2, ElidedSide::Truncated, Some(ELISION_HINT));
        let expected = format!("{first}\n{marker}\nfn f();\n");
        let plan = plan_char_cut(&[raw, whole("fn f();\n")], expected.chars().count(), &chars);
        assert_eq!(plan.output, expected);
        assert!(plan.bodies[0].cut);
        assert!(!plan.bodies[1].cut);
    }

    #[test]
    fn char_cut_keeps_every_file_when_the_cut_bodies_fit_without_a_trailer() {
        let chars = |s: &str| Some(s.chars().count());
        let short = "fn small();\n";
        let marker = "// ... (2 lines truncated)\n";
        let input = [whole(&format!("{}\nlast\n", "x".repeat(200))), whole(short)];
        let expected = format!("{marker}{short}");
        let plan = plan_char_cut(&input, expected.chars().count(), &chars);
        assert_eq!(plan.output, expected);
        assert_eq!(plan.omitted, 0);
    }

    #[test]
    fn char_cut_reserves_the_join_newline_before_the_trailer() {
        let chars = |s: &str| Some(s.chars().count());
        let first = "// first";
        let marker = elision_marker_line(
            Some(Language::Rust),
            1,
            ElidedSide::Truncated,
            Some(ELISION_HINT),
        );
        let trailer = run_trailer_line(1, 2).unwrap();
        let input = [
            whole(&format!("{first}\n// {}", "x".repeat(200))),
            piece("", Body::Fixed("x".repeat(200).into())),
        ];
        let budget = format!("{first}\n{marker}{trailer}").chars().count();
        let plan = plan_char_cut(&input, budget, &chars);
        assert_eq!(
            plan.output,
            format!("// ... (2 lines truncated)\n{trailer}")
        );
    }

    #[test]
    fn failed_counter_returns_only_the_trailer() {
        let calls = AtomicUsize::new(0);
        let count = |_: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            None
        };
        let plan = plan_cut(&[whole(&many_lines(100))], 100, &count);
        assert_eq!(plan.output, run_trailer_line(0, 1).unwrap());
        assert!(plan.bodies.is_empty());
        assert_eq!(plan.omitted, 1);
        assert!(calls.load(Ordering::Relaxed) <= 1 + MAX_DROP_RETRIES + 2);
    }

    #[test]
    fn joined_overcounts_stop_after_three_drops() {
        let joined_sizes = std::sync::Mutex::new(Vec::new());
        let count = |s: &str| {
            if s.starts_with("HEADER\n") && s != "HEADER\n" {
                joined_sizes
                    .lock()
                    .unwrap()
                    .push(s.matches("HEADER\n").count());
                Some(1001)
            } else {
                bytes(s)
            }
        };
        let pieces: Vec<_> = (0..8)
            .map(|_| piece("HEADER\n", Body::Whole(many_lines(200).into())))
            .collect();
        let plan = plan_cut(&pieces, 1000, &count);
        assert_eq!(plan.output, run_trailer_line(0, 8).unwrap());
        assert_eq!(*joined_sizes.lock().unwrap(), vec![8, 7, 6, 5]);
        assert_eq!(joined_sizes.lock().unwrap().len(), 1 + MAX_DROP_RETRIES);
        assert_eq!(plan.omitted, 8);
    }

    #[test]
    fn headers_over_budget_return_the_trailer_without_underflow() {
        let pieces = [piece(&"H".repeat(200), Body::Fixed("x".into()))];
        let plan = plan_cut(&pieces, 1, &bytes);
        assert_eq!(plan.output, run_trailer_line(0, 1).unwrap());
        assert_eq!(plan.omitted, 1);
    }

    #[test]
    fn byte_fit_never_calls_the_counter() {
        let pieces = [whole("abc"), whole("def")];
        assert_eq!(
            set_fits(&pieces, 6, &|_| panic!("byte fit needs no count")),
            Some("abcdef".to_owned())
        );
    }

    #[test]
    fn linear_prefix_is_largest_and_sums_do_not_wrap() {
        let mut pieces = [whole("a"), whole("b"), whole("c")];
        for (p, floor) in pieces.iter_mut().zip([20, 30, 50]) {
            p.floor = floor;
        }
        assert_eq!(kept_prefix(&pieces, 100, 10), 2);
        pieces[0].floor = usize::MAX - 2;
        pieces[1].floor = 3;
        assert_eq!(kept_prefix(&pieces, usize::MAX - 1, 0), 1);
        pieces[0].floor = 0;
        assert_eq!(kept_prefix(&pieces, 0, 1), 0);
    }

    #[test]
    fn none_counter_adapter_leaves_only_an_exact_marker() {
        let output = core_cut(
            "a\nb\nc\n",
            Language::Rust,
            100,
            usize::MAX,
            &|_| None,
            TruncationCountSource::ViewLines,
            RunBudget::Tokens(100),
        )
        .unwrap();
        assert_eq!(
            output,
            format!(
                "{}\n",
                elision_marker_line(Some(Language::Rust), 3, ElidedSide::Truncated, None)
            )
        );
    }

    fn precut(kept: String, total: usize) -> Body<'static> {
        let marker = format!(
            "{}\n",
            elision_marker_line(
                Some(Language::Rust),
                total.saturating_sub(kept.lines().count()),
                ElidedSide::Truncated,
                Some(ELISION_HINT)
            )
        );
        let marked_count = kept.len().saturating_add(marker.len());
        Body::PreCut {
            kept,
            marker,
            total_view_lines: total,
            marked_count,
        }
    }

    fn assert_one_exact_marker(plan: &CutPlan, total: usize, n: usize) {
        assert_eq!(plan.bodies.len(), 1);
        assert!(plan.bodies[0].cut);
        let lines: Vec<_> = plan.bodies[0]
            .replacement
            .as_deref()
            .unwrap()
            .lines()
            .collect();
        let kept = lines.len() - 1;
        assert_eq!(
            lines.last().copied(),
            Some(
                elision_marker_line(
                    Some(Language::Rust),
                    total - kept,
                    ElidedSide::Truncated,
                    Some(ELISION_HINT)
                )
                .as_str()
            )
        );
        assert_eq!(plan.output.matches("lines truncated").count(), 1);
        assert!(plan.output.len() <= n);
    }

    #[test]
    fn precut_recuts_in_total_view_line_space_with_one_marker() {
        let plan = plan_cut(&[piece("", precut(many_lines(100), 500))], 250, &bytes);
        assert_one_exact_marker(&plan, 500, 250);
    }

    #[test]
    fn precut_recounts_the_marker_even_when_kept_lines_fit() {
        let kept = many_lines(10);
        let n = kept.len() + 1;
        let body = precut(kept.clone(), 1000);
        let plan = plan_cut(&[piece("", body)], n, &bytes);
        assert_one_exact_marker(&plan, 1000, n);
        assert!(
            plan.bodies[0]
                .replacement
                .as_deref()
                .unwrap()
                .lines()
                .count()
                - 1
                < kept.lines().count()
        );
    }

    #[test]
    fn trailer_starts_on_its_own_line_after_a_fixed_body() {
        let pieces = [
            piece("", Body::Fixed("small".into())),
            piece("", Body::Fixed("x".repeat(500).into())),
        ];
        let n = 5 + run_trailer_line(0, 2).unwrap().len() + 1;
        let plan = plan_cut(&pieces, n, &bytes);
        assert_eq!(
            plan.output,
            format!("small\n{}", run_trailer_line(1, 2).unwrap())
        );
        assert_eq!(plan.omitted, 1);
        assert!(!plan.bodies[0].cut);
    }

    #[test]
    fn max_min_shares_keep_small_later_files_whole() {
        let pieces = [whole(&many_lines(500)), whole("small\n"), whole("tiny\n")];
        let plan = plan_cut(&pieces, 400, &bytes);
        assert_eq!(plan.bodies.len(), 3);
        assert_eq!(plan.omitted, 0);
        assert!(plan.bodies[0].cut);
        assert_eq!(plan.bodies[1].replacement.as_deref().unwrap(), "small\n");
        assert_eq!(plan.bodies[2].replacement.as_deref().unwrap(), "tiny\n");
        assert!(plan.output.len() <= 400);
    }

    #[test]
    fn one_fit_rule_rejects_each_independent_stop_condition() {
        let calls = AtomicUsize::new(0);
        let count = |_: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            Some(11)
        };
        let mut pieces = [
            whole(&"a".repeat(100)),
            whole(&"b".repeat(100)),
            whole(&"c".repeat(100)),
        ];
        for p in &mut pieces {
            p.count = 10;
        }
        assert_eq!(set_fits(&pieces, 10, &count), None); // sum over 2N
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        pieces[0].count = 11;
        assert_eq!(set_fits(&pieces[..1], 10, &count), None); // one piece over N
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        pieces[0].count = 10;
        assert_eq!(set_fits(&pieces[..1], 10, &count), None); // exact join over N
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            set_fits(&pieces[..1], 10, &|_| Some(10)),
            Some("a".repeat(100))
        );
        assert_eq!(RUN_SUM_STOP_FACTOR, 2);
    }

    #[test]
    fn whole_set_fit_does_not_reserve_an_unneeded_trailer() {
        let pieces = [whole("abc"), whole("def")];
        let plan = plan_cut(&pieces, 6, &bytes);
        assert_eq!(plan.output, "abcdef");
        assert_eq!(plan.bodies.len(), 2);
        assert_eq!(plan.omitted, 0);
        assert!(run_trailer_line(0, 2).unwrap().len() > 6);
    }

    #[test]
    fn precut_within_its_share_keeps_the_worker_marker_verbatim() {
        let marker = "// ... (10 lines truncated; cut inside multiline literal)\n";
        let body = Body::PreCut {
            kept: "fn a() {}\n".into(),
            marker: marker.into(),
            total_view_lines: 11,
            marked_count: 68,
        };
        let first = piece("", body);
        let expected = format!("fn a() {{}}\n{marker}");
        assert_eq!(expected.len(), 68);
        let plan = plan_cut(&[first, whole(&many_lines(500))], 500, &bytes);
        assert_eq!(plan.bodies.len(), 2);
        assert_eq!(plan.bodies[0].replacement.as_deref().unwrap(), expected);
        assert!(plan.bodies[0].cut);
    }

    #[test]
    fn marker_only_precut_never_loses_its_disclosure() {
        let marker = format!(
            "// ... ({} lines truncated; cut inside multiline literal)\n",
            usize::MAX
        );
        let n = 200;
        let body = Body::PreCut {
            kept: String::new(),
            marked_count: marker.len(),
            marker: marker.clone(),
            total_view_lines: usize::MAX,
        };
        let plan = plan_cut(
            &[
                piece("", body),
                whole(&many_lines(500)),
                whole(&many_lines(500)),
            ],
            n,
            &bytes,
        );
        assert!(!plan.bodies.is_empty());
        assert_eq!(plan.bodies[0].replacement.as_deref().unwrap(), marker);
        assert!(plan.output.len() <= n);
    }

    #[test]
    fn caller_pool_size_does_not_change_the_plan() {
        let pieces = [whole(&many_lines(100)), whole(&many_lines(50))];
        let run = |jobs| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(jobs)
                .build()
                .unwrap()
                .install(|| plan_cut(&pieces, 500, &bytes))
        };
        let one = run(1);
        let four = run(4);
        assert!(!one.output.is_empty());
        assert_eq!(one.output, four.output);
        assert_eq!(one.omitted, four.omitted);
    }
}
