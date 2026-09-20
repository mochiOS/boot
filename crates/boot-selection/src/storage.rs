//! Crash-safe two-copy record protocol. The backend must place each copy in
//! its own failure domain and make `sync` durable before returning.

use crate::{BootDecision, BootRecord, RECORD_LEN, SelectedRecord, Slot, TransitionError, select_for_update};

pub trait RecordIo {
    type Error;

    fn read_copy(&mut self, index: usize) -> Result<[u8; RECORD_LEN], Self::Error>;
    fn write_copy(&mut self, index: usize, bytes: &[u8; RECORD_LEN]) -> Result<(), Self::Error>;
    fn sync(&mut self) -> Result<(), Self::Error>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError<E> {
    Io(E),
    NoValidRecord,
    NotBlank,
    ReadBackMismatch,
    Transition(TransitionError),
}

pub fn load<IO: RecordIo>(io: &mut IO) -> Result<SelectedRecord, StoreError<IO::Error>> {
    let first = io.read_copy(0).map_err(StoreError::Io)?;
    let second = io.read_copy(1).map_err(StoreError::Io)?;
    select_for_update(&first, &second).ok_or(StoreError::NoValidRecord)
}

fn write_and_verify<IO: RecordIo>(
    io: &mut IO,
    index: usize,
    record: BootRecord,
) -> Result<(), StoreError<IO::Error>> {
    let expected = record.encode();
    io.write_copy(index, &expected).map_err(StoreError::Io)?;
    io.sync().map_err(StoreError::Io)?;
    let actual = io.read_copy(index).map_err(StoreError::Io)?;
    if actual != expected { return Err(StoreError::ReadBackMismatch); }
    Ok(())
}

fn commit<IO: RecordIo>(
    io: &mut IO,
    selected: SelectedRecord,
    next: BootRecord,
) -> Result<BootRecord, StoreError<IO::Error>> {
    write_and_verify(io, selected.write_copy, next)?;
    // Also catch a backend which accidentally overwrote the other copy.
    if load(io)?.record != next { return Err(StoreError::ReadBackMismatch); }
    Ok(next)
}

/// For image provisioning only. Never silently initialize a damaged device.
/// A failure after the first copy leaves one valid initial record.
pub fn initialize_blank<IO: RecordIo>(io: &mut IO) -> Result<(), StoreError<IO::Error>> {
    let first = io.read_copy(0).map_err(StoreError::Io)?;
    let second = io.read_copy(1).map_err(StoreError::Io)?;
    if first != [0; RECORD_LEN] || second != [0; RECORD_LEN] {
        return Err(StoreError::NotBlank);
    }
    let initial = BootRecord::initial();
    write_and_verify(io, 0, initial)?;
    write_and_verify(io, 1, initial)?;
    Ok(())
}

/// Caller must have verified and synced the inactive slot before this call.
pub fn stage<IO: RecordIo>(io: &mut IO, target: Slot) -> Result<BootRecord, StoreError<IO::Error>> {
    let selected = load(io)?;
    let next = selected.record.stage(target).map_err(StoreError::Transition)?;
    commit(io, selected, next)
}

/// Never boot a trial slot if this returns an error. The safe fallback is the
/// `active` slot from the last readable valid record.
pub fn prepare_boot<IO: RecordIo>(io: &mut IO) -> Result<Slot, StoreError<IO::Error>> {
    let selected = load(io)?;
    match selected.record.prepare_boot().map_err(StoreError::Transition)? {
        BootDecision::Trial { slot, state_to_persist } => {
            commit(io, selected, state_to_persist)?;
            Ok(slot)
        }
        BootDecision::Stable { slot, state_to_persist: Some(next) } => {
            commit(io, selected, next)?;
            Ok(slot)
        }
        BootDecision::Stable { slot, state_to_persist: None } => Ok(slot),
    }
}

pub fn confirm<IO: RecordIo>(io: &mut IO, booted: Slot) -> Result<BootRecord, StoreError<IO::Error>> {
    let selected = load(io)?;
    let next = selected.record.confirm(booted).map_err(StoreError::Transition)?;
    commit(io, selected, next)
}

pub fn rollback<IO: RecordIo>(io: &mut IO) -> Result<BootRecord, StoreError<IO::Error>> {
    let selected = load(io)?;
    let next = selected.record.rollback().map_err(StoreError::Transition)?;
    commit(io, selected, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fault { Write, Sync }

    struct MemoryIo {
        copies: [[u8; RECORD_LEN]; 2],
        fail_write_after: Option<usize>,
        fail_sync: bool,
        corrupt_readback: bool,
        wrote: bool,
    }

    impl MemoryIo {
        fn blank() -> Self {
            Self { copies: [[0; RECORD_LEN]; 2], fail_write_after: None,
                fail_sync: false, corrupt_readback: false, wrote: false }
        }
        fn initialized() -> Self {
            let mut io = Self::blank();
            initialize_blank(&mut io).unwrap();
            io.wrote = false;
            io
        }
    }

    impl RecordIo for MemoryIo {
        type Error = Fault;

        fn read_copy(&mut self, index: usize) -> Result<[u8; RECORD_LEN], Self::Error> {
            let mut bytes = self.copies[index];
            if self.wrote && self.corrupt_readback { bytes[0] ^= 1; }
            Ok(bytes)
        }
        fn write_copy(&mut self, index: usize, bytes: &[u8; RECORD_LEN]) -> Result<(), Self::Error> {
            self.wrote = true;
            if let Some(count) = self.fail_write_after {
                self.copies[index][..count].copy_from_slice(&bytes[..count]);
                return Err(Fault::Write);
            }
            self.copies[index] = *bytes;
            Ok(())
        }
        fn sync(&mut self) -> Result<(), Self::Error> {
            if self.fail_sync { Err(Fault::Sync) } else { Ok(()) }
        }
    }

    #[test]
    fn stages_trials_confirms_and_rolls_back_through_storage() {
        let mut io = MemoryIo::initialized();
        assert_eq!(prepare_boot(&mut io), Ok(Slot::A));
        assert_eq!(stage(&mut io, Slot::B).unwrap().pending(), Some(Slot::B));
        assert_eq!(confirm(&mut io, Slot::B), Err(StoreError::Transition(TransitionError::NoPendingTrial)));
        assert_eq!(prepare_boot(&mut io), Ok(Slot::B));
        assert_eq!(confirm(&mut io, Slot::B).unwrap().active(), Slot::B);
        assert_eq!(prepare_boot(&mut io), Ok(Slot::B));
        stage(&mut io, Slot::A).unwrap();
        for _ in 0..crate::MAX_TRIAL_BOOTS { assert_eq!(prepare_boot(&mut io), Ok(Slot::A)); }
        assert_eq!(prepare_boot(&mut io), Ok(Slot::B));
        assert_eq!(load(&mut io).unwrap().record.pending(), None);
    }

    #[test]
    fn every_partial_write_preserves_a_valid_old_or_new_record() {
        for count in 0..=RECORD_LEN {
            let mut io = MemoryIo::initialized();
            io.fail_write_after = Some(count);
            assert_eq!(stage(&mut io, Slot::B), Err(StoreError::Io(Fault::Write)));
            let selected = load(&mut io).unwrap().record;
            assert_eq!(selected.active(), Slot::A);
            assert!(selected.pending().is_none() || selected.pending() == Some(Slot::B));
        }
    }

    #[test]
    fn failed_sync_or_readback_never_authorizes_trial_boot() {
        let mut io = MemoryIo::initialized();
        stage(&mut io, Slot::B).unwrap();
        io.fail_sync = true;
        assert_eq!(prepare_boot(&mut io), Err(StoreError::Io(Fault::Sync)));
        let mut io = MemoryIo::initialized();
        io.corrupt_readback = true;
        assert_eq!(stage(&mut io, Slot::B), Err(StoreError::ReadBackMismatch));
    }

    fn attempted_b() -> MemoryIo {
        let mut io = MemoryIo::initialized();
        stage(&mut io, Slot::B).unwrap();
        assert_eq!(prepare_boot(&mut io), Ok(Slot::B));
        assert_eq!(load(&mut io).unwrap().record.attempts_remaining(), crate::MAX_TRIAL_BOOTS - 1);
        io.wrote = false;
        io
    }

    fn assert_safe_after_uncertain_confirmation(io: &mut MemoryIo) {
        let record = load(io).expect("at least one valid boot-state copy must remain").record;
        match (record.active(), record.pending()) {
            (Slot::A, Some(Slot::B)) => {
                assert_eq!(record.attempts_remaining(), crate::MAX_TRIAL_BOOTS - 1);
            }
            (Slot::B, None) => assert_eq!(record.attempts_remaining(), 0),
            other => panic!("unexpected state after interrupted B confirmation: {other:?}"),
        }
    }

    #[test]
    fn interrupted_confirmation_keeps_a_valid_trial_or_stable_b() {
        for count in 0..=RECORD_LEN {
            let mut io = attempted_b();
            io.fail_write_after = Some(count);
            assert_eq!(confirm(&mut io, Slot::B), Err(StoreError::Io(Fault::Write)));
            assert_safe_after_uncertain_confirmation(&mut io);
        }
    }

    #[test]
    fn failed_confirmation_sync_and_readback_leave_a_valid_record() {
        let mut io = attempted_b();
        io.fail_sync = true;
        assert_eq!(confirm(&mut io, Slot::B), Err(StoreError::Io(Fault::Sync)));
        assert_safe_after_uncertain_confirmation(&mut io);

        let mut io = attempted_b();
        io.corrupt_readback = true;
        assert_eq!(confirm(&mut io, Slot::B), Err(StoreError::ReadBackMismatch));
        io.corrupt_readback = false;
        assert_safe_after_uncertain_confirmation(&mut io);
    }

    #[test]
    fn initialization_requires_blank_copies_and_never_repairs_implicitly() {
        let mut io = MemoryIo::initialized();
        assert_eq!(initialize_blank(&mut io), Err(StoreError::NotBlank));
        io.copies = [[0xff; RECORD_LEN]; 2];
        assert_eq!(load(&mut io), Err(StoreError::NoValidRecord));
    }
}
