// Compile the production adapter, rather than a test copy of the decision.
#[path = "../../../src/storage_flush.rs"]
mod storage_flush;

use mochios_mdriver_protocol::control::*;
use storage_flush::{flush_with, FlushError};

fn success() -> MdriverControlResponse {
    MdriverControlResponse::new(
        MDRIVER_CONTROL_BLOCK_FLUSH,
        MDRIVER_CONTROL_STATUS_OK,
        7,
        [0; 4],
    )
}

#[test]
fn unsupported_flush_never_invokes_the_transport_or_reports_success() {
    for features in [
        0,
        MDRIVER_DEVICE_FEATURE_BLOCK_READ,
        MDRIVER_DEVICE_FEATURE_BLOCK_WRITE,
    ] {
        let result: Result<(), FlushError<()>> = flush_with(7, features, |_| {
            panic!("unsupported operation reached the device")
        });
        assert_eq!(result, Err(FlushError::Unsupported));
    }
}

#[test]
fn supported_flush_waits_for_the_exact_requested_device() {
    let mut calls = 0;
    let result: Result<(), FlushError<()>> =
        flush_with(7, MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH, |request| {
            calls += 1;
            assert_eq!(
                request,
                MdriverControlRequest::new(MDRIVER_CONTROL_BLOCK_FLUSH, 7, [0; 4])
            );
            Ok(success())
        });
    assert_eq!(result, Ok(()));
    assert_eq!(calls, 1);
}

#[test]
fn stale_malformed_or_failed_acknowledgements_never_confirm_durability() {
    for mode in 0..6 {
        let mut reply = success();
        match mode {
            0 => reply.version += 1,
            1 => reply.operation = MDRIVER_CONTROL_BLOCK_READ,
            2 => reply.device_id += 1,
            3 => reply.reserved = 1,
            4 => reply.status = MDRIVER_CONTROL_STATUS_IO_ERROR,
            _ => reply.status = MDRIVER_CONTROL_STATUS_UNSUPPORTED_OPERATION,
        }
        let result: Result<(), FlushError<()>> =
            flush_with(7, MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH, |_| Ok(reply));
        assert_eq!(result, Err(FlushError::InvalidResponse));
    }
}

#[test]
fn transport_failure_is_not_a_durability_acknowledgement() {
    assert_eq!(
        flush_with(7, MDRIVER_DEVICE_FEATURE_BLOCK_FLUSH, |_| Err::<
            MdriverControlResponse,
            _,
        >(
            "timeout"
        )),
        Err(FlushError::Transport("timeout")),
    );
}
