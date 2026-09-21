use std::{env, fs, io::{Read, Write}, path::Path};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use mochios_system_image::{Architecture, Manifest};
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() != 8 { return Err("usage: system-image-sign <image> <manifest> <private-key> <version> <build> <x86_64|aarch64> <expected-public-key-base64>".into()); }
    let key = read_key(Path::new(&args[3]))?;
    let expected = STANDARD.decode(args[7].to_str().ok_or("public key is not UTF-8")?)?;
    if expected.as_slice() != key.verifying_key().as_bytes() { return Err("private key does not match the configured trusted public key".into()); }
    let mut file = fs::File::open(&args[1])?;
    let size = file.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop { let count = file.read(&mut buffer)?; if count == 0 { break; } digest.update(&buffer[..count]); }
    let architecture = match args[6].to_str() { Some("x86_64") => Architecture::X86_64, Some("aarch64") => Architecture::Aarch64, _ => return Err("unsupported architecture".into()) };
    let manifest = Manifest::create(args[4].to_str().ok_or("version is not UTF-8")?, args[5].to_str().ok_or("build is not UTF-8")?.parse()?, architecture, size, digest.finalize().into(), &key).map_err(|e| format!("invalid manifest input: {e:?}"))?;
    let output = Path::new(&args[2]);
    let temporary = output.with_extension("new");
    let mut out = fs::OpenOptions::new().create(true).truncate(true).write(true).open(&temporary)?;
    out.write_all(manifest.as_bytes())?; out.sync_all()?; drop(out);
    fs::rename(temporary, output)?;
    Ok(())
}

fn read_key(path: &Path) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let decoded = if bytes.len() == 32 { bytes } else { STANDARD.decode(std::str::from_utf8(&bytes)?.trim())? };
    Ok(SigningKey::from_bytes(decoded.as_slice().try_into().map_err(|_| "private key must contain 32 raw Ed25519 bytes")?))
}
