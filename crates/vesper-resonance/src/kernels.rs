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
use crate::simd::{LaneOut, PanLanes, SimdUnavailable, SvfLanes, svf_tick};
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
///
/// Rendering advances the ramps (scalar), then all 16 filter lanes with the
/// selected [`SvfLanes`] path, then sums the panned voices in voice order.
/// Every path is bit-identical to [`SvfBank::render_reference`].
#[derive(Clone, Debug)]
pub struct SvfBank {
    /// Filter state.
    pub tile: SvfVoiceTile,
    ramps: [[ParamRamp; 3]; LANES],
    lanes: SvfLanes,
    pan: PanLanes,
    scratch: LaneOut,
}

impl SvfBank {
    /// A silent bank with every voice at `patch`, using the widest SIMD
    /// path this CPU supports.
    pub fn new(patch: VoicePatch) -> Self {
        Self::build(patch, SvfLanes::best())
    }

    /// Like [`SvfBank::new`] with an explicit lane path; a path this CPU
    /// cannot run is a typed error.
    pub fn with_lanes(patch: VoicePatch, lanes: SvfLanes) -> Result<Self, SimdUnavailable> {
        Ok(Self::build(patch, lanes.checked()?))
    }

    /// The lane path in use.
    pub fn lanes(&self) -> SvfLanes {
        self.lanes
    }

    /// Switch the lane path (state is kept).
    pub fn set_lanes(&mut self, lanes: SvfLanes) -> Result<(), SimdUnavailable> {
        self.lanes = lanes.checked()?;
        Ok(())
    }

    fn build(patch: VoicePatch, lanes: SvfLanes) -> Self {
        let mut pan = PanLanes {
            left: [0.0; LANES],
            right: [0.0; LANES],
        };
        for v in 0..LANES {
            (pan.left[v], pan.right[v]) = Self::pan(v);
        }
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
            lanes,
            pan,
            scratch: LaneOut::ZERO,
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

    /// Render frames `[start, end)` of `input` into `left`/`right` with the
    /// selected lane path.
    pub fn render(
        &mut self,
        input: &[f32],
        left: &mut [f32],
        right: &mut [f32],
        start: usize,
        end: usize,
    ) {
        for i in start..end {
            for v in 0..LANES {
                self.tile.gain[v] = self.ramps[v][param::GAIN as usize].next_value();
                self.tile.g_coeff[v] = self.ramps[v][param::G as usize].next_value();
                self.tile.k_damping[v] = self.ramps[v][param::K as usize].next_value();
            }
            svf_tick(
                self.lanes,
                &mut self.tile,
                &self.pan,
                input[i],
                &mut self.scratch,
            );
            (left[i], right[i]) = self.scratch.sum();
        }
    }

    /// The original fused scalar loop (wave five), kept as the reference
    /// every lane path is tested against bit for bit.
    pub fn render_reference(
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

    /// Switch the voice bank's SIMD lane path.
    pub fn set_lanes(&mut self, lanes: SvfLanes) -> Result<(), SimdUnavailable> {
        self.bank.set_lanes(lanes)
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

/// The voice bank as a node of a [`crate::pdc::CompiledGraph`]: input 0 (or
/// silence when the node has no inputs) excites every voice, and the output
/// is the left plus right mix. Scratch buffers are allocated in
/// [`SvfNode::new`], so processing a block does not allocate.
#[derive(Clone, Debug)]
pub struct SvfNode {
    bank: SvfBank,
    silence: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl SvfNode {
    /// A node for blocks of `block` samples.
    pub fn new(bank: SvfBank, block: usize) -> Self {
        SvfNode {
            bank,
            silence: vec![0.0; block],
            left: vec![0.0; block],
            right: vec![0.0; block],
        }
    }

    /// The voice bank.
    pub fn bank(&self) -> &SvfBank {
        &self.bank
    }
}

impl crate::pdc::DspProcessor for SvfNode {
    fn process(&mut self, inputs: &crate::pdc::Inputs<'_>, output: &mut [f32]) {
        let n = output.len().min(self.left.len());
        let input = if inputs.is_empty() {
            &self.silence[..n]
        } else {
            let x = inputs.get(0);
            &x[..n.min(x.len())]
        };
        let n = input.len();
        self.bank
            .render(input, &mut self.left, &mut self.right, 0, n);
        for (i, y) in output[..n].iter_mut().enumerate() {
            *y = self.left[i] + self.right[i];
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
    /// A finite input overflowed inside the controller; state is unchanged.
    NonFiniteIntermediate,
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
        // Compute every intermediate first; commit state only if all are
        // finite, so an overflow (e.g. `f32::MAX - -f32::MAX`) is a fault and
        // never poisons the integrator or yields an unclamped NaN torque.
        let error = setpoint - measurement;
        let integral =
            (self.integral + g.ki * error * g.dt).clamp(-g.integral_limit, g.integral_limit);
        let derivative = match self.prev_measurement {
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
        self.integral = integral;
        self.prev_measurement = Some(measurement);
        Ok(u.clamp(-g.torque_limit, g.torque_limit))
    }

    /// Integrator state.
    pub fn integral(&self) -> f32 {
        self.integral
    }
}
