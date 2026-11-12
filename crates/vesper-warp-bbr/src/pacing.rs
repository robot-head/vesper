//! Pure integer pacing and window arithmetic.
//!
//! Every function here is transcribed one-for-one in `lean/WarpBbr.lean`
//! (`pacingDelay`, `edtStep`, `targetCwnd`, `sendAllowed`), where the
//! monotonicity, clamp and send-guard statements are proved. The
//! differential test `tests/lean_model.rs` runs the Lean definitions on the
//! same inputs. The Lean proofs are about the Lean transcription, not a
//! machine-checked statement about this Rust code.

/// Nanoseconds per second.
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Delay, in nanoseconds, that pacing `bytes` at `rate_bytes_per_sec` takes.
///
/// The result is `ceil(bytes * 10^9 / rate)`, so a paced flow never exceeds
/// the configured rate; it falls short of it by less than one nanosecond per
/// packet. The product is computed in `u128`. Because `bytes <= u32::MAX`,
/// the quotient is at most `4.3 * 10^18 < u64::MAX`, so it is exact.
///
/// A zero rate is defined as "infinitely slow": the delay saturates to
/// `u64::MAX`, which makes every later departure saturate at `u64::MAX`.
/// The controller never sets a zero pacing rate (it floors at one byte per
/// second), so this case is only reachable through this pure function.
#[must_use]
pub const fn pacing_delay_ns(bytes: u32, rate_bytes_per_sec: u64) -> u64 {
    if rate_bytes_per_sec == 0 {
        return u64::MAX;
    }
    let numerator = bytes as u128 * NANOS_PER_SEC as u128;
    let delay = numerator.div_ceil(rate_bytes_per_sec as u128);
    if delay > u64::MAX as u128 {
        u64::MAX
    } else {
        delay as u64
    }
}

/// One earliest-departure-time step.
///
/// Returns `(launch, next)` where `launch = max(now, prev_next)` and
/// `next = launch.saturating_add(delay)`. Saturation at `u64::MAX` is the
/// documented behaviour at the end of the clock; it keeps both results
/// monotone (`bbrv3_edt_launch_monotonic` in `lean/WarpBbr.lean`).
#[must_use]
pub const fn edt_step(prev_next_ns: u64, now_ns: u64, delay_ns: u64) -> (u64, u64) {
    let launch = if now_ns > prev_next_ns {
        now_ns
    } else {
        prev_next_ns
    };
    (launch, launch.saturating_add(delay_ns))
}

/// The §35.5 congestion-window clamp, widened to `u64` so the bounds are
/// exact: the result lies in `[4 * mss, max(inflight_hi, 4 * mss)]`.
///
/// `inflight_hi == u64::MAX` means "no upper bound yet".
#[must_use]
pub const fn clamp_target_cwnd(cwnd: u64, mss: u32, inflight_hi: u64) -> u64 {
    let floor = mss as u64 * 4;
    let ceiling = if inflight_hi > floor {
        inflight_hi
    } else {
        floor
    };
    let raised = if cwnd > floor { cwnd } else { floor };
    if raised < ceiling { raised } else { ceiling }
}

/// The congestion-window send guard: a new in-flight packet may be sent only
/// while `bytes_in_flight < cwnd`.
///
/// If every send passes this guard and no packet exceeds `max_packet`, then
/// right after each send `bytes_in_flight < cwnd + max_packet`
/// (`send_respects_cwnd` in `lean/WarpBbr.lean`). This is a statement about
/// send transitions only: after the window is cut below the current flight,
/// in-flight bytes can exceed `cwnd + max_packet`, and the guard then blocks
/// every send until enough data is acknowledged or declared lost.
#[must_use]
pub const fn send_allowed(bytes_in_flight: u64, cwnd: u64) -> bool {
    bytes_in_flight < cwnd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_delay_is_ceiling_and_defined_at_zero() {
        assert_eq!(pacing_delay_ns(1500, 1_500_000_000), 1000);
        assert_eq!(pacing_delay_ns(1, 3), 333_333_334);
        assert_eq!(pacing_delay_ns(0, 7), 0);
        assert_eq!(pacing_delay_ns(1200, 0), u64::MAX);
        assert_eq!(
            pacing_delay_ns(u32::MAX, 1),
            u32::MAX as u64 * NANOS_PER_SEC
        );
        assert_eq!(pacing_delay_ns(1, u64::MAX), 1);
    }

    #[test]
    fn edt_step_saturates_and_never_regresses() {
        assert_eq!(edt_step(10, 5, 3), (10, 13));
        assert_eq!(edt_step(10, 50, 3), (50, 53));
        assert_eq!(edt_step(u64::MAX - 1, 0, 9), (u64::MAX - 1, u64::MAX));
        assert_eq!(edt_step(u64::MAX, u64::MAX, u64::MAX), (u64::MAX, u64::MAX));
    }

    #[test]
    fn clamp_bounds() {
        assert_eq!(clamp_target_cwnd(0, 1200, u64::MAX), 4800);
        assert_eq!(clamp_target_cwnd(10_000, 1200, u64::MAX), 10_000);
        assert_eq!(clamp_target_cwnd(10_000, 1200, 6000), 6000);
        // inflight_hi below the floor: the floor wins.
        assert_eq!(clamp_target_cwnd(10_000, 1200, 100), 4800);
        assert_eq!(clamp_target_cwnd(u64::MAX, u32::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn guard() {
        assert!(send_allowed(0, 1));
        assert!(!send_allowed(1, 1));
        assert!(!send_allowed(0, 0));
    }
}
