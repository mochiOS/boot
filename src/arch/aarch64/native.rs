//! Direct AArch64 Image entry for firmware that hands off at EL1.
//!
//! This bootstrap uses the loader-owned Image span to bring up mnu's VM.
//! A board/frontend must supply interrupt and device ownership before the
//! normal scheduler and userspace can start.

#![no_std]
#![no_main]

#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
compile_error!("mochios-native requires aarch64-unknown-none");

use core::mem::size_of;
use mboot::fdt::{memory::MemoryMap, Fdt};
use mnu::mem::allocator::HardenedKernelHeap;
use mnu::{BootInfo, MemoryRegion, MemoryType, BOOT_FEATURE_ENTROPY};
use mnu_abi::hypervisor::{
    DomainBootInfo, DOMAIN_BOOT_MAGIC, DOMAIN_BOOT_VERSION, HYPERVISOR_BACKEND_ARM_EL2,
};

core::arch::global_asm!(
    include_str!("entry.S"),
    native_image = const 1,
    boot_magic = const DOMAIN_BOOT_MAGIC,
    boot_version = const DOMAIN_BOOT_VERSION,
    boot_size = const size_of::<DomainBootInfo>(),
    version_offset = const core::mem::offset_of!(DomainBootInfo, abi_version),
    size_offset = const core::mem::offset_of!(DomainBootInfo, struct_size),
    backend_offset = const core::mem::offset_of!(DomainBootInfo, hypervisor_backend),
    memory_offset = const core::mem::offset_of!(DomainBootInfo, memory_size),
    arm_backend = const HYPERVISOR_BACKEND_ARM_EL2,
);

const KERNEL_MAP_OFFSET: u64 = 0xffff_8000_0000_0000;

unsafe extern "C" {
    static __kernel_end: u8;
    static __arm64_boot_dtb: u8;
}

#[global_allocator]
static KERNEL_ALLOCATOR: HardenedKernelHeap = HardenedKernelHeap::empty();
static mut BOOT_INFO: BootInfo = BootInfo::empty();
static mut BOOT_MEMORY: MemoryRegion = MemoryRegion {
    start: 0,
    len: 0,
    region_type: MemoryType::Usable,
};

#[unsafe(no_mangle)]
extern "C" fn __mnu_arm64_native_entry(base: u64, size: u64, dtb_len: usize) -> ! {
    let used = core::ptr::addr_of!(__kernel_end) as u64 - KERNEL_MAP_OFFSET;
    if used >= size {
        mnu::arch::halt();
    }

    // SAFETY: The entry copied this many checked bytes to the retained DTB
    // buffer. These bytes are not shared with devices or the allocator.
    let dtb =
        unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(__arm64_boot_dtb), dtb_len) };
    let Ok(tree) = Fdt::new(dtb) else {
        mnu::arch::halt();
    };
    let Ok(map) = MemoryMap::from_fdt(&tree) else {
        mnu::arch::halt();
    };
    let end = base + size;
    if !map
        .ram()
        .iter()
        .any(|r| r.start() <= base && end <= r.end())
        || map
            .reservations()
            .iter()
            .any(|r| r.region.start() < end && base < r.region.end())
    {
        mnu::arch::halt();
    }
    let Ok(seed) = tree.rng_seed() else {
        mnu::arch::halt();
    };

    let hardware_id = mnu::percpu::hardware_id();
    // SAFETY: Entry admitted a single CPU, sized this span against its PA
    // width, and retained it in TTBR1. Only the part beyond the linked kernel
    // can enter the allocator. The firmware Image protocol owns that span.
    let info = unsafe {
        BOOT_MEMORY.start = base + used;
        BOOT_MEMORY.len = size - used;
        BOOT_INFO.physical_memory_offset = KERNEL_MAP_OFFSET - base;
        BOOT_INFO.memory_map_addr = core::ptr::addr_of!(BOOT_MEMORY) as u64;
        BOOT_INFO.memory_map_len = 1;
        BOOT_INFO.kernel_heap_addr = core::ptr::addr_of!(KERNEL_ALLOCATOR) as u64;
        BOOT_INFO.bsp_hardware_id = hardware_id;
        BOOT_INFO.cpu_hardware_ids[0] = hardware_id;
        if let Some(seed) = seed {
            BOOT_INFO.entropy_seed = seed;
            BOOT_INFO.entropy_seed_valid = 1;
            BOOT_INFO.feature_flags |= BOOT_FEATURE_ENTROPY;
        }
        &*core::ptr::addr_of!(BOOT_INFO)
    };
    mnu::kernel_entry(info)
}
