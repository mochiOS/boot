//! Read-only evidence that an OS-visible GPT contains the ESP used by UEFI.
//! This is not, by itself, authorization to write the disk or boot state.

const SECTOR_BYTES: usize = 512;
const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11,
    0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const ESP_TYPE_GUID: [u8; 16] = [
    0xc1, 0x2a, 0x73, 0x28, 0xf8, 0x1f, 0x11, 0xd2,
    0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const SYSTEM_TYPE_GUID: [u8; 16] = *b"mochiOS\0\x80\0mPart\x01";
const DATA_TYPE_GUID: [u8; 16] = *b"mochiOS\0\x80\0mPart\x02";
const STATE_TYPE_GUID: [u8; 16] = *b"mochiOS\0\x80\0mPart\x03";
const BOOT_TYPE_GUID: [u8; 16] = *b"mochiOS\0\x80\0mPart\x04";

pub trait SectorReader {
    type Error;
    fn read_sector(&mut self, lba: u64, sector: &mut [u8; SECTOR_BYTES]) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EspMatch {
    pub partition_index: u32,
    pub first_lba: u64,
    pub last_lba: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartitionRange {
    pub partition_index: u32,
    pub first_lba: u64,
    pub last_lba: u64,
}

impl PartitionRange {
    pub const fn sectors(self) -> u64 { self.last_lba - self.first_lba + 1 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateLayout {
    pub esp: PartitionRange,
    pub boot: [PartitionRange; 2],
    pub system: [PartitionRange; 2],
    pub data: PartitionRange,
    pub state: PartitionRange,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MatchError<E> {
    Read(E),
    InvalidGpt,
    Ambiguous,
}

fn le32(bytes: &[u8]) -> u32 { u32::from_le_bytes(bytes.try_into().unwrap()) }
fn le64(bytes: &[u8]) -> u64 { u64::from_le_bytes(bytes.try_into().unwrap()) }

fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    crc
}

fn canonical_guid(on_disk: &[u8]) -> [u8; 16] {
    let mut guid: [u8; 16] = on_disk.try_into().unwrap();
    guid[..4].reverse();
    guid[4..6].reverse();
    guid[6..8].reverse();
    guid
}

#[derive(Clone, Copy)]
struct GptHeader {
    first_usable: u64,
    last_usable: u64,
    table_lba: u64,
    entry_count: u32,
    entry_size: u32,
    table_bytes: u64,
    table_sectors: u64,
    table_crc: u32,
}

fn scan_gpt<R, F>(
    reader: &mut R,
    disk_sectors: u64,
    mut visit: F,
) -> Result<(), MatchError<R::Error>>
where
    R: SectorReader,
    F: FnMut(u32, &[u8]) -> Result<(), MatchError<R::Error>>,
{
    if disk_sectors < 68 { return Err(MatchError::InvalidGpt); }
    let mut sector = [0u8; SECTOR_BYTES];
    reader.read_sector(1, &mut sector).map_err(MatchError::Read)?;
    if &sector[..8] != b"EFI PART" || le32(&sector[8..12]) != 0x0001_0000 {
        return Err(MatchError::InvalidGpt);
    }
    let header_size = le32(&sector[12..16]) as usize;
    if !(92..=SECTOR_BYTES).contains(&header_size) || le32(&sector[20..24]) != 0
        || le64(&sector[24..32]) != 1 || le64(&sector[32..40]) != disk_sectors - 1
        || sector[56..72] == [0; 16]
    {
        return Err(MatchError::InvalidGpt);
    }
    let expected_header_crc = le32(&sector[16..20]);
    sector[16..20].fill(0);
    if !crc32(!0, &sector[..header_size]) != expected_header_crc {
        return Err(MatchError::InvalidGpt);
    }
    let entry_count = le32(&sector[80..84]);
    let entry_size = le32(&sector[84..88]);
    let table_bytes = (entry_count as u64).checked_mul(entry_size as u64)
        .ok_or(MatchError::InvalidGpt)?;
    let header = GptHeader {
        first_usable: le64(&sector[40..48]),
        last_usable: le64(&sector[48..56]),
        table_lba: le64(&sector[72..80]),
        entry_count,
        entry_size,
        table_bytes,
        table_sectors: table_bytes.checked_add(511).ok_or(MatchError::InvalidGpt)? / 512,
        table_crc: le32(&sector[88..92]),
    };
    if header.first_usable >= header.last_usable || header.last_usable >= disk_sectors - 1
        || header.table_lba < 2
        || header.table_lba.checked_add(header.table_sectors)
            .is_none_or(|end| end > header.first_usable)
        || !(1..=4096).contains(&header.entry_count)
        || !(128..=512).contains(&header.entry_size) || header.entry_size % 8 != 0
    {
        return Err(MatchError::InvalidGpt);
    }

    let mut table_crc = !0;
    let mut entry = [0u8; 512];
    let mut entry_fill = 0usize;
    let mut entry_index = 0u32;
    for sector_index in 0..header.table_sectors {
        reader.read_sector(header.table_lba + sector_index, &mut sector)
            .map_err(MatchError::Read)?;
        let remaining = header.table_bytes - sector_index * 512;
        let valid = core::cmp::min(remaining, 512) as usize;
        table_crc = crc32(table_crc, &sector[..valid]);
        for &byte in &sector[..valid] {
            entry[entry_fill] = byte;
            entry_fill += 1;
            if entry_fill == header.entry_size as usize {
                if entry[..16] != [0; 16] {
                    let first_lba = le64(&entry[32..40]);
                    let last_lba = le64(&entry[40..48]);
                    if entry[16..32] == [0; 16] || first_lba < header.first_usable
                        || last_lba > header.last_usable || first_lba > last_lba
                    {
                        return Err(MatchError::InvalidGpt);
                    }
                    visit(entry_index, &entry[..header.entry_size as usize])?;
                }
                entry_index += 1;
                entry_fill = 0;
            }
        }
    }
    if entry_fill != 0 || entry_index != header.entry_count || !table_crc != header.table_crc {
        return Err(MatchError::InvalidGpt);
    }
    Ok(())
}

/// Searches one disk. The caller must enumerate every disk and reject zero or
/// multiple matching disks; a cloned GPT must never silently select disk 0.
pub fn find_boot_esp<R: SectorReader>(
    reader: &mut R,
    disk_sectors: u64,
    boot_esp_guid: [u8; 16],
) -> Result<Option<EspMatch>, MatchError<R::Error>> {
    let mut matched = None;
    if boot_esp_guid == [0; 16] { return Err(MatchError::InvalidGpt); }
    scan_gpt(reader, disk_sectors, |entry_index, entry| {
        if canonical_guid(&entry[16..32]) == boot_esp_guid {
            if entry[..16] != ESP_TYPE { return Err(MatchError::InvalidGpt); }
            if matched.is_some() { return Err(MatchError::Ambiguous); }
            matched = Some(EspMatch { partition_index: entry_index,
                first_lba: le64(&entry[32..40]), last_lba: le64(&entry[40..48]) });
        }
        Ok(())
    })?;
    Ok(matched)
}

fn partition_name_eq(entry: &[u8], expected: &str) -> bool {
    let mut units = expected.bytes();
    for offset in (56..128).step_by(2) {
        let unit = u16::from_le_bytes([entry[offset], entry[offset + 1]]);
        match units.next() {
            Some(byte) if unit == u16::from(byte) => {}
            Some(_) => return false,
            None => return unit == 0,
        }
    }
    units.next().is_none()
}

/// Resolves the complete update layout only on the disk whose ESP identity
/// was supplied by UEFI. Names and type GUIDs are both part of the contract;
/// missing, duplicate, overlapping, or unexpected matches fail closed.
pub fn find_update_layout<R: SectorReader>(
    reader: &mut R,
    disk_sectors: u64,
    boot_esp_guid: [u8; 16],
) -> Result<Option<UpdateLayout>, MatchError<R::Error>> {
    if boot_esp_guid == [0; 16] { return Err(MatchError::InvalidGpt); }
    let mut parts: [Option<PartitionRange>; 7] = [None; 7];
    scan_gpt(reader, disk_sectors, |index, entry| {
        let type_guid = canonical_guid(&entry[..16]);
        let unique_guid = canonical_guid(&entry[16..32]);
        let slot = if unique_guid == boot_esp_guid {
            if type_guid != ESP_TYPE_GUID || !partition_name_eq(entry, "mochiOS ESP") {
                return Err(MatchError::InvalidGpt);
            }
            Some(0)
        } else {
            if type_guid == BOOT_TYPE_GUID && partition_name_eq(entry, "mochiOS Boot A") { Some(1) }
            else if type_guid == SYSTEM_TYPE_GUID && partition_name_eq(entry, "mochiOS System A") { Some(2) }
            else if type_guid == BOOT_TYPE_GUID && partition_name_eq(entry, "mochiOS Boot B") { Some(3) }
            else if type_guid == SYSTEM_TYPE_GUID && partition_name_eq(entry, "mochiOS System B") { Some(4) }
            else if type_guid == DATA_TYPE_GUID && partition_name_eq(entry, "mochiOS Data") { Some(5) }
            else if type_guid == STATE_TYPE_GUID && partition_name_eq(entry, "mochiOS Boot State") { Some(6) }
            else { None }
        };
        if let Some(slot) = slot {
            if parts[slot].is_some() { return Err(MatchError::Ambiguous); }
            parts[slot] = Some(PartitionRange { partition_index: index,
                first_lba: le64(&entry[32..40]), last_lba: le64(&entry[40..48]) });
        }
        Ok(())
    })?;
    if parts[0].is_none() { return Ok(None); }
    if parts.iter().any(Option::is_none) { return Err(MatchError::InvalidGpt); }
    let parts = parts.map(Option::unwrap);
    for left in 0..parts.len() {
        for right in left + 1..parts.len() {
            if parts[left].first_lba <= parts[right].last_lba
                && parts[right].first_lba <= parts[left].last_lba
            {
                return Err(MatchError::InvalidGpt);
            }
        }
    }
    Ok(Some(UpdateLayout {
        esp: parts[0], boot: [parts[1], parts[3]], system: [parts[2], parts[4]],
        data: parts[5], state: parts[6],
    }))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    const GUID: [u8; 16] = [
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
    ];

    struct MemoryDisk(Vec<[u8; 512]>);
    impl SectorReader for MemoryDisk {
        type Error = ();
        fn read_sector(&mut self, lba: u64, sector: &mut [u8; 512]) -> Result<(), Self::Error> {
            *sector = *self.0.get(lba as usize).ok_or(())?;
            Ok(())
        }
    }

    fn finish_crcs(disk: &mut MemoryDisk) {
        let entry_count = le32(&disk.0[1][80..84]) as usize;
        let entry_size = le32(&disk.0[1][84..88]) as usize;
        let table_bytes = entry_count * entry_size;
        let mut checksum = !0;
        for offset in 0..table_bytes {
            checksum = crc32(checksum, &disk.0[2 + offset / 512][offset % 512..offset % 512 + 1]);
        }
        let table_crc = !checksum;
        disk.0[1][88..92].copy_from_slice(&table_crc.to_le_bytes());
        disk.0[1][16..20].fill(0);
        let header_crc = !crc32(!0, &disk.0[1][..92]);
        disk.0[1][16..20].copy_from_slice(&header_crc.to_le_bytes());
    }

    fn fixture_disk() -> MemoryDisk {
        let mut disk = MemoryDisk(vec![[0u8; 512]; 128]);
        let header = &mut disk.0[1];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[32..40].copy_from_slice(&127u64.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&94u64.to_le_bytes());
        header[56] = 1;
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&4u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
        add_esp(&mut disk, 0);
        finish_crcs(&mut disk);
        disk
    }

    fn add_esp(disk: &mut MemoryDisk, index: usize) {
        let entry = &mut disk.0[2][index * 128..(index + 1) * 128];
        entry[..16].copy_from_slice(&ESP_TYPE);
        let mut on_disk = GUID;
        on_disk[..4].reverse();
        on_disk[4..6].reverse();
        on_disk[6..8].reverse();
        entry[16..32].copy_from_slice(&on_disk);
        entry[32..40].copy_from_slice(&34u64.to_le_bytes());
        entry[40..48].copy_from_slice(&40u64.to_le_bytes());
    }

    fn on_disk_guid(mut guid: [u8; 16]) -> [u8; 16] {
        guid[..4].reverse();
        guid[4..6].reverse();
        guid[6..8].reverse();
        guid
    }

    fn add_partition(
        disk: &mut MemoryDisk,
        index: usize,
        type_guid: [u8; 16],
        unique_guid: [u8; 16],
        first_lba: u64,
        last_lba: u64,
        name: &str,
    ) {
        let byte_offset = index * 128;
        let sector = 2 + byte_offset / 512;
        let offset = byte_offset % 512;
        let entry = &mut disk.0[sector][offset..offset + 128];
        entry[..16].copy_from_slice(&on_disk_guid(type_guid));
        entry[16..32].copy_from_slice(&on_disk_guid(unique_guid));
        entry[32..40].copy_from_slice(&first_lba.to_le_bytes());
        entry[40..48].copy_from_slice(&last_lba.to_le_bytes());
        for (index, byte) in name.bytes().enumerate() {
            entry[56 + index * 2] = byte;
        }
    }

    fn layout_disk() -> MemoryDisk {
        let mut disk = MemoryDisk(vec![[0u8; 512]; 256]);
        let header = &mut disk.0[1];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[32..40].copy_from_slice(&255u64.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&222u64.to_le_bytes());
        header[56] = 1;
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&8u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
        let specs = [
            (ESP_TYPE_GUID, GUID, 34, 39, "mochiOS ESP"),
            (BOOT_TYPE_GUID, [1; 16], 40, 59, "mochiOS Boot A"),
            (SYSTEM_TYPE_GUID, [2; 16], 60, 89, "mochiOS System A"),
            (BOOT_TYPE_GUID, [3; 16], 90, 109, "mochiOS Boot B"),
            (SYSTEM_TYPE_GUID, [4; 16], 110, 139, "mochiOS System B"),
            (DATA_TYPE_GUID, [5; 16], 140, 199, "mochiOS Data"),
            (STATE_TYPE_GUID, [6; 16], 200, 207, "mochiOS Boot State"),
        ];
        for (index, (kind, unique, first, last, name)) in specs.into_iter().enumerate() {
            add_partition(&mut disk, index, kind, unique, first, last, name);
        }
        finish_crcs(&mut disk);
        disk
    }

    #[test]
    fn matches_canonical_uefi_guid_only_after_crc_checks() {
        let mut disk = fixture_disk();
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Ok(Some(EspMatch {
            partition_index: 0, first_lba: 34, last_lba: 40,
        })));
        assert_eq!(find_boot_esp(&mut disk, 128, [9; 16]), Ok(None));
        disk.0[2][80] ^= 1;
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Err(MatchError::InvalidGpt));
    }

    #[test]
    fn rejects_corrupt_header_wrong_type_duplicate_and_out_of_range() {
        let mut disk = fixture_disk();
        disk.0[1][32] ^= 1;
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Err(MatchError::InvalidGpt));

        let mut disk = fixture_disk();
        disk.0[2][0] ^= 1;
        finish_crcs(&mut disk);
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Err(MatchError::InvalidGpt));

        let mut disk = fixture_disk();
        add_esp(&mut disk, 1);
        finish_crcs(&mut disk);
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Err(MatchError::Ambiguous));

        let mut disk = fixture_disk();
        disk.0[2][32..40].copy_from_slice(&1u64.to_le_bytes());
        finish_crcs(&mut disk);
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Err(MatchError::InvalidGpt));
    }

    #[test]
    fn rejects_missing_identity_and_truncated_disk() {
        let mut disk = fixture_disk();
        assert_eq!(find_boot_esp(&mut disk, 128, [0; 16]), Err(MatchError::InvalidGpt));
        assert_eq!(find_boot_esp(&mut disk, 100, GUID), Err(MatchError::InvalidGpt));
        disk.0.truncate(2);
        assert_eq!(find_boot_esp(&mut disk, 128, GUID), Err(MatchError::Read(())));
    }

    #[test]
    fn resolves_only_the_complete_non_overlapping_update_layout() {
        let mut disk = layout_disk();
        let layout = find_update_layout(&mut disk, 256, GUID).unwrap().unwrap();
        assert_eq!(layout.esp.first_lba, 34);
        assert_eq!(layout.boot.map(PartitionRange::sectors), [20, 20]);
        assert_eq!(layout.system.map(PartitionRange::sectors), [30, 30]);
        assert_eq!(layout.data.first_lba, 140);
        assert_eq!(layout.state.first_lba, 200);

        assert_eq!(find_update_layout(&mut disk, 256, [9; 16]), Ok(None));
    }

    #[test]
    fn update_layout_rejects_missing_duplicate_mistyped_and_overlapping_parts() {
        let mut disk = layout_disk();
        disk.0[3][2 * 128..2 * 128 + 16].fill(0);
        finish_crcs(&mut disk);
        assert_eq!(find_update_layout(&mut disk, 256, GUID), Err(MatchError::InvalidGpt));

        let mut disk = layout_disk();
        add_partition(&mut disk, 7, BOOT_TYPE_GUID, [7; 16], 208, 211, "mochiOS Boot A");
        finish_crcs(&mut disk);
        assert_eq!(find_update_layout(&mut disk, 256, GUID), Err(MatchError::Ambiguous));

        let mut disk = layout_disk();
        add_partition(&mut disk, 1, DATA_TYPE_GUID, [1; 16], 40, 59, "mochiOS Boot A");
        finish_crcs(&mut disk);
        assert_eq!(find_update_layout(&mut disk, 256, GUID), Err(MatchError::InvalidGpt));

        let mut disk = layout_disk();
        disk.0[2][3 * 128 + 32..3 * 128 + 40].copy_from_slice(&80u64.to_le_bytes());
        finish_crcs(&mut disk);
        assert_eq!(find_update_layout(&mut disk, 256, GUID), Err(MatchError::InvalidGpt));
    }
}
