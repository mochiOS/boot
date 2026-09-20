//! Host-only test utility. Never shipped in a mochiOS image.
//! Confirms an already-attempted trial B in an isolated 1 MiB state image.

use std::env;
use std::fs::{File, OpenOptions};
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

    fn sync(&mut self) -> io::Result<()> { self.0.sync_all() }
}

fn run(path: &Path) -> io::Result<()> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != PARTITION_SIZE {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "expected an isolated 1 MiB boot-state partition image"));
    }
    let mut records = FileRecords(file);
    let selected = storage::load(&mut records)
        .map_err(|error| io::Error::other(format!("boot-state load failed: {error:?}")))?;
    if selected.record.pending() != Some(Slot::B) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "no pending system B trial to confirm"));
    }
    let confirmed = storage::confirm(&mut records, Slot::B)
        .map_err(|error| io::Error::other(format!("boot-state confirmation failed: {error:?}")))?;
    if confirmed.active() != Slot::B || confirmed.pending().is_some() {
        return Err(io::Error::other("boot-state confirmation did not select stable B"));
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args = env::args_os();
    let _program = args.next();
    let Some(path) = args.next() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: boot-selection-confirm-test <isolated-state-image>"));
    };
    if args.next().is_some() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "usage: boot-selection-confirm-test <isolated-state-image>"));
    }
    run(Path::new(&path))
}
