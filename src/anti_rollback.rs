use uefi::table::runtime::{RuntimeServices, VariableAttributes, VariableVendor};
use uefi::{cstr16, guid, Status};

const VENDOR: VariableVendor = VariableVendor(guid!("6d6f6368-694f-5200-8000-6d5061727401"));
const NAME: &uefi::CStr16 = cstr16!("MochiOSRollbackFloor");
const MAGIC: &[u8; 8] = b"MORBFLR\0";
const FORMAT_VERSION: u16 = 1;
const RECORD_LEN: usize = 24;
const ATTRIBUTES: VariableAttributes = VariableAttributes::NON_VOLATILE
    .union(VariableAttributes::BOOTSERVICE_ACCESS);

pub struct Decision { pub floor: u64, pub advanced: bool }

pub fn enforce(runtime: &RuntimeServices, candidate: u64, stable: bool) -> Result<Decision, &'static str> {
    let embedded = parse_build(option_env!("MOCHIOS_MINIMUM_BUILD").unwrap_or("1"))?;
    let stored = read(runtime)?.unwrap_or(0);
    let floor = core::cmp::max(embedded, stored);
    if candidate < floor { return Err("signed build is below the rollback floor"); }
    if stable && candidate > stored {
        write(runtime, candidate)?;
        if read(runtime)? != Some(candidate) { return Err("rollback floor read-back failed"); }
        return Ok(Decision { floor: candidate, advanced: true });
    }
    Ok(Decision { floor, advanced: false })
}

fn read(runtime: &RuntimeServices) -> Result<Option<u64>, &'static str> {
    let mut buffer = [0u8; RECORD_LEN];
    let (bytes, attributes) = match runtime.get_variable(NAME, &VENDOR, &mut buffer) {
        Ok(value) => value,
        Err(error) if error.status() == Status::NOT_FOUND => return Ok(None),
        Err(_) => return Err("rollback floor variable is unreadable"),
    };
    if attributes != ATTRIBUTES || bytes.len() != RECORD_LEN || &bytes[..8] != MAGIC
        || u16::from_le_bytes(bytes[8..10].try_into().unwrap()) != FORMAT_VERSION
        || bytes[10..16].iter().any(|byte| *byte != 0) {
        return Err("rollback floor variable is malformed");
    }
    let build = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    if build == 0 { return Err("rollback floor variable is invalid"); }
    Ok(Some(build))
}

fn write(runtime: &RuntimeServices, build: u64) -> Result<(), &'static str> {
    if build == 0 { return Err("zero rollback floor is invalid"); }
    let mut bytes = [0u8; RECORD_LEN];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[16..24].copy_from_slice(&build.to_le_bytes());
    runtime.set_variable(NAME, &VENDOR, ATTRIBUTES, &bytes)
        .map_err(|_| "rollback floor could not be persisted")
}

fn parse_build(value: &str) -> Result<u64, &'static str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("embedded rollback floor is malformed");
    }
    let mut result = 0u64;
    for byte in value.bytes() {
        result = result.checked_mul(10).and_then(|v| v.checked_add(u64::from(byte - b'0')))
            .ok_or("embedded rollback floor overflows")?;
    }
    if result == 0 { return Err("embedded rollback floor is zero"); }
    Ok(result)
}
