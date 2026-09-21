use mochios_boot_selection::Slot;
use mochios_system_image::{Architecture, Manifest, MANIFEST_LEN};
use sha2::{Digest, Sha256};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::file::{File, FileAttribute, FileMode, FileType};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::media::partition::{GptPartitionType, PartitionInfo};
use uefi::table::boot::{AllocateType, BootServices, MemoryType};
use uefi::{guid, CStr16, Handle};

const SYSTEM_TYPE: GptPartitionType = GptPartitionType(guid!("6d6f6368-694f-5300-8000-6d5061727401"));
const RELEASE_KEY: [u8; 32] = [
    0xec, 0x68, 0x7e, 0xc6, 0x85, 0x04, 0x42, 0xc3,
    0x85, 0xdc, 0x7a, 0x19, 0x5c, 0xaf, 0xb2, 0xe0,
    0xeb, 0x59, 0x73, 0x1f, 0x71, 0x6b, 0xfa, 0x16,
    0x86, 0x72, 0xba, 0xf8, 0x93, 0xfb, 0x1d, 0xb8,
];
#[cfg(feature = "development-system-key")]
const DEVELOPMENT_KEY: [u8; 32] = [
    0x93, 0x42, 0x5a, 0xde, 0x29, 0xe8, 0x0d, 0x01,
    0x80, 0x3b, 0xbe, 0x01, 0x58, 0x3c, 0x78, 0xa4,
    0x86, 0x6c, 0x09, 0x20, 0xc1, 0xfe, 0x17, 0x48,
    0xb7, 0xb8, 0x9a, 0xb2, 0x93, 0x4a, 0x2f, 0xf4,
];
const CHUNK_BYTES: usize = 64 * 1024;

pub fn verify(bt: &BootServices, image_handle: Handle, slot: Slot, path: &CStr16) -> Result<(), &'static str> {
    let manifest = read_manifest(bt, image_handle, path)?;
    let loaded = bt.open_protocol_exclusive::<LoadedImage>(image_handle).map_err(|_| "loaded image unavailable")?;
    let esp = loaded.device().ok_or("boot ESP unavailable")?;
    drop(loaded);
    let esp_path = bt.open_protocol_exclusive::<DevicePath>(esp).map_err(|_| "boot ESP path unavailable")?;
    let handles = bt.find_handles::<PartitionInfo>().map_err(|_| "partition enumeration failed")?;
    let mut candidates = [(u64::MAX, None), (u64::MAX, None)];
    let mut candidate_count = 0usize;
    for handle in handles {
        let partition = match bt.open_protocol_exclusive::<PartitionInfo>(handle) { Ok(value) => value, Err(_) => continue };
        let Some(entry) = partition.gpt_partition_entry() else { continue };
        let partition_type = entry.partition_type_guid;
        if partition_type != SYSTEM_TYPE { continue; }
        let start = entry.starting_lba;
        drop(partition);
        let path = bt.open_protocol_exclusive::<DevicePath>(handle).map_err(|_| "System partition path unavailable")?;
        if !crate::boot_state::same_disk(&esp_path, &path) { continue; }
        candidate_count += 1;
        if candidate_count > 2 { return Err("unexpected System partition count"); }
        let candidate = (start, Some(handle));
        if start < candidates[0].0 { candidates[1] = candidates[0]; candidates[0] = candidate; }
        else if start < candidates[1].0 { candidates[1] = candidate; }
        else { return Err("too many System partitions"); }
    }
    if candidate_count != 2 || candidates[0].1.is_none() || candidates[1].1.is_none() { return Err("System A/B partition pair unavailable"); }
    if manifest.architecture() != Architecture::X86_64 { return Err("System architecture mismatch"); }
    let handle = match slot { Slot::A => candidates[0].1.unwrap(), Slot::B => candidates[1].1.unwrap() };
    let block = bt.open_protocol_exclusive::<BlockIO>(handle).map_err(|_| "System Block I/O unavailable")?;
    let media = block.media();
    if !media.is_media_present() || media.block_size() != 512 || media.io_align() > 4096 { return Err("unsupported System block geometry"); }
    let size = (media.last_block() + 1).checked_mul(512).ok_or("System size overflow")?;
    if size != manifest.image_size() { return Err("System size does not match manifest"); }
    let address = bt.allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, CHUNK_BYTES / 4096)
        .map_err(|_| "System verification buffer allocation failed")?;
    let buffer = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, CHUNK_BYTES) };
    let mut digest = Sha256::new();
    let mut lba = 0u64;
    let blocks = size / 512;
    while lba < blocks {
        let count = core::cmp::min((CHUNK_BYTES / 512) as u64, blocks - lba);
        let bytes = count as usize * 512;
        block.read_blocks(media.media_id(), lba, &mut buffer[..bytes]).map_err(|_| "System read failed")?;
        digest.update(&buffer[..bytes]);
        lba += count;
    }
    #[cfg(feature = "development-system-key")]
    let keys = &[RELEASE_KEY, DEVELOPMENT_KEY][..];
    #[cfg(not(feature = "development-system-key"))]
    let keys = &[RELEASE_KEY][..];
    manifest.verify(&digest.finalize().into(), keys).map_err(|_| "System signature verification failed")
}

fn read_manifest(bt: &BootServices, image_handle: Handle, path: &CStr16) -> Result<Manifest, &'static str> {
    let loaded = bt.open_protocol_exclusive::<LoadedImage>(image_handle).map_err(|_| "loaded image unavailable")?;
    let device = loaded.device().ok_or("boot ESP unavailable")?;
    drop(loaded);
    let mut fs = bt.open_protocol_exclusive::<SimpleFileSystem>(device).map_err(|_| "boot ESP filesystem unavailable")?;
    let mut root = fs.open_volume().map_err(|_| "boot ESP volume unavailable")?;
    let file = root.open(path, FileMode::Read, FileAttribute::empty()).map_err(|_| "System manifest unavailable")?;
    let mut file = match file.into_type().map_err(|_| "System manifest unreadable")? { FileType::Regular(file) => file, _ => return Err("System manifest is not a file") };
    let mut bytes = [0u8; MANIFEST_LEN];
    let mut read = 0;
    while read < bytes.len() { let count = file.read(&mut bytes[read..]).map_err(|_| "System manifest read failed")?; if count == 0 { break; } read += count; }
    if read != bytes.len() { return Err("System manifest is truncated"); }
    let mut extra = [0u8; 1];
    if file.read(&mut extra).map_err(|_| "System manifest read failed")? != 0 { return Err("System manifest has trailing bytes"); }
    Manifest::decode(&bytes).map_err(|_| "System manifest is malformed")
}
