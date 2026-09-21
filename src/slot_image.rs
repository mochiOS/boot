//! Access to independently replaceable raw Boot A/B partitions.

use alloc::vec;
use alloc::vec::Vec;
use mochios_boot_selection::Slot;
use mochios_system_image::{SlotHeader, SlotRegion, SLOT_HEADER_LEN};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::partition::{GptPartitionType, PartitionInfo};
use uefi::table::boot::BootServices;
use uefi::{guid, Handle};

const BOOT_TYPE: GptPartitionType = GptPartitionType(guid!("6d6f6368-694f-5300-8000-6d5061727404"));
const SECTOR_BYTES: usize = 512;
const CHUNK_BYTES: usize = 64 * 1024;

#[repr(align(4096))]
struct AlignedChunk([u8; CHUNK_BYTES]);

#[derive(Clone, Copy)]
pub struct OpenSlot {
    pub handle: Handle,
    pub header: SlotHeader,
}

pub fn open(
    bt: &BootServices,
    image_handle: Handle,
    slot: Slot,
) -> Result<OpenSlot, &'static str> {
    let loaded = bt.open_protocol_exclusive::<LoadedImage>(image_handle)
        .map_err(|_| "loaded image unavailable")?;
    let esp = loaded.device().ok_or("boot ESP unavailable")?;
    drop(loaded);
    let esp_path = bt.open_protocol_exclusive::<DevicePath>(esp)
        .map_err(|_| "boot ESP path unavailable")?;
    let handles = bt.find_handles::<PartitionInfo>()
        .map_err(|_| "Boot partition enumeration failed")?;
    let mut candidates = [(u64::MAX, None), (u64::MAX, None)];
    let mut count = 0usize;
    for handle in handles {
        let partition = match bt.open_protocol_exclusive::<PartitionInfo>(handle) {
            Ok(partition) => partition,
            Err(_) => continue,
        };
        let Some(entry) = partition.gpt_partition_entry() else { continue };
        let partition_type = entry.partition_type_guid;
        if partition_type != BOOT_TYPE { continue; }
        let start = entry.starting_lba;
        drop(partition);
        let path = bt.open_protocol_exclusive::<DevicePath>(handle)
            .map_err(|_| "Boot partition path unavailable")?;
        if !crate::boot_state::same_disk(&esp_path, &path) { continue; }
        count += 1;
        if count > 2 { return Err("unexpected Boot partition count"); }
        let candidate = (start, Some(handle));
        if start < candidates[0].0 {
            candidates[1] = candidates[0];
            candidates[0] = candidate;
        } else if start < candidates[1].0 {
            candidates[1] = candidate;
        } else {
            return Err("too many Boot partitions");
        }
    }
    if count != 2 { return Err("Boot A/B partition pair unavailable"); }
    let handle = match slot {
        Slot::A => candidates[0].1,
        Slot::B => candidates[1].1,
    }.ok_or("Boot slot unavailable")?;
    let mut bytes = [0u8; SLOT_HEADER_LEN];
    read_exact(bt, handle, SlotRegion { offset: 0, length: SLOT_HEADER_LEN as u64 }, &mut bytes)?;
    let header = SlotHeader::decode(&bytes).map_err(|_| "Boot slot header rejected")?;
    let block = bt.open_protocol_exclusive::<BlockIO>(handle)
        .map_err(|_| "Boot slot Block I/O unavailable")?;
    let media = block.media();
    let size = (media.last_block() + 1).checked_mul(SECTOR_BYTES as u64)
        .ok_or("Boot slot size overflow")?;
    if header.image_size != size { return Err("Boot slot size does not match header"); }
    Ok(OpenSlot { handle, header })
}

pub fn read_vec(
    bt: &BootServices,
    handle: Handle,
    region: SlotRegion,
) -> Result<Vec<u8>, &'static str> {
    let length = usize::try_from(region.length).map_err(|_| "Boot slot region too large")?;
    let mut bytes = vec![0u8; length];
    read_exact(bt, handle, region, &mut bytes)?;
    Ok(bytes)
}

pub fn read_exact(
    bt: &BootServices,
    handle: Handle,
    region: SlotRegion,
    destination: &mut [u8],
) -> Result<(), &'static str> {
    if region.offset % SECTOR_BYTES as u64 != 0
        || destination.len() as u64 != region.length
        || region.length == 0
    {
        return Err("invalid Boot slot region");
    }
    let block = bt.open_protocol_exclusive::<BlockIO>(handle)
        .map_err(|_| "Boot slot Block I/O unavailable")?;
    let media = block.media();
    if !media.is_media_present() || media.block_size() != SECTOR_BYTES as u32 || media.io_align() > 4096 {
        return Err("unsupported Boot slot geometry");
    }
    let partition_size = (media.last_block() + 1).checked_mul(SECTOR_BYTES as u64)
        .ok_or("Boot slot size overflow")?;
    if region.offset.checked_add(region.length).is_none_or(|end| end > partition_size) {
        return Err("Boot slot region out of range");
    }
    let mut scratch = AlignedChunk([0; CHUNK_BYTES]);
    let mut copied = 0usize;
    while copied < destination.len() {
        let wanted = core::cmp::min(CHUNK_BYTES, destination.len() - copied);
        let aligned = (wanted + SECTOR_BYTES - 1) & !(SECTOR_BYTES - 1);
        let lba = region.offset / SECTOR_BYTES as u64 + (copied / SECTOR_BYTES) as u64;
        block.read_blocks(media.media_id(), lba, &mut scratch.0[..aligned])
            .map_err(|_| "Boot slot read failed")?;
        destination[copied..copied + wanted].copy_from_slice(&scratch.0[..wanted]);
        copied += wanted;
    }
    Ok(())
}
