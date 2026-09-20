//! Host-side provisioning tool for a new, empty boot-state partition image.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use mochios_boot_selection::storage::{self, RecordIo};
use mochios_boot_selection::{Slot, RECORD_LEN};

const PARTITION_SIZE: u64 = 1024 * 1024;
const COPY_OFFSETS: [u64; 2] = [0, 4096];

struct FileRecords(File);

impl RecordIo for FileRecords {
    type Error = io::Error;

    fn read_copy(&mut self, index: usize) -> io::Result<[u8; RECORD_LEN]> {
        let mut bytes = [0; RECORD_LEN];
        self.0.read_exact_at(&mut bytes, COPY_OFFSETS[index])?;
        Ok(bytes)
    }

    fn write_copy(&mut self, index: usize, bytes: &[u8; RECORD_LEN]) -> io::Result<()> {
        self.0.write_all_at(bytes, COPY_OFFSETS[index])
    }

    fn sync(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}

#[derive(Clone, Copy)]
enum SeedState { StableA, StableB, TrialB }

fn run(path: &Path, state: SeedState) -> io::Result<()> {
    let file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
    let mut records = FileRecords(file);
    let result = (|| {
        records.0.set_len(PARTITION_SIZE)?;
        storage::initialize_blank(&mut records)
            .map_err(|error| io::Error::other(format!("boot-state initialization failed: {error:?}")))?;
        if matches!(state, SeedState::StableB | SeedState::TrialB) {
            storage::stage(&mut records, Slot::B)
                .map_err(|error| io::Error::other(format!("boot-state stage failed: {error:?}")))?;
        }
        if matches!(state, SeedState::StableB) {
            if storage::prepare_boot(&mut records)
                .map_err(|error| io::Error::other(format!("boot-state trial failed: {error:?}")))? != Slot::B {
                return Err(io::Error::other("boot-state trial selected the wrong slot"));
            }
            storage::confirm(&mut records, Slot::B)
                .map_err(|error| io::Error::other(format!("boot-state confirmation failed: {error:?}")))?;
        }
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(path); }
    result
}

fn main() -> io::Result<()> {
    let mut args = env::args_os();
    let _program = args.next();
    let Some(path) = args.next() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: boot-selection-seed <new-image-path>"));
    };
    let state = match args.next().as_deref() {
        None => SeedState::StableA,
        Some(value) if value == "B" => SeedState::StableB,
        Some(value) if value == "trial-B" => SeedState::TrialB,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: boot-selection-seed <new-image-path> [B|trial-B]")),
    };
    if args.next().is_some() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: boot-selection-seed <new-image-path> [B|trial-B]"));
    }
    run(Path::new(&path), state)
}
