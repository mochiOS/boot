//! A development boot medium retained in the native Image. Its Data writes
//! last for this boot, so the source image is safe to use in repeated trials.

use core::ops::Range;
use mnu::cext::disk::McxDiskOps;
use mnu::platform::{self, DeviceControlResponse, PlatformError};
use mnu_abi::status::Error;
use mochios_boot_selection::gpt_identity::{legacy_data_partition, SectorReader};
use mochios_mdriver_protocol::control::{
    MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH, MDRIVER_DEVICE_FEATURE_BLOCK_READ,
    MDRIVER_DEVICE_FEATURE_BLOCK_WRITE, MDRIVER_DEVICE_FEATURE_EPHEMERAL,
    MDRIVER_DEVICE_FEATURE_PARTITIONED, MDRIVER_DEVICE_FEATURE_SOFTWARE,
};

core::arch::global_asm!(
    include_str!(concat!(env!("OUT_DIR"), "/arm64-boot-disk.S")),
    options(raw)
);

unsafe extern "C" {
    static mut __arm64_boot_disk_start: u8;
    static __arm64_boot_disk_end: u8;
}

const SECTOR_BYTES: usize = 512;
const DEVICE_ID: u32 = 0;

struct Disk {
    base: usize,
    length: usize,
    data: Range<usize>,
}

static DISK: spin::Once<Disk> = spin::Once::new();

struct ImageReader<'a>(&'a [u8]);

impl SectorReader for ImageReader<'_> {
    type Error = ();

    fn read_sector(&mut self, lba: u64, output: &mut [u8; SECTOR_BYTES]) -> Result<(), ()> {
        let range = checked_range(lba, SECTOR_BYTES, self.0.len()).ok_or(())?;
        output.copy_from_slice(&self.0[range]);
        Ok(())
    }
}

pub(super) fn install() -> bool {
    let base = core::ptr::addr_of_mut!(__arm64_boot_disk_start) as usize;
    let end = core::ptr::addr_of!(__arm64_boot_disk_end) as usize;
    let Some(length) = end
        .checked_sub(base)
        .filter(|size| *size % SECTOR_BYTES == 0)
    else {
        return false;
    };
    // SAFETY: The linker pinned this initialized byte range, and the disk
    // callbacks have not been published. Parsing only borrows it temporarily.
    let bytes = unsafe { core::slice::from_raw_parts(base as *const u8, length) };
    let Ok(data) = legacy_data_partition(&mut ImageReader(bytes), (length / SECTOR_BYTES) as u64)
    else {
        return false;
    };
    let Some(data_bytes) = data
        .sectors()
        .checked_mul(SECTOR_BYTES as u64)
        .and_then(|size| usize::try_from(size).ok())
        .and_then(|size| checked_range(data.first_lba, size, length))
    else {
        return false;
    };
    DISK.call_once(|| Disk {
        base,
        length,
        data: data_bytes,
    });
    platform::install(&PLATFORM).is_ok() && mnu::cext::disk::activate_bundle(1, &OPERATIONS)
}

fn checked_range(lba: u64, length: usize, capacity: usize) -> Option<Range<usize>> {
    if length == 0 || length % SECTOR_BYTES != 0 {
        return None;
    }
    let start = usize::try_from(lba).ok()?.checked_mul(SECTOR_BYTES)?;
    let end = start.checked_add(length)?;
    (end <= capacity).then_some(start..end)
}

extern "C" fn probe() -> i32 {
    i32::from(DISK.get().is_some())
}

extern "C" fn read(id: u32, lba: u64, output: *mut u8, length: usize) -> i32 {
    transfer(id, lba, output, length, false)
}

extern "C" fn write(id: u32, lba: u64, input: *const u8, length: usize) -> i32 {
    transfer(id, lba, input.cast_mut(), length, true)
}

fn transfer(id: u32, lba: u64, buffer: *mut u8, length: usize, write: bool) -> i32 {
    let Some(disk) = DISK.get().filter(|_| id == DEVICE_ID) else {
        return Error::NotFound.into_raw() as i32;
    };
    let Some(range) = checked_range(lba, length, disk.length).filter(|_| !buffer.is_null()) else {
        return Error::InvalidArgument.into_raw() as i32;
    };
    let address = (disk.base + range.start) as *mut u8;
    if write {
        if range.start < disk.data.start || range.end > disk.data.end {
            return Error::AccessDenied.into_raw() as i32;
        }
        // SAFETY: The kernel serializes disk operations and supplies a
        // separate kernel buffer. The checked target lies wholly in Data.
        unsafe { core::ptr::copy_nonoverlapping(buffer, address, length) };
    } else {
        // SAFETY: The checked source lies wholly in the retained Image. The
        // kernel supplies a separate writable buffer under the same lock.
        unsafe { core::ptr::copy_nonoverlapping(address, buffer, length) };
    }
    0
}

extern "C" fn flush(id: u32) -> i32 {
    if id == DEVICE_ID && DISK.get().is_some() {
        // Operations are serialized and complete in RAM before returning.
        0
    } else {
        Error::NotFound.into_raw() as i32
    }
}

static OPERATIONS: McxDiskOps = McxDiskOps {
    probe,
    read_sector: read,
    write_sector: write,
    flush,
};

fn device_control(
    authority: &str,
    operation: u16,
    id: u32,
    args: [u64; 4],
) -> Result<DeviceControlResponse, PlatformError> {
    if authority != "device.storage" || operation != mnu_abi::STORAGE_CONTROL_LIST_DEVICE {
        return Err(PlatformError::Unsupported);
    }
    if id != 0 || args[1..] != [0; 3] {
        return Err(PlatformError::InvalidRequest);
    }
    let (status, features) = if args[0] == 0 {
        (
            mnu_abi::STORAGE_STATUS_OK,
            MDRIVER_DEVICE_FEATURE_SOFTWARE
                | MDRIVER_DEVICE_FEATURE_EPHEMERAL
                | MDRIVER_DEVICE_FEATURE_PARTITIONED
                | MDRIVER_DEVICE_FEATURE_BLOCK_READ
                | MDRIVER_DEVICE_FEATURE_BLOCK_WRITE
                | MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH,
        )
    } else {
        (mnu_abi::STORAGE_STATUS_END, 0)
    };
    Ok(DeviceControlResponse {
        status,
        device_id: DEVICE_ID,
        values: [features, 0, 0, 0],
    })
}

static PLATFORM: platform::PlatformOps = platform::PlatformOps {
    display_info: || Err(PlatformError::Unsupported),
    display_transfer_limit: || Err(PlatformError::Unsupported),
    present_display: |_, _, _, _, _| Err(PlatformError::Unsupported),
    commit_display: |_, _, _, _| Err(PlatformError::Unsupported),
    device_control,
};
