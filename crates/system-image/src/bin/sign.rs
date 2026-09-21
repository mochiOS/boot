use std::{env, fs, io::{Read, Write}, path::Path};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use mochios_system_image::{Architecture, ArtifactDigests, Manifest};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 11 { return Err("usage: system-image-sign <system-image> <kernel> <kernel-meta> <initfs> <manifest> <private-key> <version> <build> <x86_64|aarch64> <expected-public-key-base64>".into()); }
    let key = read_key(Path::new(&args[6]))?;
    let expected = STANDARD.decode(args[10].to_str().ok_or("public key is not UTF-8")?)?;
    if expected.as_slice() != key.verifying_key().as_bytes() { return Err("private key does not match the configured trusted public key".into()); }
    let (size, system) = hash(Path::new(&args[1]))?;
    let (_, kernel) = hash(Path::new(&args[2]))?;
    let (_, kernel_meta) = hash(Path::new(&args[3]))?;
    let (_, initfs) = hash(Path::new(&args[4]))?;
    let digests = ArtifactDigests { system, kernel, kernel_meta, initfs };
    let architecture = match args[9].to_str() { Some("x86_64") => Architecture::X86_64, Some("aarch64") => Architecture::Aarch64, _ => return Err("unsupported architecture".into()) };
    let manifest = Manifest::create(args[7].to_str().ok_or("version is not UTF-8")?, args[8].to_str().ok_or("build is not UTF-8")?.parse()?, architecture, size, digests, &key).map_err(|e| format!("invalid manifest input: {e:?}"))?;
    let output = Path::new(&args[5]);
    let temporary = output.with_extension("new");
    let mut out = fs::OpenOptions::new().create(true).truncate(true).write(true).open(&temporary)?;
    out.write_all(manifest.as_bytes())?; out.sync_all()?; drop(out);
    fs::rename(temporary, output)?;
    Ok(())
}

fn hash(path: &Path) -> Result<(u64, [u8; 32]), Box<dyn std::error::Error>> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop { let count = file.read(&mut buffer)?; if count == 0 { break; } digest.update(&buffer[..count]); }
    Ok((size, digest.finalize().into()))
}

fn read_key(path: &Path) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let decoded = if bytes.len() == 32 { bytes } else { STANDARD.decode(std::str::from_utf8(&bytes)?.trim())? };
    Ok(SigningKey::from_bytes(decoded.as_slice().try_into().map_err(|_| "private key must contain 32 raw Ed25519 bytes")?))
}
