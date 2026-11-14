//! The GNU ld / ld.lld linker script equivalent to [`super::link::link`].

use super::{Board, FreestandingTarget};

/// Linker script for `target` on `board` with a `stack_bytes` stack. Every
/// symbol the stub and runtime import (`__stack_top`, `__bss_start`,
/// `__bss_end`) is defined here; `.text.boot` (the stub) is first.
pub fn linker_script(target: FreestandingTarget, board: &Board, stack_bytes: u32) -> String {
    let arch = match target {
        FreestandingTarget::Riscv64Imac => "riscv",
        FreestandingTarget::Aarch64 => "aarch64",
    };
    let stack = u64::from(stack_bytes).div_ceil(16) * 16;
    format!(
        "/* {schema}: {triple} on {board} */
OUTPUT_ARCH({arch})
ENTRY(_start)

SECTIONS
{{
  . = {base:#x};
  __image_start = .;
  .text : ALIGN(16)
  {{
    KEEP(*(.text.boot))
    *(.text .text.*)
  }}
  .rodata : ALIGN(16)
  {{
    *(.rodata .rodata.* .srodata .srodata.*)
  }}
  . = ALIGN(0x1000);
  .data : ALIGN(16)
  {{
    *(.data .data.* .sdata .sdata.*)
  }}
  .bss (NOLOAD) : ALIGN(16)
  {{
    __bss_start = .;
    *(.bss .bss.* .sbss .sbss.* COMMON)
    . = ALIGN(8);
    __bss_end = .;
  }}
  .stack (NOLOAD) : ALIGN(16)
  {{
    __stack_bottom = .;
    . += {stack:#x};
    __stack_top = .;
  }}
  __image_end = .;
  /DISCARD/ : {{ *(.eh_frame*) *(.comment) *(.note*) *(.riscv.attributes) }}
}}

ASSERT(__stack_top <= {ram_end:#x}, \"image and stack exceed RAM ({ram:#x} + {ram_len:#x})\")
ASSERT(_start == {base:#x}, \"the entry stub must be first\")
",
        schema = super::FREESTANDING_SCHEMA,
        triple = target.triple(),
        board = board.name,
        ram = board.ram_base,
        ram_len = board.ram_bytes,
        ram_end = board.ram_base + board.ram_bytes,
        base = board.image_base,
    )
}
