//! Driver-pinned regions. Only trusted storage owners reach these raw adapters.
use super::*;
use mochios_mdriver_protocol::control::*;
#[derive(Clone, Copy)]
pub(super) struct Region {
    physical: u32,
    token: u32,
    block: BlockState,
}

pub(super) fn block(client: &Client, id: u32) -> Option<BlockState> {
    if id == 0 {
        client.block
    } else {
        client
            .scoped
            .filter(|r| r.block.device_id == id)
            .map(|r| r.block)
    }
}
fn reply(response: MdriverControlResponse, physical: u32) -> Result<MdriverControlResponse, Error> {
    if response.status != MDRIVER_CONTROL_STATUS_OK || response.device_id != physical {
        Err(Error::Device)
    } else {
        Ok(response)
    }
}
pub(super) fn control(
    client: &mut Client,
    op: u16,
    device: u32,
    args: [u64; 4],
) -> Result<MdriverControlResponse, Error> {
    if client.poisoned {
        return Err(Error::Protocol);
    }
    if client.capabilities & MDRIVER_CONTROL_CAP_SCOPED_STORAGE == 0 {
        return Err(Error::Unsupported);
    }
    if op == mnu_abi::STORAGE_CONTROL_CLOSE_REGION {
        if args != [0; 4] {
            return Err(Error::Protocol);
        }
        let r = client
            .scoped
            .filter(|r| r.block.device_id == device)
            .ok_or(Error::Device)?;
        let response = reply(
            transact(
                client,
                MdriverControlRequest::new(
                    MDRIVER_CONTROL_CLOSE_REGION,
                    r.physical,
                    [u64::from(r.token), 0, 0, 0],
                ),
            )?,
            r.physical,
        )?;
        client.scoped = None;
        let _ = response;
        return Ok(MdriverControlResponse::new(op, 0, device, [0; 4]));
    }
    if client.scoped.is_some()
        || args[0] == 0
        || args[2] == 0
        || args[3] != 0
        || args[1].checked_add(args[2]).is_none()
        || !client
            .block_candidates
            .iter()
            .flatten()
            .any(|c| c.device_id == device && c.features & MDRIVER_DEVICE_FEATURE_BLOCK_WRITE != 0)
    {
        return Err(Error::Protocol);
    }
    let address = client.ring_address + MDRIVER_INSTALL_METADATA_PAGE * 4096;
    let reference = if let Some(reference) = client.scoped_reference {
        reference
    } else {
        let reference = hypervisor_guest::invoke(
            HypercallNumber::GrantCreate,
            address,
            u64::from(MDRIVER_DOMAIN_ID),
            GRANT_FLAG_WRITABLE,
        );
        if reference == GRANT_REF_INVALID || reference > u64::from(u32::MAX) {
            return Err(Error::Hypercall);
        }
        client.scoped_reference = Some(reference as u32);
        reference as u32
    };
    let response = reply(
        transact(
            client,
            MdriverControlRequest::new(
                MDRIVER_CONTROL_OPEN_REGION,
                device,
                [args[0], args[1], args[2], u64::from(reference)],
            ),
        )?,
        device,
    )?;
    if response.values[0] == 0
        || response.values[0] > 0x7fffffff
        || response.values[1] != args[2]
        || response.values[2] != 512
        || response.values[3] != args[0]
    {
        client.poisoned = true;
        return Err(Error::Protocol);
    }
    let token = response.values[0] as u32;
    let id = token | 0x80000000;
    client.scoped = Some(Region {
        physical: device,
        token,
        block: BlockState {
            device_id: id,
            capacity_sectors: args[2],
            logical_block_size: 512,
            features: MDRIVER_DEVICE_FEATURE_BLOCK_READ
                | MDRIVER_DEVICE_FEATURE_BLOCK_WRITE
                | MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH,
            data_address: address,
        },
    });
    Ok(MdriverControlResponse::new(
        op,
        0,
        id,
        [args[2], 512, args[0], 0],
    ))
}
pub(super) fn io(client: &mut Client, id: u32, write: bool, lba: u64, length: usize) -> i32 {
    let Some(r) = client.scoped.filter(|r| r.block.device_id == id) else {
        return EINVAL;
    };
    let operation = if write {
        MDRIVER_CONTROL_REGION_WRITE
    } else {
        MDRIVER_CONTROL_REGION_READ
    };
    match transact(
        client,
        MdriverControlRequest::new(
            operation,
            r.physical,
            [u64::from(r.token), lba, length as u64, 0],
        ),
    )
    .and_then(|response| reply(response, r.physical))
    {
        Ok(response) if response.values == [length as u64, 0, 0, 0] => 0,
        _ => EIO,
    }
}
pub(super) fn flush(client: &mut Client, id: u32) -> i32 {
    let Some(r) = client.scoped.filter(|r| r.block.device_id == id) else {
        return EINVAL;
    };
    match transact(
        client,
        MdriverControlRequest::new(
            MDRIVER_CONTROL_REGION_FLUSH,
            r.physical,
            [u64::from(r.token), 0, 0, 0],
        ),
    )
    .and_then(|response| reply(response, r.physical))
    {
        Ok(response) if response.values == [0; 4] => 0,
        _ => EIO,
    }
}
