//! Host-only, read-only integration probe for a complete A/B disk image.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use mochios_boot_selection::gpt_identity::{SectorReader, find_boot_esp};

struct FileReader(File);

impl SectorReader for FileReader {
    type Error = std::io::Error;

    fn read_sector(&mut self, lba: u64, sector: &mut [u8; 512]) -> Result<(), Self::Error> {
        let offset = lba.checked_mul(512).ok_or(std::io::ErrorKind::InvalidInput)?;
        self.0.seek(SeekFrom::Start(offset))?;
        self.0.read_exact(sector)
    }
}

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 || [8, 13, 18, 23].iter().any(|&i| bytes[i] != b'-') {
        return None;
    }
    let mut result = [0u8; 16];
    let mut digits = bytes.iter().copied().filter(|byte| *byte != b'-');
    for byte in &mut result {
        let high = (digits.next()? as char).to_digit(16)? as u8;
        let low = (digits.next()? as char).to_digit(16)? as u8;
        *byte = high << 4 | low;
    }
    Some(result)
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let image = args.next().ok_or("missing image path")?;
    let guid = parse_guid(&args.next().ok_or("missing ESP GUID")?)
        .ok_or("invalid ESP GUID")?;
    let first = args.next().ok_or("missing first LBA")?
        .parse::<u64>().map_err(|_| "invalid first LBA")?;
    let last = args.next().ok_or("missing last LBA")?
        .parse::<u64>().map_err(|_| "invalid last LBA")?;
    if args.next().is_some() { return Err("too many arguments".into()); }
    let file = File::open(image).map_err(|error| error.to_string())?;
    let bytes = file.metadata().map_err(|error| error.to_string())?.len();
    if bytes == 0 || bytes % 512 != 0 { return Err("invalid image size".into()); }
    let matched = find_boot_esp(&mut FileReader(file), bytes / 512, guid)
        .map_err(|error| format!("GPT identity probe failed: {error:?}"))?
        .ok_or("ESP GUID not found")?;
    if matched.partition_index != 0 || matched.first_lba != first || matched.last_lba != last {
        return Err(format!("ESP range mismatch: {matched:?}"));
    }
    println!("validated boot ESP GPT identity: partition 1, LBAs {first}..{last}");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
