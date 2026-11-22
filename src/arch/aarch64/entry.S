// Only this page runs at an identity address. No Rust call or stack spill is
// permitted before TTBR1 retains the real kernel. A System Domain receives
// DomainBootInfo in x0; the firmware Image owns its header-declared RAM span.
.section .text.arm64_entry, "ax"
.balign 16
.global domain_entry
domain_entry:
.if {native_image}
    // Standard little-endian AArch64 Image header. The loader reserves
    // image_size bytes starting at this entry, including BSS and free frames.
    b .Lentry
    .long 0
    .quad 0x80000, __native_image_size, 8
    .quad 0, 0, 0
    .ascii "ARM\144"
    .long 0
.endif
.Lentry:
    msr daifset, #15
    mrs x1, CurrentEL
    cmp x1, #4
    b.ne .Lhalt
    mrs x20, sctlr_el1
    tst x20, #1
    b.ne .Lhalt
    tst x20, #(3 << 24)
    b.ne .Lhalt
    adr x26, domain_entry
.if {native_image}
    tst x26, #4095
    b.ne .Lhalt
    msr spsel, #1
    mov x24, x26
    ldr x19, =__native_image_size
    adds x1, x24, x19
    b.cs .Lhalt
    cbz x0, .Lhalt
    tst x0, #7
    b.ne .Lhalt
    mov x21, x0
    mov x27, x0
.else
    mov x24, #0
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
.endif

    mrs x18, id_aa64mmfr0_el1
    ubfx x1, x18, #28, #4
    cmp x1, #1
    b.hi .Lhalt
    and x18, x18, #15
    cmp x18, #6
    b.hi .Lhalt
.if {native_image}
    adr x2, .Lphysical_bits
    ldrb w2, [x2, x18]
    add x1, x24, x19
    sub x1, x1, #1
    lsr x1, x1, x2
    cbnz x1, .Lhalt

    // x0 points to the firmware DTB. Preserve it while clearing the linked
    // Image's BSS. A DTB in spare Image RAM is copied before allocator setup.
    adds x1, x21, #7
    b.cs .Lhalt
    lsr x1, x1, x2
    cbnz x1, .Lhalt
    ldr w1, [x21]
    ldr w3, =0xedfe0dd0
    cmp w1, w3
    b.ne .Lhalt
    ldr w1, [x21, #4]
    rev w25, w1
    cmp w25, #40
    b.lo .Lhalt
    cmp w25, #512, lsl #12
    b.hi .Lhalt
    adds x1, x21, x25
    b.cs .Lhalt
    sub x3, x1, #1
    lsr x3, x3, x2
    cbnz x3, .Lhalt
    cmp x1, x24
    b.ls .Lfdt_disjoint
    ldr x1, =__kernel_physical_end
    add x1, x24, x1
    cmp x21, x1
    b.lo .Lhalt
.Lfdt_disjoint:

    // Firmware loads the Image bytes, not the ELF's zero-filled segments.
    ldr x1, =__bss_physical_start
    ldr x2, =__kernel_physical_end
    add x1, x1, x24
    add x2, x2, x24
.Lzero_bss:
    stp xzr, xzr, [x1], #16
    cmp x1, x2
    b.lo .Lzero_bss

    ldr x1, =__arm64_boot_dtb
    ldr x3, =KERNEL_MAP_OFFSET
    sub x1, x1, x3
    add x1, x24, x1
    mov x3, x25
.Lcopy_dtb:
    ldrb w2, [x21], #1
    strb w2, [x1], #1
    subs x3, x3, #1
    b.ne .Lcopy_dtb
.endif
    mov x1, #5
    cmp x18, x1
    csel x18, x18, x1, ls
    lsl x18, x18, #32
    ldr x1, =0xb5103510
    orr x18, x18, x1

    // Two roots, upper L1/L2, lower L1/L2/L3 and 512 upper L3 pages:
    // at most 1 GiB of owned Domain RAM, all with 4-KiB W^X permissions.
    ldr x22, =__boot_tables_physical
    add x22, x22, x24
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
    ubfx x4, x26, #39, #9
    str x2, [x22, x4, lsl #3]
    add x4, x22, #5, lsl #12
    orr x2, x4, #3
    ubfx x5, x26, #30, #9
    str x2, [x1, x5, lsl #3]
    add x1, x22, #6, lsl #12
    orr x2, x1, #3
    ubfx x5, x26, #21, #9
    str x2, [x4, x5, lsl #3]
    ldr x4, =0x0040000000000783 // privileged RO/X, UXN
    orr x4, x4, x26
    ubfx x2, x26, #12, #9
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
    add x15, x24, x1
    orr x2, x2, x15
    str x2, [x5], #8
    add x1, x1, #1, lsl #12
    cmp x1, x19
    b.lo .Lupper_pages

    // Preload everything used immediately after enabling translation.
    ldr x1, =KERNEL_MAP_OFFSET
    ldr x2, =__boot_stack_physical_end
    add x2, x2, x1
.if {native_image}
    mov x0, x24
    mov x1, x19
    ldr x16, =__mnu_arm64_native_entry
.else
    add x0, x21, x1
    ldr x16, =__mnu_arm64_system_entry
.endif
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
.if {native_image}
    mov x2, x25
    mov x3, x27
.endif
    br x16
.Lhalt:
    wfi
    b .Lhalt
.if {native_image}
.Lphysical_bits:
    .byte 32, 36, 40, 42, 44, 48, 48
    .balign 4
.endif
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

.if {native_image}
.section .bss.arm64_dtb, "aw", %nobits
.balign 8
.global __arm64_boot_dtb
__arm64_boot_dtb:
    .skip 2097152
.endif
