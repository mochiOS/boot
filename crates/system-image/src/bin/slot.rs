use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use mochios_system_image::{Architecture, MANIFEST_LEN, Manifest, SLOT_HEADER_LEN, SlotHeader};

fn copy_at(source: &Path, output: &mut File, offset: u64, expected_len: u64) -> io::Result<()> {
    let mut input = File::open(source)?;
    if input.metadata()?.len() != expected_len {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "input length changed while building slot"));
    }
    output.seek(SeekFrom::Start(offset))?;
    io::copy(&mut input, output).and_then(|written| {
        if written == expected_len { Ok(()) } else {
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "slot input was truncated"))
        }
    })
}

fn parse_size(value: &str) -> io::Result<u64> {
    let size = value.parse::<u64>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid slot image size"))?;
    if size == 0 || size % 4096 != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "slot image size must be 4 KiB aligned"));
    }
    Ok(size)
}

fn main() -> io::Result<()> {
    let arguments: Vec<_> = env::args_os().collect();
    if arguments.len() != 7 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: system-slot-image <manifest> <kernel> <kernel-meta> <initfs> <size-bytes> <output>",
        ));
    }
    let manifest_path = PathBuf::from(&arguments[1]);
    let kernel_path = PathBuf::from(&arguments[2]);
    let meta_path = PathBuf::from(&arguments[3]);
    let initfs_path = PathBuf::from(&arguments[4]);
    let image_size = parse_size(arguments[5].to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "slot image size is not UTF-8")
    })?)?;
    let output_path = PathBuf::from(&arguments[6]);

    let manifest_bytes = fs::read(&manifest_path)?;
    if manifest_bytes.len() != MANIFEST_LEN || Manifest::decode(&manifest_bytes).is_err() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid System manifest"));
    }
    let kernel_len = fs::metadata(&kernel_path)?.len();
    let meta_len = fs::metadata(&meta_path)?.len();
    let initfs_len = fs::metadata(&initfs_path)?.len();
    let (header_bytes, header) = SlotHeader::create(
        Architecture::X86_64,
        image_size,
        kernel_len,
        meta_len,
        initfs_len,
    ).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "slot artifacts do not fit"))?;

    let mut output = OpenOptions::new().read(true).write(true).create_new(true).open(&output_path)?;
    output.set_len(image_size)?;
    output.write_all(&header_bytes)?;
    copy_at(&manifest_path, &mut output, header.manifest.offset, header.manifest.length)?;
    copy_at(&kernel_path, &mut output, header.kernel.offset, header.kernel.length)?;
    copy_at(&meta_path, &mut output, header.kernel_meta.offset, header.kernel_meta.length)?;
    copy_at(&initfs_path, &mut output, header.initfs.offset, header.initfs.length)?;
    output.sync_all()?;

    output.seek(SeekFrom::Start(0))?;
    let mut readback = [0u8; SLOT_HEADER_LEN];
    output.read_exact(&mut readback)?;
    if SlotHeader::decode(&readback).ok() != Some(header) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "slot header read-back mismatch"));
    }
    Ok(())
}
