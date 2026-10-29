//! Strict durability acknowledgement for the mDriver control transport.
//! This does not grant disk access; the caller must already own the device.
use mochios_mdriver_protocol::control::{
    MdriverControlRequest, MdriverControlResponse, MDRIVER_CONTROL_BLOCK_FLUSH,
    MDRIVER_CONTROL_STATUS_OK, MDRIVER_CONTROL_VERSION, MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH,
};

#[derive(Debug, PartialEq, Eq)]
pub enum FlushError<E> {
    Unsupported,
    InvalidResponse,
    Transport(E),
}

/// Success requires an advertised flush and a matching positive acknowledgement.
/// An absent feature must never be interpreted as an already-durable device.
pub fn flush_with<E>(
    device_id: u32,
    features: u64,
    transact: impl FnOnce(MdriverControlRequest) -> Result<MdriverControlResponse, E>,
) -> Result<(), FlushError<E>> {
    if features & MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH == 0 {
        return Err(FlushError::Unsupported);
    }
    let response = transact(MdriverControlRequest::new(
        MDRIVER_CONTROL_BLOCK_FLUSH,
        device_id,
        [0; 4],
    ))
    .map_err(FlushError::Transport)?;
    if response.version != MDRIVER_CONTROL_VERSION
        || response.operation != MDRIVER_CONTROL_BLOCK_FLUSH
        || response.device_id != device_id
        || response.reserved != 0
        || response.status != MDRIVER_CONTROL_STATUS_OK
    {
        return Err(FlushError::InvalidResponse);
    }
    Ok(())
}
