use uefi::table::runtime::{RuntimeServices, VariableAttributes, VariableVendor};
use uefi::{cstr16, guid};

const GLOBAL: VariableVendor = VariableVendor(guid!("8be4df61-93ca-11d2-aa0d-00e098032b8c"));
const REQUIRED_ATTRIBUTES: VariableAttributes = VariableAttributes::BOOTSERVICE_ACCESS
    .union(VariableAttributes::RUNTIME_ACCESS);

pub fn require_enabled(runtime: &RuntimeServices) -> Result<(), &'static str> {
    let secure_boot = read_flag(runtime, cstr16!("SecureBoot"))?;
    let setup_mode = read_flag(runtime, cstr16!("SetupMode"))?;
    if !secure_boot { return Err("UEFI Secure Boot is disabled"); }
    if setup_mode { return Err("UEFI Secure Boot is in Setup Mode"); }
    Ok(())
}

fn read_flag(runtime: &RuntimeServices, name: &uefi::CStr16) -> Result<bool, &'static str> {
    let mut value = [0u8; 1];
    let (value, attributes) = runtime.get_variable(name, &GLOBAL, &mut value)
        .map_err(|_| "UEFI Secure Boot state is unavailable")?;
    if value.len() != 1 || !matches!(value[0], 0 | 1) || !attributes.contains(REQUIRED_ATTRIBUTES) {
        return Err("UEFI Secure Boot state is malformed");
    }
    Ok(value[0] == 1)
}
