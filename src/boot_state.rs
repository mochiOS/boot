//! Boot-state discovery and durable trial-attempt consumption. A pending slot
//! is selected only after its reduced attempt count has been synced and read back.

use mochios_boot_selection::storage::{self, RecordIo};
use mochios_boot_selection::{Slot, RECORD_LEN};
use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::partition::{GptPartitionType, PartitionInfo};
use uefi::table::boot::BootServices;
use uefi::{guid, Handle, Status};

const STATE_TYPE: GptPartitionType = GptPartitionType(guid!("6d6f6368-694f-5300-8000-6d5061727403"));
const COPY_LBA: [u64; 2] = [0, 8];

#[derive(Clone, Copy, Debug)]
pub enum Probe {
    Legacy,
    Stable(Slot),
    Trial { slot: Slot, attempts_remaining: u8 },
    WriteFailedFallback(Slot),
    Invalid,
}

#[repr(align(4096))]
struct AlignedSector([u8; 512]);

struct UefiRecords<'a> {
    block: &'a mut BlockIO,
    media_id: u32,
}

impl RecordIo for UefiRecords<'_> {
    type Error = Status;

    fn read_copy(&mut self, index: usize) -> Result<[u8; RECORD_LEN], Status> {
        let mut sector = AlignedSector([0; 512]);
        self.block.read_blocks(self.media_id, COPY_LBA[index], &mut sector.0)
            .map_err(|error| error.status())?;
        let mut record = [0; RECORD_LEN];
        record.copy_from_slice(&sector.0[..RECORD_LEN]);
        Ok(record)
    }

    fn write_copy(&mut self, index: usize, bytes: &[u8; RECORD_LEN]) -> Result<(), Status> {
        let mut sector = AlignedSector([0; 512]);
        self.block.read_blocks(self.media_id, COPY_LBA[index], &mut sector.0)
            .map_err(|error| error.status())?;
        sector.0[..RECORD_LEN].copy_from_slice(bytes);
        self.block.write_blocks(self.media_id, COPY_LBA[index], &sector.0)
            .map_err(|error| error.status())
    }

    fn sync(&mut self) -> Result<(), Status> {
        self.block.flush_blocks().map_err(|error| error.status())
    }
}

/// Compare the physical-device path prefix before the partition's HD node.
/// A similarly named state partition on a different attached disk is ignored.
pub(crate) fn same_disk(left: &DevicePath, right: &DevicePath) -> bool {
    let mut left_nodes = left.node_iter();
    let mut right_nodes = right.node_iter();
    let mut parent_nodes = 0;
    loop {
        match (left_nodes.next(), right_nodes.next()) {
            (Some(a), Some(b)) => {
                let a_hd = a.full_type() == (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE);
                let b_hd = b.full_type() == (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE);
                if a_hd || b_hd {
                    return parent_nodes > 0 && a_hd && b_hd
                        && left_nodes.next().is_none() && right_nodes.next().is_none();
                }
                if a != b { return false; }
                parent_nodes += 1;
            }
            _ => return false,
        }
    }
}

/// Identifies the ESP the firmware used to launch this bootloader. A later
/// writer must match this GUID against a unique GPT entry before touching any
/// boot-state sector; a missing GUID is never permission to guess a disk.
pub fn boot_esp_guid(bt: &BootServices, image_handle: Handle) -> Option<[u8; 16]> {
    let image = bt.open_protocol_exclusive::<LoadedImage>(image_handle).ok()?;
    let device = image.device()?;
    drop(image);
    let partition = bt.open_protocol_exclusive::<PartitionInfo>(device).ok()?;
    let entry = partition.gpt_partition_entry()?;
    let kind = entry.partition_type_guid;
    if kind != GptPartitionType::EFI_SYSTEM_PARTITION {
        return None;
    }
    // `uguid::Guid::to_bytes` preserves the UEFI/GPT in-memory byte order for
    // the first three fields. BootInfo carries the canonical RFC 4122 byte
    // representation so OS-side GPT readers do not depend on a firmware type.
    let mut guid = entry.unique_partition_guid.to_bytes();
    guid[..4].reverse();
    guid[4..6].reverse();
    guid[6..8].reverse();
    (guid != [0; 16]).then_some(guid)
}

pub fn probe(bt: &BootServices, image_handle: Handle) -> Probe {
    let Some(esp_handle) = bt.open_protocol_exclusive::<LoadedImage>(image_handle)
        .ok().and_then(|image| image.device()) else { return Probe::Legacy; };
    let Ok(esp_path) = bt.open_protocol_exclusive::<DevicePath>(esp_handle) else {
        return Probe::Legacy;
    };
    let Ok(handles) = bt.find_handles::<PartitionInfo>() else { return Probe::Legacy; };
    let mut candidate = None;
    for handle in handles {
        let Ok(partition) = bt.open_protocol_exclusive::<PartitionInfo>(handle) else { continue; };
        let matches_type = partition.gpt_partition_entry()
            .is_some_and(|entry| {
                let kind = entry.partition_type_guid;
                kind == STATE_TYPE
            });
        drop(partition);
        if !matches_type { continue; }
        let Ok(path) = bt.open_protocol_exclusive::<DevicePath>(handle) else { return Probe::Invalid; };
        if !same_disk(&esp_path, &path) { continue; }
        if candidate.replace(handle).is_some() { return Probe::Invalid; }
    }
    let Some(handle) = candidate else { return Probe::Legacy; };
    let Ok(mut block) = bt.open_protocol_exclusive::<BlockIO>(handle) else { return Probe::Invalid; };
    let media = block.media();
    if !media.is_media_present() || media.block_size() != 512 || media.last_block() < COPY_LBA[1]
        || media.io_align() > 4096
    { return Probe::Invalid; }
    let media_id = media.media_id();
    let mut records = UefiRecords { block: &mut block, media_id };
    let Ok(selected) = storage::load(&mut records) else { return Probe::Invalid; };
    let active = selected.record.active();
    if selected.record.pending().is_none() {
        return Probe::Stable(active);
    }
    match storage::prepare_boot(&mut records) {
        Ok(slot) if slot != active => Probe::Trial {
            slot,
            attempts_remaining: selected.record.attempts_remaining() - 1,
        },
        Ok(slot) => Probe::Stable(slot),
        Err(_) => Probe::WriteFailedFallback(active),
    }
}
