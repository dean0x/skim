//! Shared timing helpers for the scaling guards in `pseudo.rs` and `minimal.rs`.
//!
//! # Clock (thread CPU time)
//!
//! On unix, a sample is the **CPU time of the calling thread**
//! (`CLOCK_THREAD_CPUTIME_ID`), not wall-clock time. On a loaded CI runner the
//! scheduler can take the thread off the CPU in the middle of a sample. Wall
//! time counts that wait and CPU time does not, so a busy runner has much less
//! effect on a ratio. CPU time still counts some interference: cache refill
//! after a switch, a busy SMT sibling, frequency changes. The 4× step below
//! gives the margin for that part. Other targets fall back to `Instant`; CI runs
//! the guards on Linux only.
//!
//! # Paired, alternate samples
//!
//! A ratio guard measures its small and its large input with
//! [`time_pair_5`]. The two inputs run in turn (small, large, small, large, …),
//! so a change in runner load during the guard reaches both sides. Two
//! back-to-back batches let a load change fall on one side only, and that
//! skews the ratio by itself.
//!
//! # Sampling rule (workspace standard)
//!
//! **Ratio gates** (growth-of-N tests: assert `ratio < THRESHOLD`) use the
//! **minimum** of 5 samples.  Rationale: interference inflates samples and
//! never deflates them; taking the minimum discards the spikes while keeping
//! the true algorithmic signal.
//!
//! **Absolute gates** (single-threshold tests: assert `t >= FLOOR` or
//! `t < BUDGET`) use the **median** of 5 samples.  Rationale: the minimum
//! could be unrepresentatively fast (warm branch-predictor, etc.), making
//! the noise-floor assertion vacuous; the median is a better estimate of
//! the typical cost.
//!
//! # Parse hoisting
//!
//! All helpers hoist the tree-sitter parse **outside** the 5-sample loop.
//! We are measuring the transform walk, not the parser.  A parse inside the
//! loop inflates every sample by a constant (fast) parse cost and narrows
//! the ratio signal; hoisting it keeps the measurement honest.
//!
//! # Threshold derivation
//!
//! Every ratio guard grows N 4× and asserts a ratio below 8.0 = 4^1.5, the
//! exponent-space midpoint between linear growth (4^1.0 = 4×) and quadratic
//! growth (4^2.0 = 16×).  Choosing the midpoint gives equal log-scale margin
//! to both sides, whereas an empirically fitted constant drifts as hardware
//! changes.  Correct implementations measure 3.95×–4.36×, so 8.0 allows
//! ~1.9× runner noise.
//!
//! The earlier doubling form (2× growth against 2.8 ≈ 2^1.5) measured ~2.0×
//! and allowed only ~1.4× noise.  It failed on CI runners at 2.93×, 3.16× and
//! 3.20× with no walker change (#633, #635).

/// Minimum and median of one 5-sample measurement, in milliseconds.
pub(crate) type MinMedian = (f64, f64);

/// Number of samples per input.
const SAMPLES: usize = 5;

/// CPU time of the calling thread, in milliseconds (see the module doc).
#[cfg(unix)]
fn thread_cpu_ms() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable `timespec` that lives for the whole
    // call, and `CLOCK_THREAD_CPUTIME_ID` is a clock id that Linux and macOS
    // define. The call writes only into `ts`.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    assert_eq!(
        rc,
        0,
        "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed: {}",
        std::io::Error::last_os_error()
    );
    ts.tv_sec as f64 * 1000.0 + ts.tv_nsec as f64 / 1_000_000.0
}

/// Milliseconds that one call of `f` takes on the clock of the module doc.
fn sample_ms<F: FnMut()>(mut f: F) -> f64 {
    #[cfg(unix)]
    {
        let start = thread_cpu_ms();
        f();
        thread_cpu_ms() - start
    }
    #[cfg(not(unix))]
    {
        let start = std::time::Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    }
}

/// `(min, median)` of `samples`.
fn min_median(mut samples: [f64; SAMPLES]) -> MinMedian {
    samples.sort_by(f64::total_cmp);
    (samples[0], samples[SAMPLES / 2])
}

/// Run `f` exactly 5 times and return `(min, median)` in milliseconds.
///
/// Use this for an *absolute gate*: `assert!(median < BUDGET)`.
///
/// The parse step must be *outside* the closure — `f` should only run the
/// transform, not re-parse the source.
pub(crate) fn time_5<F: FnMut()>(mut f: F) -> MinMedian {
    min_median(std::array::from_fn(|_| sample_ms(&mut f)))
}

/// Run `small` and `large` 5 times each, in turn, and return their
/// `(min, median)` in milliseconds, in that order.
///
/// Use this for a *ratio gate*: `let ratio = large.0 / small.0;`. The noise
/// floor of the guard reads `small.1`.
///
/// The parse steps must be *outside* the closures.
pub(crate) fn time_pair_5<S: FnMut(), L: FnMut()>(
    mut small: S,
    mut large: L,
) -> (MinMedian, MinMedian) {
    let mut small_ms = [0.0; SAMPLES];
    let mut large_ms = [0.0; SAMPLES];
    for i in 0..SAMPLES {
        small_ms[i] = sample_ms(&mut small);
        large_ms[i] = sample_ms(&mut large);
    }
    (min_median(small_ms), min_median(large_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_median_reads_the_lowest_and_the_middle_sample() {
        assert_eq!(min_median([5.0, 1.0, 4.0, 2.0, 3.0]), (1.0, 3.0));
    }

    #[test]
    fn time_pair_5_runs_each_side_five_times_in_turn() {
        let order = std::cell::RefCell::new(Vec::new());
        let _ = time_pair_5(
            || order.borrow_mut().push('s'),
            || order.borrow_mut().push('l'),
        );
        assert_eq!(order.into_inner(), "slslslslsl".chars().collect::<Vec<_>>());
    }

    /// A sleep takes no CPU, so the CPU clock must not count it. This is the
    /// property that keeps a descheduled sample out of a ratio.
    #[cfg(unix)]
    #[test]
    fn sample_ms_does_not_count_time_off_the_cpu() {
        let ms = sample_ms(|| std::thread::sleep(std::time::Duration::from_millis(50)));
        assert!(
            ms < 25.0,
            "a 50 ms sleep took {ms:.3} ms of thread CPU time"
        );
    }
}
