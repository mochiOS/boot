// Only this page runs at an identity address. No Rust call or stack spill is
// permitted before TTBR1 retains the real kernel. x0 is DomainBootInfo's GPA.
.section .text.arm64_entry, "ax"
.balign 16
.global domain_entry
domain_entry:
    msr daifset, #15
    mrs x1, CurrentEL
    cmp x1, #4
    b.ne .Lhalt
    mrs x20, sctlr_el1
    tst x20, #1
    b.ne .Lhalt
    tst x20, #(3 << 24)
    b.ne .Lhalt
    tst x0, #7
    b.ne .Lhalt
    ldr x1, [x0]
    ldr x2, ={boot_magic}
    cmp x1, x2
    b.ne .Lhalt
    ldr w1, [x0, #{version_offset}]
    cmp w1, #{boot_version}
    b.ne .Lhalt
    ldr w1, [x0, #{size_offset}]
    cmp w1, #{boot_size}
    b.ne .Lhalt
    ldr w1, [x0, #{backend_offset}]
    cmp w1, #{arm_backend}
    b.ne .Lhalt
    ldr x19, [x0, #{memory_offset}]
    cbz x19, .Lhalt
    tst x19, #4095
    b.ne .Lhalt
    mov x2, #0x40000000
    cmp x19, x2
    b.hi .Lhalt
    ldr x1, =__kernel_physical_end
    cmp x19, x1
    b.lo .Lhalt
    adds x1, x0, #{boot_size}
    b.cs .Lhalt
    cmp x1, x19
    b.hi .Lhalt
    mov x21, x0

    mrs x18, id_aa64mmfr0_el1
    ubfx x1, x18, #28, #4
    cmp x1, #1
    b.hi .Lhalt
    and x18, x18, #15
    cmp x18, #6
    b.hi .Lhalt
    mov x1, #5
    cmp x18, x1
    csel x18, x18, x1, ls
    lsl x18, x18, #32
    ldr x1, =0xb5103510
    orr x18, x18, x1

    // Two roots, upper L1/L2, lower L1/L2/L3 and 512 upper L3 pages:
    // at most 1 GiB of owned Domain RAM, all with 4-KiB W^X permissions.
    ldr x22, =__boot_tables_physical
    mov x1, x22
    ldr x2, =(519 * 4096)
.Lzero_tables:
    stp xzr, xzr, [x1], #16
    subs x2, x2, #16
    b.ne .Lzero_tables

    // Upper root[256] -> L1[0] -> L2. Lower root maps only the entry RX page.
    add x23, x22, #1, lsl #12
    add x1, x22, #2, lsl #12
    orr x2, x1, #3
    str x2, [x23, #(256 * 8)]
    add x3, x22, #3, lsl #12
    orr x2, x3, #3
    str x2, [x1]
    add x1, x22, #4, lsl #12
    orr x2, x1, #3
    str x2, [x22]
    add x4, x22, #5, lsl #12
    orr x2, x4, #3
    str x2, [x1]
    add x1, x22, #6, lsl #12
    orr x2, x1, #3
    str x2, [x4]
    ldr x2, =__head_physical_start
    ldr x4, =0x0040000000000783 // privileged RO/X, UXN
    orr x4, x4, x2
    lsr x2, x2, #12
    str x4, [x1, x2, lsl #3]

    // Every present L2 entry points to an exclusively owned L3 page.
    add x5, x22, #7, lsl #12
    add x1, x19, #0x1ff, lsl #12
    lsr x1, x1, #21
    mov x2, #0
.Lupper_l2:
    add x4, x5, x2, lsl #12
    orr x4, x4, #3
    str x4, [x3, x2, lsl #3]
    add x2, x2, #1
    cmp x2, x1
    b.lo .Lupper_l2

    ldr x6, =__text_physical_start
    ldr x7, =__text_physical_end
    ldr x8, =__rodata_physical_start
    ldr x9, =__rodata_physical_end
    ldr x10, =__head_physical_start
    ldr x11, =__head_physical_end
    ldr x12, =0x0060000000000703 // privileged RW, PXN/UXN
    ldr x13, =0x0040000000000783 // privileged RO/X, UXN
    ldr x14, =0x0060000000000783 // privileged RO, PXN/UXN
    mov x1, #0
.Lupper_pages:
    mov x2, x12
    cmp x1, x6
    b.lo .Lread_only_data
    cmp x1, x7
    b.lo .Ltext_page
.Lread_only_data:
    cmp x1, x8
    b.lo .Lhead_alias
    cmp x1, x9
    b.lo .Lread_only_page
.Lhead_alias:
    cmp x1, x10
    b.lo .Lwrite_page
    cmp x1, x11
    b.hs .Lwrite_page
.Lread_only_page:
    mov x2, x14
    b .Lwrite_page
.Ltext_page:
    mov x2, x13
.Lwrite_page:
    orr x2, x2, x1
    str x2, [x5], #8
    add x1, x1, #1, lsl #12
    cmp x1, x19
    b.lo .Lupper_pages

    // Preload everything used immediately after enabling translation.
    ldr x1, =KERNEL_MAP_OFFSET
    add x0, x21, x1
    ldr x2, =__boot_stack_physical_end
    add x2, x2, x1
    ldr x16, =__mnu_arm64_system_entry
    mov x3, #(3 << 20)
    msr cpacr_el1, x3
    isb
    ldr x3, =0xbb4400ff
    ldr x4, =0x8101d // M/C/I, WXN, SA/SA0
    orr x20, x20, x4
    ldr x4, =((1 << 16) | (1 << 18) | (1 << 26))
    bic x20, x20, x4
    dsb sy
    msr mair_el1, x3
    msr tcr_el1, x18
    isb
    msr ttbr0_el1, x22
    msr ttbr1_el1, x23
    tlbi vmalle1is
    dsb ish
    isb
    msr sctlr_el1, x20
    isb
    mov sp, x2
    br x16
.Lhalt:
    wfi
    b .Lhalt
    .ltorg

.section .bss.arm64_tables, "aw", %nobits
.balign 4096
.global __arm64_boot_tables
__arm64_boot_tables:
    .skip (519 * 4096)
.section .bss.arm64_stack, "aw", %nobits
.balign 4096
    .skip 65536
.global __arm64_boot_stack_end
__arm64_boot_stack_end:
