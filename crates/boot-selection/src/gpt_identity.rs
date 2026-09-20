//! Read-only evidence that an OS-visible GPT contains the ESP used by UEFI.
//! This is not, by itself, authorization to write the disk or boot state.

const SECTOR_BYTES: usize = 512;
const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11,
    0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

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

/// Searches one disk. The caller must enumerate every disk and reject zero or
/// multiple matching disks; a cloned GPT must never silently select disk 0.
pub fn find_boot_esp<R: SectorReader>(
    reader: &mut R,
    disk_sectors: u64,
    boot_esp_guid: [u8; 16],
) -> Result<Option<EspMatch>, MatchError<R::Error>> {
    if disk_sectors < 68 || boot_esp_guid == [0; 16] {
        return Err(MatchError::InvalidGpt);
    }
    let mut header = [0u8; SECTOR_BYTES];
    reader.read_sector(1, &mut header).map_err(MatchError::Read)?;
    if &header[..8] != b"EFI PART" || le32(&header[8..12]) != 0x0001_0000 {
        return Err(MatchError::InvalidGpt);
    }
    let header_size = le32(&header[12..16]) as usize;
    if !(92..=SECTOR_BYTES).contains(&header_size) || le32(&header[20..24]) != 0
        || le64(&header[24..32]) != 1 || le64(&header[32..40]) != disk_sectors - 1
        || header[56..72] == [0; 16]
    {
        return Err(MatchError::InvalidGpt);
    }
    let expected_header_crc = le32(&header[16..20]);
    header[16..20].fill(0);
    if !crc32(!0, &header[..header_size]) != expected_header_crc {
        return Err(MatchError::InvalidGpt);
    }
    let first_usable = le64(&header[40..48]);
    let last_usable = le64(&header[48..56]);
    let table_lba = le64(&header[72..80]);
    let entry_count = le32(&header[80..84]);
    let entry_size = le32(&header[84..88]);
    let expected_table_crc = le32(&header[88..92]);
    let table_bytes = (entry_count as u64).checked_mul(entry_size as u64)
        .ok_or(MatchError::InvalidGpt)?;
    let table_sectors = table_bytes.checked_add(511).ok_or(MatchError::InvalidGpt)? / 512;
    if first_usable >= last_usable || last_usable >= disk_sectors - 1
        || table_lba < 2 || table_lba.checked_add(table_sectors).is_none_or(|end| end > first_usable)
        || !(1..=4096).contains(&entry_count) || !(128..=512).contains(&entry_size)
        || entry_size % 8 != 0
    {
        return Err(MatchError::InvalidGpt);
    }

    let mut table_crc = !0;
    let mut entry = [0u8; 512];
    let mut entry_fill = 0usize;
    let mut entry_index = 0u32;
    let mut matched = None;
    for sector_index in 0..table_sectors {
        let mut sector = [0u8; SECTOR_BYTES];
        reader.read_sector(table_lba + sector_index, &mut sector).map_err(MatchError::Read)?;
        let remaining = table_bytes - sector_index * 512;
        let valid = core::cmp::min(remaining, 512) as usize;
        table_crc = crc32(table_crc, &sector[..valid]);
        for &byte in &sector[..valid] {
            entry[entry_fill] = byte;
            entry_fill += 1;
            if entry_fill == entry_size as usize {
                let unique = &entry[16..32];
                if unique != [0; 16] && canonical_guid(unique) == boot_esp_guid {
                    let first_lba = le64(&entry[32..40]);
                    let last_lba = le64(&entry[40..48]);
                    if entry[..16] != ESP_TYPE || first_lba < first_usable
                        || last_lba > last_usable || first_lba > last_lba
                    {
                        return Err(MatchError::InvalidGpt);
                    }
                    if matched.is_some() { return Err(MatchError::Ambiguous); }
                    matched = Some(EspMatch {
                        partition_index: entry_index,
                        first_lba, last_lba,
                    });
                }
                entry_index += 1;
                entry_fill = 0;
            }
        }
    }
    if entry_fill != 0 || !table_crc != expected_table_crc {
        return Err(MatchError::InvalidGpt);
    }
    Ok(matched)
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
        let table_crc = !crc32(!0, &disk.0[2]);
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
}
