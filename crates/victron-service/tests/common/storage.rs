//! In-memory storage fake honouring the atomic acquisition-commit and
//! spool ownership contracts.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use victron_service::*;

pub struct Record {
    pub id: u64,
    pub payload: Vec<u8>,
    pub attempts: u32,
    pub created_at: SystemTime,
    pub next_attempt_at: Option<SystemTime>,
    pub claim_deadline: Option<SystemTime>,
}

pub struct FakeStorage {
    pub records: Vec<Record>,
    pub next_id: u64,
    pub last_success: Option<SystemTime>,
    pub energy: Option<EnergyState>,
    pub enqueues: u64,
    pub delivered: u64,
    pub dropped: u64,
    /// Adapter-side retry deadline offset (deterministic fake backoff).
    pub retry_delay: Duration,
    /// Adapter-side attempt budget (mirrors the storage config).
    pub max_attempts: u32,
    /// Failure injection for `commit_acquisition` (all-or-nothing: an
    /// injected error applies nothing).
    pub commit_script: VecDeque<Result<AcquisitionCommitOutcome, StorageError>>,
}

impl FakeStorage {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            next_id: 1,
            last_success: None,
            energy: None,
            enqueues: 0,
            delivered: 0,
            dropped: 0,
            retry_delay: Duration::from_secs(30),
            max_attempts: 5,
            commit_script: VecDeque::new(),
        }
    }

    pub fn enqueues(&self) -> u64 {
        self.enqueues
    }

    pub fn spool_depth(&self) -> usize {
        self.records.len()
    }

    /// Simulate a crash after claiming: force the claim deadline into the past.
    pub fn expire_claims(&mut self, now: SystemTime) {
        for r in &mut self.records {
            if r.claim_deadline.is_some() {
                r.claim_deadline = Some(now - Duration::from_secs(1));
            }
        }
    }
}

pub struct SharedStorage(pub Arc<Mutex<FakeStorage>>);

#[async_trait]
impl StoragePort for SharedStorage {
    async fn last_success(&self) -> Result<Option<SystemTime>, StorageError> {
        Ok(self.0.lock().unwrap().last_success)
    }

    async fn energy_state(&self) -> Result<Option<EnergyState>, StorageError> {
        Ok(self.0.lock().unwrap().energy.clone())
    }

    async fn commit_acquisition(
        &mut self,
        commit: AcquisitionCommit,
    ) -> Result<AcquisitionCommitOutcome, StorageError> {
        let mut s = self.0.lock().unwrap();
        // Injected failure: nothing is applied (all-or-nothing).
        if let Some(behavior) = s.commit_script.pop_front() {
            return behavior;
        }
        // Idempotency: the same observed_at is a no-op.
        if let Some(last) = s.last_success {
            if commit.observed_at <= last {
                return Ok(AcquisitionCommitOutcome::AlreadyCommitted);
            }
        }
        // Optimistic anchor: a concurrent modification is a typed conflict.
        if s.energy != commit.expected_energy {
            return Err(StorageError::EnergyAnchorConflict);
        }
        // Apply atomically: energy, identity, batch. The batch enters the
        // spool as attempt 0; claiming increments it (attempt 1 on first
        // claim), matching victron-storage.
        s.energy = Some(commit.next_energy);
        s.last_success = Some(commit.observed_at);
        let id = s.next_id;
        s.records.push(Record {
            id,
            payload: commit.payload,
            attempts: 0,
            created_at: commit.observed_at,
            next_attempt_at: None,
            claim_deadline: None,
        });
        s.next_id += 1;
        s.enqueues += 1;
        Ok(AcquisitionCommitOutcome::Committed)
    }

    async fn spool_health(&self, now: SystemTime) -> Result<SpoolHealth, StorageError> {
        let s = self.0.lock().unwrap();
        let oldest_age = s
            .records
            .first()
            .map(|r| now.duration_since(r.created_at).unwrap_or(Duration::ZERO));
        Ok(SpoolHealth {
            depth: s.records.len(),
            oldest_age,
        })
    }

    async fn spool_claim_next(
        &mut self,
        claim_ttl: Duration,
        now: SystemTime,
    ) -> Result<Option<ClaimedBatch>, StorageError> {
        let mut s = self.0.lock().unwrap();
        let idx = s.records.iter().position(|r| {
            let due = match r.next_attempt_at {
                Some(deadline) => deadline <= now,
                None => true,
            };
            let claimable = match r.claim_deadline {
                Some(deadline) => deadline <= now,
                None => true,
            };
            due && claimable
        });
        Ok(idx.map(|i| {
            let r = &mut s.records[i];
            // Claiming increments the stored attempt counter: the returned
            // attempts is the 1-based attempt of the current delivery.
            r.attempts += 1;
            r.claim_deadline = Some(now + claim_ttl);
            ClaimedBatch {
                id: r.id,
                payload: r.payload.clone(),
                attempts: r.attempts,
            }
        }))
    }

    async fn spool_complete(&mut self, claim: &ClaimedBatch) -> Result<(), StorageError> {
        let mut s = self.0.lock().unwrap();
        let pos = s
            .records
            .iter()
            .position(|r| r.id == claim.id && r.claim_deadline.is_some());
        match pos {
            Some(i) => {
                s.records.remove(i);
                s.delivered += 1;
                Ok(())
            }
            None => Err(StorageError::Corrupt),
        }
    }

    async fn spool_retry(
        &mut self,
        claim: &ClaimedBatch,
        now: SystemTime,
    ) -> Result<RetryOutcome, StorageError> {
        let mut s = self.0.lock().unwrap();
        let pos = s
            .records
            .iter()
            .position(|r| r.id == claim.id && r.claim_deadline.is_some())
            .ok_or(StorageError::Corrupt)?;
        let attempts = s.records[pos].attempts;
        if attempts >= s.max_attempts {
            s.records.remove(pos);
            s.dropped += 1;
            return Ok(RetryOutcome::Dropped { attempts });
        }
        s.records[pos].next_attempt_at = Some(now + s.retry_delay);
        s.records[pos].claim_deadline = None;
        Ok(RetryOutcome::Retried { attempts })
    }

    async fn spool_drop(&mut self, claim: &ClaimedBatch) -> Result<(), StorageError> {
        let mut s = self.0.lock().unwrap();
        let pos = s
            .records
            .iter()
            .position(|r| r.id == claim.id && r.claim_deadline.is_some())
            .ok_or(StorageError::Corrupt)?;
        s.records.remove(pos);
        s.dropped += 1;
        Ok(())
    }
}
