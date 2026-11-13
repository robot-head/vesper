//! Kernel helper ids (`enum bpf_func_id`) this crate supports. Any other
//! helper id is a verifier rejection.

use crate::program::ProgramType;

pub const MAP_LOOKUP_ELEM: i32 = 1;
pub const MAP_UPDATE_ELEM: i32 = 2;
pub const MAP_DELETE_ELEM: i32 = 3;
pub const KTIME_GET_NS: i32 = 5;
pub const GET_PRANDOM_U32: i32 = 7;
pub const GET_SMP_PROCESSOR_ID: i32 = 8;
pub const TAIL_CALL: i32 = 12;
pub const GET_CURRENT_PID_TGID: i32 = 14;
pub const GET_CURRENT_UID_GID: i32 = 15;
pub const GET_CURRENT_COMM: i32 = 16;
pub const XDP_ADJUST_HEAD: i32 = 44;
pub const SK_LOOKUP_TCP: i32 = 84;
pub const SK_RELEASE: i32 = 86;
pub const RINGBUF_OUTPUT: i32 = 130;
pub const RINGBUF_RESERVE: i32 = 131;
pub const RINGBUF_SUBMIT: i32 = 132;
pub const RINGBUF_DISCARD: i32 = 133;
pub const LOOP: i32 = 181;

/// Every supported helper, with its kernel name.
pub const SUPPORTED: &[(i32, &str)] = &[
    (MAP_LOOKUP_ELEM, "bpf_map_lookup_elem"),
    (MAP_UPDATE_ELEM, "bpf_map_update_elem"),
    (MAP_DELETE_ELEM, "bpf_map_delete_elem"),
    (KTIME_GET_NS, "bpf_ktime_get_ns"),
    (GET_PRANDOM_U32, "bpf_get_prandom_u32"),
    (GET_SMP_PROCESSOR_ID, "bpf_get_smp_processor_id"),
    (TAIL_CALL, "bpf_tail_call"),
    (GET_CURRENT_PID_TGID, "bpf_get_current_pid_tgid"),
    (GET_CURRENT_UID_GID, "bpf_get_current_uid_gid"),
    (GET_CURRENT_COMM, "bpf_get_current_comm"),
    (XDP_ADJUST_HEAD, "bpf_xdp_adjust_head"),
    (SK_LOOKUP_TCP, "bpf_sk_lookup_tcp"),
    (SK_RELEASE, "bpf_sk_release"),
    (RINGBUF_OUTPUT, "bpf_ringbuf_output"),
    (RINGBUF_RESERVE, "bpf_ringbuf_reserve"),
    (RINGBUF_SUBMIT, "bpf_ringbuf_submit"),
    (RINGBUF_DISCARD, "bpf_ringbuf_discard"),
    (LOOP, "bpf_loop"),
];

/// Whether `prog_type` may call helper `id` (the kernel's per-type
/// `get_func_proto`, restricted to [`SUPPORTED`]).
pub fn allowed(prog_type: ProgramType, id: i32) -> bool {
    let common = matches!(
        id,
        MAP_LOOKUP_ELEM
            | MAP_UPDATE_ELEM
            | MAP_DELETE_ELEM
            | KTIME_GET_NS
            | GET_PRANDOM_U32
            | GET_SMP_PROCESSOR_ID
            | TAIL_CALL
            | RINGBUF_OUTPUT
            | RINGBUF_RESERVE
            | RINGBUF_SUBMIT
            | RINGBUF_DISCARD
            | LOOP
    );
    common
        || match prog_type {
            ProgramType::Xdp => matches!(id, XDP_ADJUST_HEAD | SK_LOOKUP_TCP | SK_RELEASE),
            ProgramType::SchedCls => matches!(id, SK_LOOKUP_TCP | SK_RELEASE),
            ProgramType::Tracepoint | ProgramType::Kprobe => matches!(
                id,
                GET_CURRENT_PID_TGID | GET_CURRENT_UID_GID | GET_CURRENT_COMM
            ),
        }
}

/// kfuncs the verifier models (generic kfuncs registered for every
/// program type).
pub const KFUNCS: &[&str] = &["bpf_preempt_disable", "bpf_preempt_enable"];

/// `license_is_gpl_compatible`.
pub fn gpl_compatible(license: &str) -> bool {
    matches!(
        license,
        "GPL"
            | "GPL v2"
            | "GPL and additional rights"
            | "Dual BSD/GPL"
            | "Dual MIT/GPL"
            | "Dual MPL/GPL"
    )
}

pub fn name(id: i32) -> Option<&'static str> {
    SUPPORTED.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)
}

/// `sizeof(struct bpf_sock_tuple.ipv4)`.
pub const SOCK_TUPLE_IPV4: u32 = 12;
/// `sizeof(struct bpf_sock_tuple.ipv6)`.
pub const SOCK_TUPLE_IPV6: u32 = 36;
/// `BPF_F_CURRENT_NETNS`.
pub const CURRENT_NETNS: u64 = u64::MAX;
