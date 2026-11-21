//! Native System Domain frontend. Only the low assembly page executes before
//! stage-1 translation; the retained kernel owns all Rust state in TTBR1.
use super::*;

const KERNEL_MAP_OFFSET: u64 = 0xffff_8000_0000_0000;
const MAX_BOOT_RAM: u64 = 1 << 30;

core::arch::global_asm!(
    include_str!("entry.S"),
    native_image = const 0,
    boot_magic = const mnu_abi::hypervisor::DOMAIN_BOOT_MAGIC,
    boot_version = const mnu_abi::hypervisor::DOMAIN_BOOT_VERSION,
    boot_size = const size_of::<DomainBootInfo>(),
    version_offset = const core::mem::offset_of!(DomainBootInfo, abi_version),
    size_offset = const core::mem::offset_of!(DomainBootInfo, struct_size),
    backend_offset = const core::mem::offset_of!(DomainBootInfo, hypervisor_backend),
    memory_offset = const core::mem::offset_of!(DomainBootInfo, memory_size),
    arm_backend = const mnu_abi::hypervisor::HYPERVISOR_BACKEND_ARM_EL2,
);

#[unsafe(no_mangle)]
unsafe extern "C" fn __mnu_arm64_system_entry(info: *const DomainBootInfo) -> ! {
    // SAFETY: The low entry bounded this complete pointer inside owned RAM
    // and installed its privileged, retained upper linear mapping.
    let Some(info) = (unsafe { info.as_ref() }) else {
        halt()
    };
    let Some(kernel_end) =
        (core::ptr::addr_of!(__kernel_end) as u64).checked_sub(KERNEL_MAP_OFFSET)
    else {
        halt()
    };
    if info.validate().is_err()
        || info.domain_role != DOMAIN_ROLE_SYSTEM
        || info.memory_size > MAX_BOOT_RAM
        || info.boot_module_start < kernel_end
        || info.boot_module_size == 0
        || !mnu::hypervisor_guest::configure(info)
    {
        halt()
    }
    // SAFETY: This is the only admitted boot CPU, before any task exists. The
    // ELF loader zeroed BSS; these statics stay in the retained upper mapping.
    unsafe {
        MEMORY_MAP[0].len = kernel_end;
        MEMORY_MAP[1].start = kernel_end;
        MEMORY_MAP[1].len = info.boot_module_start - kernel_end;
        KERNEL_BOOT_INFO.feature_flags = BOOT_FEATURE_INITFS | BOOT_FEATURE_HYPERVISOR_DOMAIN;
        KERNEL_BOOT_INFO.physical_memory_offset = KERNEL_MAP_OFFSET;
        KERNEL_BOOT_INFO.memory_map_addr = core::ptr::addr_of!(MEMORY_MAP) as u64;
        KERNEL_BOOT_INFO.memory_map_len = 2;
        KERNEL_BOOT_INFO.memory_map_entry_size = size_of::<MemoryRegion>() as u32;
        KERNEL_BOOT_INFO.kernel_heap_addr = core::ptr::addr_of!(KERNEL_ALLOCATOR) as u64;
        // mnu consumes the retained module as bytes, not an identity GPA.
        KERNEL_BOOT_INFO.initfs_addr = KERNEL_MAP_OFFSET + info.boot_module_start;
        KERNEL_BOOT_INFO.initfs_size = info.boot_module_size;
        let hardware = mnu::percpu::hardware_id();
        KERNEL_BOOT_INFO.bsp_hardware_id = hardware;
        KERNEL_BOOT_INFO.cpu_hardware_ids[0] = hardware;
        KERNEL_BOOT_INFO.cpu_id_count = 1;
        KERNEL_BOOT_INFO.cpu_enabled = 1;
        KERNEL_BOOT_INFO.cpu_total = 1;
        KERNEL_BOOT_INFO.entropy_seed = info.entropy_seed;
        KERNEL_BOOT_INFO.entropy_seed_valid = info.entropy_seed_valid;
        if info.entropy_seed_valid != 0 {
            KERNEL_BOOT_INFO.feature_flags |= mnu::BOOT_FEATURE_ENTROPY;
        }
        if info.feature_flags & DOMAIN_FEATURE_VIRTUAL_GIC != 0 {
            KERNEL_BOOT_INFO.feature_flags |= BOOT_FEATURE_ARM64_INTERRUPTS;
            KERNEL_BOOT_INFO.arm64_interrupts = info.arm64_interrupts;
        }
        if info.feature_flags & DOMAIN_FEATURE_FRAMEBUFFER != 0 {
            // This is only discovery metadata. mDriver owns display I/O;
            // firmware MMIO is not exposed through the kernel's RAM map.
            KERNEL_BOOT_INFO.feature_flags |= BOOT_FEATURE_FRAMEBUFFER;
            KERNEL_BOOT_INFO.framebuffer_addr = info.framebuffer_addr;
            KERNEL_BOOT_INFO.framebuffer_size = info.framebuffer_size;
            KERNEL_BOOT_INFO.screen_width = u64::from(info.framebuffer_width);
            KERNEL_BOOT_INFO.screen_height = u64::from(info.framebuffer_height);
            KERNEL_BOOT_INFO.stride = u64::from(info.framebuffer_stride);
        }
    }
    if mnu::hypervisor_guest::invoke(HypercallNumber::Ready, 0, 0, 0) != HYPERCALL_SUCCESS
        || mnu::platform::install(&mdriver::PLATFORM_OPS).is_err()
    {
        halt()
    }
    // SAFETY: All boot metadata is now kernel-owned and permanently mapped.
    mnu::kernel_entry(unsafe { &*core::ptr::addr_of!(KERNEL_BOOT_INFO) })
}
