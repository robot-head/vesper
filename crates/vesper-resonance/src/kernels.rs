//! The §32.3 kernels: a 16-voice state-variable-filter bank with
//! sample-accurate parameter events, the realtime synth quantum, and a 1 kHz
//! PID torque controller.
//!
//! The SVF is the trapezoidal (TPT) state-variable filter (Simper/Zavalishin
//! form) in low-pass mode. Per voice and sample, with input `v0`:
//!
//! ```text
//! a1 = 1 / (1 + g (g + k)); a2 = g a1; a3 = g a2
//! v3 = v0 - ic2eq
//! v1 = a1 ic1eq + a2 v3
//! v2 = ic2eq + a2 ic1eq + a3 v3
//! ic1eq = 2 v1 - ic1eq; ic2eq = 2 v2 - ic2eq
//! y = gain * v2
//! ```
//!
//! `g = tan(pi fc / fs)` and `k = 1 / Q` are parameters, ramped per sample.
//! The tile is the §32.3 AoSoA layout: one 64-byte line per hot field
//! across 16 lanes.

use crate::events::{
    EventError, ParamRamp, ParamTable, RtParamEvent, decode_events, process_with_events,
};
use crate::lease::{RtBufferLease, RtOutputPort};
use crate::spsc::{MailboxReceiver, RtRetirementSink, SwapOutcome};

/// Voices per tile.
pub const LANES: usize = 16;
/// Frames per realtime quantum (the design's `AlignedSlice[f32, 64]`).
pub const QUANTUM_FRAMES: usize = 64;
/// Maximum events accepted per quantum (`poll_param_events(32)`).
pub const MAX_EVENTS: usize = 32;

/// Parameter ids of a voice.
pub mod param {
    /// Output gain.
    pub const GAIN: u32 = 0;
    /// Warped cutoff coefficient `g = tan(pi fc / fs)`.
    pub const G: u32 = 1;
    /// Damping `k = 1 / Q`.
    pub const K: u32 = 2;
}

/// 16-voice AoSoA SVF state (each hot field is one 64-byte line).
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, align(64))]
pub struct SvfVoiceTile {
    /// First integrator state.
    pub ic1eq: [f32; LANES],
    /// Second integrator state.
    pub ic2eq: [f32; LANES],
    /// Cutoff coefficient.
    pub g_coeff: [f32; LANES],
    /// Damping.
    pub k_damping: [f32; LANES],
    /// Gain.
    pub gain: [f32; LANES],
    /// Patch identifier (cold).
    pub patch_id: [u32; LANES],
}

/// Per-voice patch values, swapped in from a non-realtime thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoicePatch {
    /// Cutoff coefficient.
    pub g: f32,
    /// Damping.
    pub k: f32,
    /// Gain.
    pub gain: f32,
    /// Patch id.
    pub patch_id: u32,
}

/// A full patch bank (heap-allocated by the UI, retired off the realtime thread).
#[derive(Clone, Debug, PartialEq)]
pub struct VoicePatchBank {
    /// One patch per voice.
    pub voices: [VoicePatch; LANES],
}

/// The voice bank: tile state plus per-sample parameter ramps.
#[derive(Clone, Debug)]
pub struct SvfBank {
    /// Filter state.
    pub tile: SvfVoiceTile,
    ramps: [[ParamRamp; 3]; LANES],
}

impl SvfBank {
    /// A silent bank with every voice at `patch`.
    pub fn new(patch: VoicePatch) -> Self {
        let mut bank = SvfBank {
            tile: SvfVoiceTile {
                ic1eq: [0.0; LANES],
                ic2eq: [0.0; LANES],
                g_coeff: [patch.g; LANES],
                k_damping: [patch.k; LANES],
                gain: [patch.gain; LANES],
                patch_id: [patch.patch_id; LANES],
            },
            ramps: [[ParamRamp::new(0.0); 3]; LANES],
        };
        bank.apply_patch(&VoicePatchBank {
            voices: [patch; LANES],
        });
        bank
    }

    /// Load a patch bank (jumps every parameter; filter state is kept).
    pub fn apply_patch(&mut self, bank: &VoicePatchBank) {
        for (v, p) in bank.voices.iter().enumerate() {
            self.ramps[v][param::GAIN as usize] = ParamRamp::new(p.gain);
            self.ramps[v][param::G as usize] = ParamRamp::new(p.g);
            self.ramps[v][param::K as usize] = ParamRamp::new(p.k);
            self.tile.gain[v] = p.gain;
            self.tile.g_coeff[v] = p.g;
            self.tile.k_damping[v] = p.k;
            self.tile.patch_id[v] = p.patch_id;
        }
    }

    /// Stereo pan of voice `v` (left weight, right weight).
    pub fn pan(v: usize) -> (f32, f32) {
        let p = v as f32 / (LANES - 1) as f32;
        (1.0 - p, p)
    }

    /// Render frames `[start, end)` of `input` into `left`/`right`.
    pub fn render(
        &mut self,
        input: &[f32],
        left: &mut [f32],
        right: &mut [f32],
        start: usize,
        end: usize,
    ) {
        let t = &mut self.tile;
        for i in start..end {
            let v0 = input[i];
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for v in 0..LANES {
                let gain = self.ramps[v][param::GAIN as usize].next_value();
                let g = self.ramps[v][param::G as usize].next_value();
                let k = self.ramps[v][param::K as usize].next_value();
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
                let (pl, pr) = Self::pan(v);
                l += y * pl;
                r += y * pr;
            }
            left[i] = l;
            right[i] = r;
        }
    }
}

impl ParamTable for SvfBank {
    fn ramp(&mut self, node_id: u16, param_id: u32) -> Option<&mut ParamRamp> {
        self.ramps
            .get_mut(usize::from(node_id))?
            .get_mut(usize::try_from(param_id).ok()?)
    }
}

/// Outcome of one realtime quantum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantumOutcome {
    /// Rendered and committed.
    Rendered {
        /// Sub-blocks rendered (1 + distinct event offsets after 0).
        segments: usize,
        /// Patch swap result.
        swap: SwapOutcome,
    },
    /// Events were malformed; the period was zero-silenced and committed.
    Silenced(EventError),
    /// The driver had no free period; nothing was rendered.
    NoPeriod,
}

/// The §32.3 `process_realtime_quantum` engine. Every buffer it touches is
/// allocated in [`SynthEngine::new`].
pub struct SynthEngine {
    bank: SvfBank,
    current_patch: Box<VoicePatchBank>,
    patches: MailboxReceiver<Box<VoicePatchBank>>,
    trash: RtRetirementSink<Box<VoicePatchBank>>,
    events: [RtParamEvent; MAX_EVENTS],
    left: [f32; QUANTUM_FRAMES],
    right: [f32; QUANTUM_FRAMES],
}

impl SynthEngine {
    /// Build an engine with an initial patch bank.
    pub fn new(
        initial: Box<VoicePatchBank>,
        patches: MailboxReceiver<Box<VoicePatchBank>>,
        trash: RtRetirementSink<Box<VoicePatchBank>>,
    ) -> Self {
        let mut bank = SvfBank::new(initial.voices[0]);
        bank.apply_patch(&initial);
        SynthEngine {
            bank,
            current_patch: initial,
            patches,
            trash,
            events: [RtParamEvent {
                sample_offset: 0,
                node_id: 0,
                param_id: 0,
                target: 0.0,
                ramp_samples: 0,
            }; MAX_EVENTS],
            left: [0.0; QUANTUM_FRAMES],
            right: [0.0; QUANTUM_FRAMES],
        }
    }

    /// The voice bank.
    pub fn bank(&self) -> &SvfBank {
        &self.bank
    }

    /// The patch currently loaded.
    pub fn current_patch(&self) -> &VoicePatchBank {
        &self.current_patch
    }

    /// One realtime quantum: swap in the latest patch (retiring the old one
    /// wait-free), decode and apply sample-accurate events, render, and
    /// discharge the output lease.
    pub fn process_quantum(
        &mut self,
        input: &[f32; QUANTUM_FRAMES],
        event_bytes: &[u8],
        port: &mut RtOutputPort,
    ) -> QuantumOutcome {
        let Some(mut lease) = port.lease() else {
            return QuantumOutcome::NoPeriod;
        };
        let swap = self
            .patches
            .try_swap_in_latest(&mut self.current_patch, &mut self.trash);
        if swap.swapped > 0 {
            self.bank.apply_patch(&self.current_patch);
        }
        let decoded = match decode_events(event_bytes, &mut self.events) {
            Ok(evs) => evs.len(),
            Err(e) => return silence(port, lease, e),
        };
        let (bank, left, right) = (&mut self.bank, &mut self.left, &mut self.right);
        let segments =
            match process_with_events(&self.events[..decoded], QUANTUM_FRAMES, bank, |b, s, e| {
                b.render(input, left, right, s, e)
            }) {
                Ok(n) => n,
                Err(e) => return silence(port, lease, e),
            };
        if write_stereo(&mut lease, &self.left, &self.right).is_err() {
            return silence(port, lease, EventError::NonFiniteTarget);
        }
        match port.commit(lease) {
            Ok(()) => QuantumOutcome::Rendered { segments, swap },
            Err(lease) => silence(port, lease, EventError::NonFiniteTarget),
        }
    }
}

fn write_stereo(
    lease: &mut RtBufferLease,
    left: &[f32],
    right: &[f32],
) -> Result<(), crate::lease::LeaseError> {
    for i in 0..lease.frames().min(left.len()) {
        lease.write_frame(i, &[left[i], right[i]])?;
    }
    Ok(())
}

fn silence(port: &mut RtOutputPort, lease: RtBufferLease, e: EventError) -> QuantumOutcome {
    // A lease from `port.lease()` always belongs to `port`, so this cannot
    // hand it back; if it did, dropping it is counted as a leak.
    let _ = port.emergency_silence_and_commit(lease);
    QuantumOutcome::Silenced(e)
}

/// PID gains and limits for a torque loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PidGains {
    /// Proportional gain.
    pub kp: f32,
    /// Integral gain.
    pub ki: f32,
    /// Derivative gain (on measurement, so setpoint steps do not kick).
    pub kd: f32,
    /// Period in seconds (0.001 for 1 kHz).
    pub dt: f32,
    /// Output torque clamp, `|u| <= torque_limit`.
    pub torque_limit: f32,
    /// Integrator clamp (anti-windup), `|i| <= integral_limit`.
    pub integral_limit: f32,
}

/// Controller faults; the caller must command safe torque (0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PidFault {
    /// Setpoint or measurement was NaN or infinite.
    NonFiniteInput,
    /// Gains are non-finite, negative, or `dt <= 0`.
    BadGains,
}

/// A 1 kHz PID torque controller with clamping and anti-windup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PidController {
    gains: PidGains,
    integral: f32,
    prev_measurement: Option<f32>,
}

impl PidController {
    /// A controller at rest.
    pub fn new(gains: PidGains) -> Result<Self, PidFault> {
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

    /// One control period: torque command for `setpoint` given `measurement`.
    pub fn step(&mut self, setpoint: f32, measurement: f32) -> Result<f32, PidFault> {
        if !setpoint.is_finite() || !measurement.is_finite() {
            return Err(PidFault::NonFiniteInput);
        }
        let g = self.gains;
        let error = setpoint - measurement;
        self.integral =
            (self.integral + g.ki * error * g.dt).clamp(-g.integral_limit, g.integral_limit);
        let derivative = match self.prev_measurement {
            Some(prev) => -(measurement - prev) / g.dt,
            None => 0.0,
        };
        self.prev_measurement = Some(measurement);
        let u = g.kp * error + self.integral + g.kd * derivative;
        Ok(u.clamp(-g.torque_limit, g.torque_limit))
    }

    /// Integrator state.
    pub fn integral(&self) -> f32 {
        self.integral
    }
}
