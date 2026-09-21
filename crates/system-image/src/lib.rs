#![no_std]

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub const MANIFEST_LEN: usize = 196;
pub const SIGNED_HEADER_LEN: usize = 132;
const MAGIC: &[u8; 8] = b"MOSYSIG\0";
const CONTEXT: &[u8] = b"mochios-system-image-v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Architecture { X86_64 = 1, Aarch64 = 2 }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error { InvalidFormat, UnsupportedVersion, UnknownKey, InvalidSignature, DigestMismatch }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    bytes: [u8; MANIFEST_LEN],
}

impl Manifest {
    pub fn create(version: &str, build: u64, architecture: Architecture, image_size: u64,
        image_digest: [u8; 32], signing_key: &ed25519_dalek::SigningKey) -> Result<Self, Error> {
        use ed25519_dalek::Signer;
        if image_size == 0 || !valid_version(version) { return Err(Error::InvalidFormat); }
        let mut bytes = [0u8; MANIFEST_LEN];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&(MANIFEST_LEN as u16).to_le_bytes());
        bytes[12] = 1;
        bytes[13] = architecture as u8;
        bytes[20..28].copy_from_slice(&build.to_le_bytes());
        bytes[28..36].copy_from_slice(&image_size.to_le_bytes());
        bytes[36] = version.len() as u8;
        bytes[37..37 + version.len()].copy_from_slice(version.as_bytes());
        let public = signing_key.verifying_key().to_bytes();
        bytes[68..100].copy_from_slice(&Sha256::digest(public));
        bytes[100..132].copy_from_slice(&image_digest);
        let mut message = [0u8; CONTEXT.len() + SIGNED_HEADER_LEN];
        message[..CONTEXT.len()].copy_from_slice(CONTEXT);
        message[CONTEXT.len()..].copy_from_slice(&bytes[..SIGNED_HEADER_LEN]);
        bytes[132..].copy_from_slice(&signing_key.sign(&message).to_bytes());
        Ok(Self { bytes })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: [u8; MANIFEST_LEN] = bytes.try_into().map_err(|_| Error::InvalidFormat)?;
        if &bytes[..8] != MAGIC { return Err(Error::InvalidFormat); }
        if u16::from_le_bytes([bytes[8], bytes[9]]) != 1 { return Err(Error::UnsupportedVersion); }
        if u16::from_le_bytes([bytes[10], bytes[11]]) as usize != MANIFEST_LEN || bytes[12] != 1
            || !matches!(bytes[13], 1 | 2) || bytes[14..20].iter().any(|b| *b != 0)
            || u64::from_le_bytes(bytes[28..36].try_into().unwrap()) == 0 {
            return Err(Error::InvalidFormat);
        }
        let len = bytes[36] as usize;
        if len == 0 || len > 31 || bytes[37 + len..68].iter().any(|b| *b != 0)
            || core::str::from_utf8(&bytes[37..37 + len]).ok().is_none_or(|v| !valid_version(v)) {
            return Err(Error::InvalidFormat);
        }
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8; MANIFEST_LEN] { &self.bytes }
    pub fn architecture(&self) -> Architecture { if self.bytes[13] == 1 { Architecture::X86_64 } else { Architecture::Aarch64 } }
    pub fn image_size(&self) -> u64 { u64::from_le_bytes(self.bytes[28..36].try_into().unwrap()) }
    pub fn image_digest(&self) -> &[u8; 32] { self.bytes[100..132].try_into().unwrap() }
    pub fn verify(&self, actual_digest: &[u8; 32], keys: &[[u8; 32]]) -> Result<(), Error> {
        if !constant_time_eq(self.image_digest(), actual_digest) { return Err(Error::DigestMismatch); }
        let key = keys.iter().find(|key| constant_time_eq(&Sha256::digest(**key), &self.bytes[68..100]))
            .ok_or(Error::UnknownKey)?;
        let key = VerifyingKey::from_bytes(key).map_err(|_| Error::UnknownKey)?;
        let signature = Signature::from_slice(&self.bytes[132..]).map_err(|_| Error::InvalidFormat)?;
        let mut message = [0u8; CONTEXT.len() + SIGNED_HEADER_LEN];
        message[..CONTEXT.len()].copy_from_slice(CONTEXT);
        message[CONTEXT.len()..].copy_from_slice(&self.bytes[..SIGNED_HEADER_LEN]);
        key.verify_strict(&message, &signature).map_err(|_| Error::InvalidSignature)
    }
}

fn valid_version(value: &str) -> bool {
    let mut count = 0;
    for part in value.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) { return false; }
        count += 1;
    }
    matches!(count, 2 | 3) && value.len() <= 31
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}
