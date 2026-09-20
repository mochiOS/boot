#![no_std]
#![deny(unsafe_code)]

/// Two independent copies of this record are required on disk. A writer must
/// update only the older copy, sync it, and re-read it before using the new
/// state. The storage module supplies this protocol through a disk I/O trait.
pub const RECORD_LEN: usize = 32;
pub mod gpt_identity;
pub mod storage;
pub const MAX_TRIAL_BOOTS: u8 = 3;
const MAGIC: [u8; 4] = *b"MBST";
const FORMAT_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Slot {
    A = 1,
    B = 2,
}

impl Slot {
    pub const fn other(self) -> Self {
        match self { Self::A => Self::B, Self::B => Self::A }
    }

    const fn from_byte(value: u8) -> Option<Self> {
        match value { 1 => Some(Self::A), 2 => Some(Self::B), _ => None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootRecord {
    generation: u64,
    active: Slot,
    pending: Option<Slot>,
    attempts_remaining: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionError {
    TrialAlreadyPending,
    WrongSlot,
    NoPendingTrial,
    GenerationExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootDecision {
    Stable { slot: Slot, state_to_persist: Option<BootRecord> },
    Trial { slot: Slot, state_to_persist: BootRecord },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedRecord {
    pub record: BootRecord,
    /// 0 or 1: replace the older/invalid copy, never the only good copy.
    pub write_copy: usize,
}

impl BootRecord {
    pub const fn initial() -> Self {
        Self { generation: 1, active: Slot::A, pending: None, attempts_remaining: 0 }
    }

    pub const fn generation(&self) -> u64 { self.generation }
    pub const fn active(&self) -> Slot { self.active }
    pub const fn pending(&self) -> Option<Slot> { self.pending }
    pub const fn attempts_remaining(&self) -> u8 { self.attempts_remaining }

    /// Call only after the inactive slot has been fully verified and synced.
    pub fn stage(self, target: Slot) -> Result<Self, TransitionError> {
        if self.pending.is_some() { return Err(TransitionError::TrialAlreadyPending); }
        if target == self.active { return Err(TransitionError::WrongSlot); }
        Ok(Self {
            generation: self.next_generation()?, active: self.active,
            pending: Some(target), attempts_remaining: MAX_TRIAL_BOOTS,
        })
    }

    /// The trial record must be durably written before loading its slot.
    /// A failed write means the caller must boot `active`, never `pending`.
    pub fn prepare_boot(self) -> Result<BootDecision, TransitionError> {
        match (self.pending, self.attempts_remaining) {
            (Some(slot), remaining @ 1..=u8::MAX) => Ok(BootDecision::Trial {
                slot,
                state_to_persist: Self {
                    generation: self.next_generation()?,
                    attempts_remaining: remaining - 1,
                    ..self
                },
            }),
            (Some(_), 0) => Ok(BootDecision::Stable {
                slot: self.active,
                state_to_persist: Some(Self {
                    generation: self.next_generation()?, pending: None,
                    attempts_remaining: 0, ..self
                }),
            }),
            (None, _) => Ok(BootDecision::Stable { slot: self.active, state_to_persist: None }),
        }
    }

    /// Only the actual running trial slot may confirm first boot success.
    pub fn confirm(self, booted: Slot) -> Result<Self, TransitionError> {
        if self.pending != Some(booted) || self.attempts_remaining == MAX_TRIAL_BOOTS {
            return Err(TransitionError::NoPendingTrial);
        }
        Ok(Self {
            generation: self.next_generation()?, active: booted,
            pending: None, attempts_remaining: 0,
        })
    }

    pub fn rollback(self) -> Result<Self, TransitionError> {
        if self.pending.is_none() { return Err(TransitionError::NoPendingTrial); }
        Ok(Self {
            generation: self.next_generation()?, pending: None,
            attempts_remaining: 0, ..self
        })
    }

    fn next_generation(self) -> Result<u64, TransitionError> {
        self.generation.checked_add(1).ok_or(TransitionError::GenerationExhausted)
    }

    pub fn encode(self) -> [u8; RECORD_LEN] {
        let mut bytes = [0u8; RECORD_LEN];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4] = FORMAT_VERSION;
        bytes[5] = self.active as u8;
        bytes[6] = self.pending.map_or(0, |slot| slot as u8);
        bytes[7] = self.attempts_remaining;
        bytes[8..16].copy_from_slice(&self.generation.to_le_bytes());
        let checksum = crc32(&bytes[..28]);
        bytes[28..32].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8; RECORD_LEN]) -> Option<Self> {
        if bytes[..4] != MAGIC || bytes[4] != FORMAT_VERSION
            || bytes[16..28].iter().any(|byte| *byte != 0)
            || u32::from_le_bytes(bytes[28..32].try_into().ok()?) != crc32(&bytes[..28])
        { return None; }
        let active = Slot::from_byte(bytes[5])?;
        let pending = if bytes[6] == 0 { None } else { Some(Slot::from_byte(bytes[6])?) };
        let attempts_remaining = bytes[7];
        let generation = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
        if generation == 0 || attempts_remaining > MAX_TRIAL_BOOTS
            || pending == Some(active)
            || (pending.is_none() && attempts_remaining != 0)
        { return None; }
        Some(Self { generation, active, pending, attempts_remaining })
    }
}

/// A mismatched equal-generation pair is ambiguous and must not select a
/// pending slot. The bootloader will need an explicit recovery path for it.
pub fn newest_valid(first: &[u8; RECORD_LEN], second: &[u8; RECORD_LEN]) -> Option<BootRecord> {
    select_for_update(first, second).map(|selection| selection.record)
}

pub fn select_for_update(
    first: &[u8; RECORD_LEN],
    second: &[u8; RECORD_LEN],
) -> Option<SelectedRecord> {
    match (BootRecord::decode(first), BootRecord::decode(second)) {
        (Some(left), Some(right)) if left.generation > right.generation => Some(SelectedRecord { record: left, write_copy: 1 }),
        (Some(left), Some(right)) if right.generation > left.generation => Some(SelectedRecord { record: right, write_copy: 0 }),
        (Some(left), Some(right)) if left == right => Some(SelectedRecord { record: left, write_copy: 0 }),
        (Some(_), Some(_)) => None,
        (Some(left), None) => Some(SelectedRecord { record: left, write_copy: 1 }),
        (None, Some(right)) => Some(SelectedRecord { record: right, write_copy: 0 }),
        (None, None) => None,
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_is_consumed_before_boot_and_unconfirmed_slot_rolls_back() {
        let mut record = BootRecord::initial().stage(Slot::B).unwrap();
        for remaining in (1..=MAX_TRIAL_BOOTS).rev() {
            let BootDecision::Trial { slot, state_to_persist } = record.prepare_boot().unwrap()
                else { panic!("trial slot not selected") };
            assert_eq!(slot, Slot::B);
            assert_eq!(state_to_persist.attempts_remaining(), remaining - 1);
            record = state_to_persist;
        }
        let BootDecision::Stable { slot, state_to_persist: Some(rolled_back) } = record.prepare_boot().unwrap()
            else { panic!("old slot not recovered") };
        assert_eq!(slot, Slot::A);
        assert_eq!(rolled_back.active(), Slot::A);
        assert_eq!(rolled_back.pending(), None);
    }

    #[test]
    fn only_running_trial_slot_can_commit() {
        let pending = BootRecord::initial().stage(Slot::B).unwrap();
        assert_eq!(pending.confirm(Slot::A), Err(TransitionError::NoPendingTrial));
        assert_eq!(pending.confirm(Slot::B), Err(TransitionError::NoPendingTrial));
        let BootDecision::Trial { state_to_persist, .. } = pending.prepare_boot().unwrap()
            else { panic!("trial missing") };
        let committed = state_to_persist.confirm(Slot::B).unwrap();
        assert_eq!(committed.active(), Slot::B);
        assert_eq!(committed.pending(), None);
        assert_eq!(committed.stage(Slot::B), Err(TransitionError::WrongSlot));
        assert_eq!(committed.stage(Slot::A).unwrap().pending(), Some(Slot::A));
    }

    #[test]
    fn torn_write_keeps_old_or_complete_new_record() {
        let old = BootRecord::initial();
        let new = old.stage(Slot::B).unwrap();
        let first = old.encode();
        let updated = new.encode();
        assert_eq!(select_for_update(&first, &updated).unwrap().write_copy, 0);
        assert_eq!(select_for_update(&updated, &first).unwrap().write_copy, 1);
        for written in 0..=RECORD_LEN {
            let mut second = first;
            second[..written].copy_from_slice(&updated[..written]);
            let selected = newest_valid(&first, &second).unwrap();
            assert!(selected == old || selected == new);
        }
        let mut corrupted = updated;
        corrupted[7] ^= 1;
        assert_eq!(newest_valid(&first, &corrupted), Some(old));
        assert_eq!(select_for_update(&first, &corrupted).unwrap().write_copy, 1);
    }

    #[test]
    fn invalid_or_ambiguous_records_never_choose_trial() {
        let old = BootRecord::initial();
        let pending = old.stage(Slot::B).unwrap();
        let mut zero = [0u8; RECORD_LEN];
        assert_eq!(newest_valid(&zero, &pending.encode()), Some(pending));
        zero[..4].copy_from_slice(&MAGIC);
        assert_eq!(BootRecord::decode(&zero), None);
        let mut wrong = pending.encode();
        wrong[5] = Slot::B as u8;
        let checksum = crc32(&wrong[..28]);
        wrong[28..32].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(BootRecord::decode(&wrong), None);
        let conflicting = BootRecord { active: Slot::B, ..old };
        assert_eq!(newest_valid(&old.encode(), &conflicting.encode()), None);
        let exhausted = BootRecord { generation: u64::MAX, ..old };
        assert_eq!(exhausted.stage(Slot::B), Err(TransitionError::GenerationExhausted));
    }
}
