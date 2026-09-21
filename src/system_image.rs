use mochios_boot_selection::Slot;
use mochios_system_image::{Architecture, ArtifactDigests, Manifest, RELEASE_PUBLIC_KEY};
#[cfg(feature = "development-system-key")]
use mochios_system_image::DEVELOPMENT_PUBLIC_KEY;
use sha2::{Digest, Sha256};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::partition::{GptPartitionType, PartitionInfo};
use uefi::table::boot::{AllocateType, BootServices, MemoryType};
use uefi::{guid, Handle};

const SYSTEM_TYPE: GptPartitionType = GptPartitionType(guid!("6d6f6368-694f-5300-8000-6d5061727401"));
const CHUNK_BYTES: usize = 64 * 1024;

pub fn verify(bt: &BootServices, image_handle: Handle, slot: Slot) -> Result<u64, &'static str> {
    let boot = crate::slot_image::open(bt, image_handle, slot)?;
    let manifest_bytes = crate::slot_image::read_vec(bt, boot.handle, boot.header.manifest)?;
    let manifest = Manifest::decode(&manifest_bytes).map_err(|_| "System manifest is malformed")?;
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
    let keys = &[RELEASE_PUBLIC_KEY, DEVELOPMENT_PUBLIC_KEY][..];
    #[cfg(not(feature = "development-system-key"))]
    let keys = &[RELEASE_PUBLIC_KEY][..];
    let digests = ArtifactDigests {
        system: digest.finalize().into(),
        kernel: hash_slot_region(bt, boot.handle, boot.header.kernel)?,
        kernel_meta: hash_slot_region(bt, boot.handle, boot.header.kernel_meta)?,
        initfs: hash_slot_region(bt, boot.handle, boot.header.initfs)?,
    };
    manifest.verify(&digests, keys).map_err(|_| "slot signature verification failed")?;
    Ok(manifest.build())
}

fn hash_slot_region(bt: &BootServices, handle: Handle, region: mochios_system_image::SlotRegion) -> Result<[u8; 32], &'static str> {
    let mut digest = Sha256::new();
    let bytes = crate::slot_image::read_vec(bt, handle, region)?;
    digest.update(&bytes);
    Ok(digest.finalize().into())
}
