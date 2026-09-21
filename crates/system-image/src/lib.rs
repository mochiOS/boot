#![no_std]

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub const MANIFEST_LEN: usize = 292;
pub const SIGNED_HEADER_LEN: usize = 228;
pub const SLOT_HEADER_LEN: usize = 4096;
const MAGIC: &[u8; 8] = b"MOSYSIG\0";
const SLOT_MAGIC: &[u8; 8] = b"MOSLOT\0\0";
const FORMAT_VERSION: u16 = 2;
const SLOT_FORMAT_VERSION: u16 = 1;
const CONTEXT: &[u8] = b"mochios-system-slot-v2\0";
pub const RELEASE_PUBLIC_KEY: [u8; 32] = [
    0xec, 0x68, 0x7e, 0xc6, 0x85, 0x04, 0x42, 0xc3,
    0x85, 0xdc, 0x7a, 0x19, 0x5c, 0xaf, 0xb2, 0xe0,
    0xeb, 0x59, 0x73, 0x1f, 0x71, 0x6b, 0xfa, 0x16,
    0x86, 0x72, 0xba, 0xf8, 0x93, 0xfb, 0x1d, 0xb8,
];
pub const DEVELOPMENT_PUBLIC_KEY: [u8; 32] = [
    0x93, 0x42, 0x5a, 0xde, 0x29, 0xe8, 0x0d, 0x01,
    0x80, 0x3b, 0xbe, 0x01, 0x58, 0x3c, 0x78, 0xa4,
    0x86, 0x6c, 0x09, 0x20, 0xc1, 0xfe, 0x17, 0x48,
    0xb7, 0xb8, 0x9a, 0xb2, 0x93, 0x4a, 0x2f, 0xf4,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Architecture { X86_64 = 1, Aarch64 = 2 }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error { InvalidFormat, UnsupportedVersion, UnknownKey, InvalidSignature, DigestMismatch }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotRegion {
    pub offset: u64,
    pub length: u64,
}

/// Canonical layout of one independently replaceable boot slot partition.
///
/// The header is intentionally not a general container directory. Regions are
/// required to appear in this exact order at 4 KiB boundaries, so alternate
/// byte interpretations cannot be introduced without changing the format.
/// Their contents and the System partition are authenticated by `Manifest`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotHeader {
    pub architecture: Architecture,
    pub image_size: u64,
    pub manifest: SlotRegion,
    pub kernel: SlotRegion,
    pub kernel_meta: SlotRegion,
    pub initfs: SlotRegion,
}

impl SlotHeader {
    pub fn create(
        architecture: Architecture,
        image_size: u64,
        kernel_len: u64,
        kernel_meta_len: u64,
        initfs_len: u64,
    ) -> Result<([u8; SLOT_HEADER_LEN], Self), Error> {
        if image_size == 0 || image_size % 4096 != 0
            || kernel_len == 0 || kernel_meta_len == 0 || initfs_len == 0
        {
            return Err(Error::InvalidFormat);
        }
        let manifest = SlotRegion { offset: SLOT_HEADER_LEN as u64, length: MANIFEST_LEN as u64 };
        let kernel = next_region(manifest, kernel_len)?;
        let kernel_meta = next_region(kernel, kernel_meta_len)?;
        let initfs = next_region(kernel_meta, initfs_len)?;
        if initfs.offset.checked_add(initfs.length).is_none_or(|end| end > image_size) {
            return Err(Error::InvalidFormat);
        }
        let header = Self { architecture, image_size, manifest, kernel, kernel_meta, initfs };
        let mut bytes = [0u8; SLOT_HEADER_LEN];
        bytes[..8].copy_from_slice(SLOT_MAGIC);
        bytes[8..10].copy_from_slice(&SLOT_FORMAT_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&(SLOT_HEADER_LEN as u16).to_le_bytes());
        bytes[12] = architecture as u8;
        bytes[16..24].copy_from_slice(&image_size.to_le_bytes());
        put_region(&mut bytes, 24, manifest);
        put_region(&mut bytes, 40, kernel);
        put_region(&mut bytes, 56, kernel_meta);
        put_region(&mut bytes, 72, initfs);
        Ok((bytes, header))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: &[u8; SLOT_HEADER_LEN] = bytes.try_into().map_err(|_| Error::InvalidFormat)?;
        if &bytes[..8] != SLOT_MAGIC { return Err(Error::InvalidFormat); }
        if u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != SLOT_FORMAT_VERSION {
            return Err(Error::UnsupportedVersion);
        }
        if u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as usize != SLOT_HEADER_LEN
            || !matches!(bytes[12], 1 | 2)
            || bytes[13..16].iter().any(|byte| *byte != 0)
            || bytes[88..].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidFormat);
        }
        let architecture = if bytes[12] == 1 { Architecture::X86_64 } else { Architecture::Aarch64 };
        let image_size = get_u64(bytes, 16);
        let manifest = get_region(bytes, 24);
        let kernel = get_region(bytes, 40);
        let kernel_meta = get_region(bytes, 56);
        let initfs = get_region(bytes, 72);
        let expected_manifest = SlotRegion { offset: SLOT_HEADER_LEN as u64, length: MANIFEST_LEN as u64 };
        if image_size == 0 || image_size % 4096 != 0 || manifest != expected_manifest
            || kernel != next_region(manifest, kernel.length)?
            || kernel_meta != next_region(kernel, kernel_meta.length)?
            || initfs != next_region(kernel_meta, initfs.length)?
            || kernel.length == 0 || kernel_meta.length == 0 || initfs.length == 0
            || initfs.offset.checked_add(initfs.length).is_none_or(|end| end > image_size)
        {
            return Err(Error::InvalidFormat);
        }
        Ok(Self { architecture, image_size, manifest, kernel, kernel_meta, initfs })
    }
}

fn align_4096(value: u64) -> Result<u64, Error> {
    value.checked_add(4095).map(|value| value & !4095).ok_or(Error::InvalidFormat)
}

fn next_region(previous: SlotRegion, length: u64) -> Result<SlotRegion, Error> {
    if length == 0 { return Err(Error::InvalidFormat); }
    let end = previous.offset.checked_add(previous.length).ok_or(Error::InvalidFormat)?;
    Ok(SlotRegion { offset: align_4096(end)?, length })
}

fn put_region(bytes: &mut [u8], offset: usize, region: SlotRegion) {
    bytes[offset..offset + 8].copy_from_slice(&region.offset.to_le_bytes());
    bytes[offset + 8..offset + 16].copy_from_slice(&region.length.to_le_bytes());
}

fn get_region(bytes: &[u8], offset: usize) -> SlotRegion {
    SlotRegion { offset: get_u64(bytes, offset), length: get_u64(bytes, offset + 8) }
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactDigests {
    pub system: [u8; 32],
    pub kernel: [u8; 32],
    pub kernel_meta: [u8; 32],
    pub initfs: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest { bytes: [u8; MANIFEST_LEN] }

impl Manifest {
    pub fn create(version: &str, build: u64, architecture: Architecture, image_size: u64,
        digests: ArtifactDigests, signing_key: &ed25519_dalek::SigningKey) -> Result<Self, Error> {
        use ed25519_dalek::Signer;
        if image_size == 0 || build == 0 || !valid_version(version) { return Err(Error::InvalidFormat); }
        let mut bytes = [0u8; MANIFEST_LEN];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&(MANIFEST_LEN as u16).to_le_bytes());
        bytes[12] = 1;
        bytes[13] = architecture as u8;
        bytes[20..28].copy_from_slice(&build.to_le_bytes());
        bytes[28..36].copy_from_slice(&image_size.to_le_bytes());
        bytes[36] = version.len() as u8;
        bytes[37..37 + version.len()].copy_from_slice(version.as_bytes());
        let public = signing_key.verifying_key().to_bytes();
        bytes[68..100].copy_from_slice(&Sha256::digest(public));
        bytes[100..132].copy_from_slice(&digests.system);
        bytes[132..164].copy_from_slice(&digests.kernel);
        bytes[164..196].copy_from_slice(&digests.kernel_meta);
        bytes[196..228].copy_from_slice(&digests.initfs);
        let mut message = [0u8; CONTEXT.len() + SIGNED_HEADER_LEN];
        message[..CONTEXT.len()].copy_from_slice(CONTEXT);
        message[CONTEXT.len()..].copy_from_slice(&bytes[..SIGNED_HEADER_LEN]);
        bytes[SIGNED_HEADER_LEN..].copy_from_slice(&signing_key.sign(&message).to_bytes());
        Ok(Self { bytes })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let bytes: [u8; MANIFEST_LEN] = bytes.try_into().map_err(|_| Error::InvalidFormat)?;
        if &bytes[..8] != MAGIC { return Err(Error::InvalidFormat); }
        if u16::from_le_bytes([bytes[8], bytes[9]]) != FORMAT_VERSION { return Err(Error::UnsupportedVersion); }
        if u16::from_le_bytes([bytes[10], bytes[11]]) as usize != MANIFEST_LEN || bytes[12] != 1
            || !matches!(bytes[13], 1 | 2) || bytes[14..20].iter().any(|b| *b != 0)
            || u64::from_le_bytes(bytes[20..28].try_into().unwrap()) == 0
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
    pub fn version(&self) -> &str {
        core::str::from_utf8(&self.bytes[37..37 + self.bytes[36] as usize]).unwrap()
    }
    pub fn build(&self) -> u64 { u64::from_le_bytes(self.bytes[20..28].try_into().unwrap()) }
    pub fn image_size(&self) -> u64 { u64::from_le_bytes(self.bytes[28..36].try_into().unwrap()) }
    pub fn verify(&self, actual: &ArtifactDigests, keys: &[[u8; 32]]) -> Result<(), Error> {
        for (expected, actual) in [
            (&self.bytes[100..132], &actual.system[..]),
            (&self.bytes[132..164], &actual.kernel[..]),
            (&self.bytes[164..196], &actual.kernel_meta[..]),
            (&self.bytes[196..228], &actual.initfs[..]),
        ] {
            if !constant_time_eq(expected, actual) { return Err(Error::DigestMismatch); }
        }
        let key = keys.iter().find(|key| constant_time_eq(&Sha256::digest(**key), &self.bytes[68..100]))
            .ok_or(Error::UnknownKey)?;
        let key = VerifyingKey::from_bytes(key).map_err(|_| Error::UnknownKey)?;
        let signature = Signature::from_slice(&self.bytes[SIGNED_HEADER_LEN..]).map_err(|_| Error::InvalidFormat)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn fixture() -> (Manifest, ArtifactDigests, [u8; 32]) {
        let key = SigningKey::from_bytes(&[7; 32]);
        let digests = ArtifactDigests { system: [1; 32], kernel: [2; 32], kernel_meta: [3; 32], initfs: [4; 32] };
        let manifest = Manifest::create("26.9", 1300, Architecture::X86_64, 4096, digests, &key).unwrap();
        (manifest, digests, key.verifying_key().to_bytes())
    }

    #[test]
    fn verifies_all_slot_artifacts() {
        let (manifest, digests, key) = fixture();
        assert_eq!(Manifest::decode(manifest.as_bytes()).unwrap().verify(&digests, &[key]), Ok(()));
        assert_eq!(manifest.build(), 1300);
    }

    #[test]
    fn rejects_each_modified_artifact() {
        let (manifest, digests, key) = fixture();
        for index in 0..4 {
            let mut changed = digests;
            [&mut changed.system, &mut changed.kernel, &mut changed.kernel_meta, &mut changed.initfs][index][0] ^= 1;
            assert_eq!(manifest.verify(&changed, &[key]), Err(Error::DigestMismatch));
        }
    }

    #[test]
    fn rejects_unknown_version_and_truncated_input() {
        let (manifest, _, _) = fixture();
        let mut bytes = *manifest.as_bytes();
        bytes[8..10].copy_from_slice(&3u16.to_le_bytes());
        assert_eq!(Manifest::decode(&bytes), Err(Error::UnsupportedVersion));
        assert_eq!(Manifest::decode(&bytes[..bytes.len() - 1]), Err(Error::InvalidFormat));
    }

    #[test]
    fn boot_slot_layout_is_canonical_and_bounded() {
        let (bytes, expected) = SlotHeader::create(
            Architecture::X86_64,
            128 * 1024 * 1024,
            789_640,
            64,
            100_663_296,
        ).unwrap();
        assert_eq!(SlotHeader::decode(&bytes), Ok(expected));
        assert_eq!(expected.manifest.offset, 4096);
        assert_eq!(expected.kernel.offset, 8192);
        assert_eq!(expected.kernel.offset % 4096, 0);
        assert_eq!(expected.kernel_meta.offset % 4096, 0);
        assert_eq!(expected.initfs.offset % 4096, 0);
    }

    #[test]
    fn boot_slot_rejects_noncanonical_unknown_and_oversized_layouts() {
        let (mut bytes, _) = SlotHeader::create(
            Architecture::X86_64,
            128 * 1024 * 1024,
            4096,
            64,
            4096,
        ).unwrap();
        bytes[40] ^= 1;
        assert_eq!(SlotHeader::decode(&bytes), Err(Error::InvalidFormat));

        let (mut bytes, _) = SlotHeader::create(
            Architecture::X86_64,
            128 * 1024 * 1024,
            4096,
            64,
            4096,
        ).unwrap();
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(SlotHeader::decode(&bytes), Err(Error::UnsupportedVersion));

        assert_eq!(
            SlotHeader::create(Architecture::X86_64, 8192, 4096, 64, 4096),
            Err(Error::InvalidFormat),
        );
    }
}
