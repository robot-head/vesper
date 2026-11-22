//! The frozen Rust SVF bank render loops (`SvfBank::render` and
//! `SvfBank::render_reference`) and `PidController`, which the Vesper port
//! (`vesper/src/kernels.ves`) replaced in the package `vesper-resonance`
//! (SH6 lane 9, #1404).

use crate::kernels::{LANES, PidController, PidFault, PidGains, SvfBank, param};
use crate::simd::{ramp_tick, svf_tick};

/// Render frames `[start, end)` of `input` into `left`/`right` with the
/// bank's lane path.
pub(crate) fn render(
    bank: &mut SvfBank,
    input: &[f32],
    left: &mut [f32],
    right: &mut [f32],
    start: usize,
    end: usize,
) {
    for i in start..end {
        ramp_tick(bank.lanes, &mut bank.ramps[param::GAIN as usize]);
        ramp_tick(bank.lanes, &mut bank.ramps[param::G as usize]);
        ramp_tick(bank.lanes, &mut bank.ramps[param::K as usize]);
        bank.tile.gain = bank.ramps[param::GAIN as usize].current;
        bank.tile.g_coeff = bank.ramps[param::G as usize].current;
        bank.tile.k_damping = bank.ramps[param::K as usize].current;
        svf_tick(
            bank.lanes,
            &mut bank.tile,
            &bank.pan,
            input[i],
            &mut bank.scratch,
        );
        (left[i], right[i]) = bank.scratch.sum();
    }
}

/// The original fused scalar loop (wave five).
pub(crate) fn render_reference(
    bank: &mut SvfBank,
    input: &[f32],
    left: &mut [f32],
    right: &mut [f32],
    start: usize,
    end: usize,
) {
    let t = &mut bank.tile;
    for i in start..end {
        let v0 = input[i];
        let mut l = 0.0f32;
        let mut r = 0.0f32;
        for v in 0..LANES {
            let gain = bank.ramps[param::GAIN as usize].advance_lane_scalar(v);
            let g = bank.ramps[param::G as usize].advance_lane_scalar(v);
            let k = bank.ramps[param::K as usize].advance_lane_scalar(v);
            t.gain[v] = gain;
            t.g_coeff[v] = g;
            t.k_damping[v] = k;
            let a1 = 1.0 / (1.0 + g * (g + k));
            let a2 = g * a1;
            let a3 = g * a2;
            let v3 = v0 - t.ic2eq[v];
            let v1 = a1 * t.ic1eq[v] + a2 * v3;
            let v2 = t.ic2eq[v] + a2 * t.ic1eq[v] + a3 * v3;
            t.ic1eq[v] = 2.0 * v1 - t.ic1eq[v];
            t.ic2eq[v] = 2.0 * v2 - t.ic2eq[v];
            let y = gain * v2;
            let (pl, pr) = SvfBank::pan(v);
            l += y * pl;
            r += y * pr;
        }
        left[i] = l;
        right[i] = r;
    }
}

pub(crate) fn pid_new(gains: PidGains) -> Result<PidController, PidFault> {
    let all = [
        gains.kp,
        gains.ki,
        gains.kd,
        gains.dt,
        gains.torque_limit,
        gains.integral_limit,
    ];
    if all.iter().any(|g| !g.is_finite() || *g < 0.0) || gains.dt <= 0.0 {
        return Err(PidFault::BadGains);
    }
    Ok(PidController {
        gains,
        integral: 0.0,
        prev_measurement: None,
    })
}

pub(crate) fn pid_step(
    me: &mut PidController,
    setpoint: f32,
    measurement: f32,
) -> Result<f32, PidFault> {
    if !setpoint.is_finite() || !measurement.is_finite() {
        return Err(PidFault::NonFiniteInput);
    }
    let g = me.gains;
    // Compute every intermediate first; commit state only if all are
    // finite, so an overflow (e.g. `f32::MAX - -f32::MAX`) is a fault and
    // never poisons the integrator or yields an unclamped NaN torque.
    let error = setpoint - measurement;
    let integral = (me.integral + g.ki * error * g.dt).clamp(-g.integral_limit, g.integral_limit);
    let derivative = match me.prev_measurement {
        Some(prev) => -(measurement - prev) / g.dt,
        None => 0.0,
    };
    let u = g.kp * error + integral + g.kd * derivative;
    if ![error, integral, derivative, u]
        .iter()
        .all(|x| x.is_finite())
    {
        return Err(PidFault::NonFiniteIntermediate);
    }
    me.integral = integral;
    me.prev_measurement = Some(measurement);
    Ok(u.clamp(-g.torque_limit, g.torque_limit))
}
