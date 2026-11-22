//! Direct AArch64 Image entry for firmware that hands off at EL1.
//!
//! This bootstrap uses the loader-owned Image span to bring up mnu's VM.
//! The firmware supplies the device tree and owns the boot transaction.

#![no_std]
#![no_main]

#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
compile_error!("mochios-native requires aarch64-unknown-none");

use core::mem::size_of;
use core::ops::Range;
use mboot::fdt::{gic::GicV2, memory::MemoryMap, timer::Timers, Fdt};
use mnu::arch::gic::NativeTimer;
use mnu::mem::allocator::HardenedKernelHeap;
use mnu::{
    BootInfo, MemoryRegion, MemoryType, BOOT_FEATURE_ENTROPY, BOOT_FEATURE_FRAMEBUFFER,
    BOOT_FEATURE_INITFS,
};
use mnu_abi::hypervisor::{
    DomainBootInfo, DOMAIN_BOOT_MAGIC, DOMAIN_BOOT_VERSION, HYPERVISOR_BACKEND_ARM_EL2,
};

#[cfg(any(arm64_boot_disk, feature = "arm64-external-boot-disk"))]
mod boot_disk;
mod boot_framebuffer;
#[cfg(any(arm64_boot_disk, feature = "arm64-external-boot-disk"))]
mod boot_rtc;

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
const EMPTY_REGION: MemoryRegion = MemoryRegion {
    start: 0,
    len: 0,
    region_type: MemoryType::Reserved,
};
static mut BOOT_MEMORY: [MemoryRegion; 5] = [EMPTY_REGION; 5];

#[cfg(arm64_boot_module)]
static BOOT_MODULE: &[u8] = include_bytes!(env!("MOCHIOS_ARM64_BOOT_MODULE"));
#[cfg(not(arm64_boot_module))]
static BOOT_MODULE: &[u8] = &[];

#[unsafe(no_mangle)]
extern "C" fn __mnu_arm64_native_entry(
    base: u64,
    size: u64,
    dtb_len: usize,
    _firmware_dtb: u64,
) -> ! {
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
    let Some(timer) = native_timer(&tree, &map) else {
        mnu::arch::halt();
    };
    if !BOOT_MODULE.is_empty() && mnu_abi::boot_module::decode(BOOT_MODULE).is_err() {
        mnu::arch::halt();
    }
    #[cfg(not(feature = "arm64-external-boot-disk"))]
    let disk = None;
    #[cfg(feature = "arm64-external-boot-disk")]
    let disk = {
        let Some(disk) = external_disk(&tree, base + used..end, _firmware_dtb, dtb_len) else {
            mnu::arch::halt();
        };
        let address = KERNEL_MAP_OFFSET + (disk.start - base);
        // SAFETY: The checked range is in the retained mapping and outside
        // the linked kernel and original DTB. The memory map keeps it owned.
        if !unsafe { boot_disk::install(address as usize, (disk.end - disk.start) as usize) } {
            mnu::arch::halt();
        }
        Some(disk)
    };
    #[cfg(arm64_boot_disk)]
    if !boot_disk::install_embedded() {
        mnu::arch::halt();
    }
    #[cfg(any(arm64_boot_disk, feature = "arm64-external-boot-disk"))]
    boot_rtc::prepare(&tree, &map);

    let framebuffer = boot_framebuffer::admit(&tree, &map, base, end);
    let (mut memory, mut memory_count) = boot_regions(base, base + used, end, disk);
    if let Some(fb) = framebuffer {
        memory[memory_count] = fb.memory_region();
        memory_count += 1;
        memory[..memory_count].sort_unstable_by_key(|region| region.start);
    }

    let hardware_id = mnu::percpu::hardware_id();
    // SAFETY: Entry admitted a single CPU, sized this span against its PA
    // width, and retained it in TTBR1. Only the part beyond the linked kernel
    // can enter the allocator. The firmware Image protocol owns that span.
    let info = unsafe {
        BOOT_MEMORY = memory;
        BOOT_INFO.physical_memory_offset = KERNEL_MAP_OFFSET - base;
        BOOT_INFO.memory_map_addr = core::ptr::addr_of!(BOOT_MEMORY) as u64;
        BOOT_INFO.memory_map_len = memory_count as u64;
        BOOT_INFO.kernel_heap_addr = core::ptr::addr_of!(KERNEL_ALLOCATOR) as u64;
        BOOT_INFO.bsp_hardware_id = hardware_id;
        BOOT_INFO.cpu_hardware_ids[0] = hardware_id;
        if let Some(fb) = framebuffer {
            BOOT_INFO.framebuffer_addr = fb.base;
            BOOT_INFO.framebuffer_size = fb.bytes;
            BOOT_INFO.screen_width = fb.width;
            BOOT_INFO.screen_height = fb.height;
            BOOT_INFO.stride = fb.stride;
            BOOT_INFO.feature_flags |= BOOT_FEATURE_FRAMEBUFFER;
        }
        if !BOOT_MODULE.is_empty() {
            // The linker's read-only segment pins the complete boot module.
            BOOT_INFO.initfs_addr = BOOT_MODULE.as_ptr() as u64;
            BOOT_INFO.initfs_size = BOOT_MODULE.len() as u64;
            BOOT_INFO.feature_flags |= BOOT_FEATURE_INITFS;
        }
        if let Some(seed) = seed {
            BOOT_INFO.entropy_seed = seed;
            BOOT_INFO.entropy_seed_valid = 1;
            BOOT_INFO.feature_flags |= BOOT_FEATURE_ENTROPY;
        }
        &*core::ptr::addr_of!(BOOT_INFO)
    };
    // SAFETY: The AArch64 Image handoff gives this non-secure boot CPU the
    // assigned controller. The DTB validated both register banks outside RAM.
    if unsafe { mnu::arch::gic::install_native_timer(timer) }.is_err() {
        mnu::arch::halt();
    }
    mnu::kernel_entry(info)
}

fn boot_regions(
    base: u64,
    free: u64,
    end: u64,
    disk: Option<Range<u64>>,
) -> ([MemoryRegion; 5], usize) {
    let mut memory = [EMPTY_REGION; 5];
    let mut count = 0;
    let mut add = |start, end, region_type| {
        if start < end {
            memory[count] = MemoryRegion {
                start,
                len: end - start,
                region_type,
            };
            count += 1;
        }
    };
    add(base, free, MemoryType::KernelImage);
    if let Some(disk) = disk {
        add(free, disk.start, MemoryType::Usable);
        add(disk.start, disk.end, MemoryType::KernelImage);
        add(disk.end, end, MemoryType::Usable);
    } else {
        add(free, end, MemoryType::Usable);
    }
    (memory, count)
}

#[cfg(feature = "arm64-external-boot-disk")]
fn external_disk(
    tree: &Fdt<'_>,
    free: Range<u64>,
    firmware_dtb: u64,
    dtb_len: usize,
) -> Option<Range<u64>> {
    use mboot::fdt::Token;
    const MAX_DISK: u64 = 256 * 1024 * 1024;
    const PAGE: u64 = 4096;

    let mut chosen = false;
    let mut start = None;
    let mut end = None;
    for token in tree.tokens() {
        match token.ok()? {
            Token::BeginNode { depth: 1, name } => chosen = name == "chosen",
            Token::EndNode { depth: 1 } => chosen = false,
            Token::Property {
                depth: 1,
                name,
                value,
            } if chosen => {
                let target = match name {
                    "linux,initrd-start" => &mut start,
                    "linux,initrd-end" => &mut end,
                    _ => continue,
                };
                let address = match value.len() {
                    4 => u64::from(u32::from_be_bytes(value.try_into().ok()?)),
                    8 => u64::from_be_bytes(value.try_into().ok()?),
                    _ => return None,
                };
                if target.replace((address, value.len())).is_some() {
                    return None;
                }
            }
            _ => {}
        }
    }
    let (start, start_width) = start?;
    let (end, end_width) = end?;
    let dtb_end = firmware_dtb.checked_add(dtb_len as u64)?;
    let length = end.checked_sub(start)?;
    (start_width == end_width
        && length > 0
        && length <= MAX_DISK
        && start % PAGE == 0
        && end % PAGE == 0
        && free.start <= start
        && end <= free.end
        && (end <= firmware_dtb || dtb_end <= start))
        .then_some(start..end)
}

fn native_timer(tree: &Fdt<'_>, map: &MemoryMap<'_>) -> Option<NativeTimer> {
    let gic = GicV2::from_fdt(tree).ok()?;
    let timers = Timers::from_fdt(tree, &gic).ok()?;
    for range in [gic.distributor, gic.cpu] {
        if range.size() < 4096
            || map
                .ram()
                .iter()
                .any(|ram| ram.start() < range.end() && range.start() < ram.end())
        {
            return None;
        }
    }
    let counter_frequency: u64;
    // SAFETY: The entry checked EL1. CNTFRQ is a read-only architectural value.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) counter_frequency,
            options(nomem, nostack, preserves_flags));
    }
    if timers
        .counter_frequency
        .is_some_and(|hz| u64::from(hz) != counter_frequency)
    {
        return None;
    }
    Some(NativeTimer {
        distributor: gic.distributor.start(),
        cpu_interface: gic.cpu.start(),
        interrupt: u32::from(timers.virtual_timer.id),
    })
}
