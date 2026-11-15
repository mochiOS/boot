/* The ELF entry executes at its GPA with MMU off. All retained Rust code,
 * tables, stacks and boot metadata live in the privileged TTBR1 linear map. */
OUTPUT_ARCH(aarch64)
ENTRY(domain_entry)

KERNEL_MAP_OFFSET = 0xffff800000000000;
PHDRS {
    head PT_LOAD FLAGS(5);
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
    stack PT_GNU_STACK FLAGS(6);
}
SECTIONS {
    . = 0x10000;
    .head : {
        KEEP(*(.text.arm64_entry))
    } :head
    __head_physical_start = ADDR(.head);
    __head_physical_end = ALIGN(ADDR(.head) + SIZEOF(.head), 4096);
    ASSERT(SIZEOF(.head) <= 4096, "ARM identity entry must fit one RX page")

    . = KERNEL_MAP_OFFSET + 0x200000;
    __kernel_start = .;
    .text : AT(ADDR(.text) - KERNEL_MAP_OFFSET) {
        __text_start = .;
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    } :text
    .rodata : AT(ADDR(.rodata) - KERNEL_MAP_OFFSET) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    } :rodata
    .data : AT(ADDR(.data) - KERNEL_MAP_OFFSET) {
        *(.data .data.* .got .got.*)
        . = ALIGN(4096);
    } :data
    .bss (NOLOAD) : AT(ADDR(.bss) - KERNEL_MAP_OFFSET) {
        KEEP(*(.bss.arm64_tables))
        KEEP(*(.bss.arm64_stack))
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
    } :data
    __kernel_end = .;
    __kernel_physical_end = __kernel_end - KERNEL_MAP_OFFSET;
    __text_physical_start = __text_start - KERNEL_MAP_OFFSET;
    __text_physical_end = __text_end - KERNEL_MAP_OFFSET;
    __rodata_physical_start = __rodata_start - KERNEL_MAP_OFFSET;
    __rodata_physical_end = __rodata_end - KERNEL_MAP_OFFSET;
    __boot_tables_physical = __arm64_boot_tables - KERNEL_MAP_OFFSET;
    __boot_stack_physical_end = __arm64_boot_stack_end - KERNEL_MAP_OFFSET;
    ASSERT(__kernel_physical_end <= 0x40000000, "ARM bootstrap exceeds assigned RAM limit")
    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr .comment .note .note.*)
    }
}
