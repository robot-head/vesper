/*
 * Vesper Silicon AIA probe: a bare-metal M-mode interpreter for QEMU `virt`
 * (`-bios none`). It runs the op script loaded at SCRIPT (by
 * `-device loader,file=...,addr=0x80100000`), buffers one 64-bit result per
 * read op, then prints the results as hex lines on the NS16550 UART and
 * powers off through the SiFive test device. Interrupts stay masked
 * (mstatus.MIE = 0) throughout; the probe only observes pending state.
 *
 * Each op is three little-endian u64 words: kind, a, b.
 *   0 end              1 sw  b -> [a]      2 lw  [a] (result)
 *   3 sd  b -> [a]     4 sb  b -> [a]      5 ld  [a] (result)
 *   6 miselect=a; mireg=b                  7 miselect=a; mireg (result)
 *   8 mtopei (result)  9 csrrw mtopei, 0 (claim, result)
 *  10 siselect=a; sireg=b                 11 siselect=a; sireg (result)
 *  12 stopei (result) 13 csrrw stopei, 0 (claim, result)
 *  14 mip & 0xa00 (result)
 *
 * Build: clang --target=riscv64-unknown-elf -march=rv64imac -mabi=lp64
 *        -nostdlib -c aia-probe.S -o aia-probe.o
 *        ld.lld -s -Ttext=0x80000000 -e _start --build-id=none aia-probe.o \
 *        -o aia-probe.elf
 * Harts other than hart 0 park in a wfi loop.
 */
    .equ SCRIPT, 0x80100000
    .equ OUTBUF, 0x80200000
    .equ UART,   0x10000000
    .equ TEST,   0x00100000

    .section .text
    .globl _start
_start:
    csrw mie, zero
    csrci mstatus, 8
    csrr t0, mhartid
    bnez t0, park
    li s0, SCRIPT
    li s1, OUTBUF
    mv s2, s1
next:
    ld t0, 0(s0)
    ld a0, 8(s0)
    ld a1, 16(s0)
    addi s0, s0, 24
    beqz t0, done
    li t1, 1
    beq t0, t1, op_sw
    li t1, 2
    beq t0, t1, op_lw
    li t1, 3
    beq t0, t1, op_sd
    li t1, 4
    beq t0, t1, op_sb
    li t1, 5
    beq t0, t1, op_ld
    li t1, 6
    beq t0, t1, op_miw
    li t1, 7
    beq t0, t1, op_mir
    li t1, 8
    beq t0, t1, op_mtopei
    li t1, 9
    beq t0, t1, op_mclaim
    li t1, 10
    beq t0, t1, op_siw
    li t1, 11
    beq t0, t1, op_sir
    li t1, 12
    beq t0, t1, op_stopei
    li t1, 13
    beq t0, t1, op_sclaim
    li t1, 14
    beq t0, t1, op_mip
    li a2, 0xbad0bad0
    j emit
op_sw:
    sw a1, 0(a0)
    fence iorw, iorw
    j next
op_lw:
    fence iorw, iorw
    lwu a2, 0(a0)
    j emit
op_sd:
    sd a1, 0(a0)
    fence iorw, iorw
    j next
op_sb:
    sb a1, 0(a0)
    fence iorw, iorw
    j next
op_ld:
    fence iorw, iorw
    ld a2, 0(a0)
    j emit
op_miw:
    csrw 0x350, a0
    csrw 0x351, a1
    j next
op_mir:
    csrw 0x350, a0
    csrr a2, 0x351
    j emit
op_mtopei:
    csrr a2, 0x35c
    j emit
op_mclaim:
    csrrw a2, 0x35c, zero
    j emit
op_siw:
    csrw 0x150, a0
    csrw 0x151, a1
    j next
op_sir:
    csrw 0x150, a0
    csrr a2, 0x151
    j emit
op_stopei:
    csrr a2, 0x15c
    j emit
op_sclaim:
    csrrw a2, 0x15c, zero
    j emit
op_mip:
    csrr a2, mip
    li t1, 0xa00
    and a2, a2, t1
emit:
    sd a2, 0(s2)
    addi s2, s2, 8
    j next

done:
    /* Print every buffered result as 16 hex digits and a newline. */
    li s3, UART
1:  bgeu s1, s2, off
    ld a2, 0(s1)
    addi s1, s1, 8
    li t2, 60
2:  srl t3, a2, t2
    andi t3, t3, 15
    li t4, 10
    blt t3, t4, 3f
    addi t3, t3, 'a' - 10
    j 4f
3:  addi t3, t3, '0'
4:  call putc
    addi t2, t2, -4
    bgez t2, 2b
    li t3, '\n'
    call putc
    j 1b
off:
    li t3, 'E'
    call putc
    li t3, '\n'
    call putc
    li t0, TEST
    li t1, 0x5555
    sw t1, 0(t0)
5:  wfi
    j 5b

putc:
    lbu t5, 5(s3)
    andi t5, t5, 0x20
    beqz t5, putc
    sb t3, 0(s3)
    ret
park:
    wfi
    j park
