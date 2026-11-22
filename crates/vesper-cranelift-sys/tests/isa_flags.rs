//! ISA-specific flags in `vcl_module_new` (SH4 lane 14, #1288): a flag the
//! shared settings do not know is set on the ISA builder, so a riscv64
//! object module can take `has_zca` (the freestanding runtime's ISA). A
//! name neither knows is still an error.
use vesper_cranelift_sys::*;

/// The `e_flags` of the object an empty riscv64 module with `flags` emits,
/// or the constructor's error.
fn riscv_e_flags(flags: &str) -> Result<u32, String> {
    let (t, n) = ("riscv64", "isa-flags");
    // SAFETY: every pointer is a live buffer of the given length; each
    // handle is consumed once.
    unsafe {
        let m = vcl_module_new(
            t.as_ptr(),
            t.len(),
            flags.as_ptr(),
            flags.len(),
            n.as_ptr(),
            n.len(),
        );
        if m.is_null() {
            let mut e = vec![0u8; 256];
            let len = vcl_new_error(e.as_mut_ptr(), e.len()) as usize;
            e.truncate(len.min(256));
            return Err(String::from_utf8_lossy(&e).into_owned());
        }
        let o = vcl_module_finish(m);
        assert!(!o.is_null(), "finish failed");
        let mut bytes = vec![0u8; vcl_object_len(o) as usize];
        vcl_object_read(o, 0, bytes.as_mut_ptr(), bytes.len());
        vcl_object_free(o);
        Ok(u32::from_le_bytes(bytes[48..52].try_into().unwrap()))
    }
}

/// `EF_RISCV_RVC`: the object uses compressed instructions.
const EF_RISCV_RVC: u32 = 1;

#[test]
fn riscv_modules_take_isa_flags() {
    // cranelift-object sets RVC when both are on.
    let with = riscv_e_flags("opt_level=speed;has_zca=true;has_zcd=true").unwrap();
    let without = riscv_e_flags("opt_level=speed;has_zca=false;has_zcd=false").unwrap();
    assert_eq!(with & EF_RISCV_RVC, EF_RISCV_RVC, "e_flags {with:#x}");
    assert_eq!(without & EF_RISCV_RVC, 0, "e_flags {without:#x}");
}

#[test]
fn unknown_flags_are_still_errors() {
    let e = riscv_e_flags("no_such_flag=true").unwrap_err();
    assert!(e.starts_with("flag no_such_flag:"), "{e}");
}
