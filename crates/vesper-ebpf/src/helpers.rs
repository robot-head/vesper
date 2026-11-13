//! Kernel helper ids (`enum bpf_func_id`) this crate supports. Any other
//! helper id is a verifier rejection.

pub const MAP_LOOKUP_ELEM: i32 = 1;
pub const MAP_UPDATE_ELEM: i32 = 2;
pub const MAP_DELETE_ELEM: i32 = 3;
pub const KTIME_GET_NS: i32 = 5;
pub const GET_PRANDOM_U32: i32 = 7;
pub const GET_SMP_PROCESSOR_ID: i32 = 8;
pub const XDP_ADJUST_HEAD: i32 = 44;
pub const SK_LOOKUP_TCP: i32 = 84;
pub const SK_RELEASE: i32 = 86;
pub const RINGBUF_OUTPUT: i32 = 130;
pub const RINGBUF_RESERVE: i32 = 131;
pub const RINGBUF_SUBMIT: i32 = 132;
pub const RINGBUF_DISCARD: i32 = 133;

/// Every supported helper, with its kernel name.
pub const SUPPORTED: &[(i32, &str)] = &[
    (MAP_LOOKUP_ELEM, "bpf_map_lookup_elem"),
    (MAP_UPDATE_ELEM, "bpf_map_update_elem"),
    (MAP_DELETE_ELEM, "bpf_map_delete_elem"),
    (KTIME_GET_NS, "bpf_ktime_get_ns"),
    (GET_PRANDOM_U32, "bpf_get_prandom_u32"),
    (GET_SMP_PROCESSOR_ID, "bpf_get_smp_processor_id"),
    (XDP_ADJUST_HEAD, "bpf_xdp_adjust_head"),
    (SK_LOOKUP_TCP, "bpf_sk_lookup_tcp"),
    (SK_RELEASE, "bpf_sk_release"),
    (RINGBUF_OUTPUT, "bpf_ringbuf_output"),
    (RINGBUF_RESERVE, "bpf_ringbuf_reserve"),
    (RINGBUF_SUBMIT, "bpf_ringbuf_submit"),
    (RINGBUF_DISCARD, "bpf_ringbuf_discard"),
];

pub fn name(id: i32) -> Option<&'static str> {
    SUPPORTED.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)
}

/// `sizeof(struct bpf_sock_tuple.ipv4)`.
pub const SOCK_TUPLE_IPV4: u32 = 12;
/// `sizeof(struct bpf_sock_tuple.ipv6)`.
pub const SOCK_TUPLE_IPV6: u32 = 36;
/// `BPF_F_CURRENT_NETNS`.
pub const CURRENT_NETNS: u64 = u64::MAX;
