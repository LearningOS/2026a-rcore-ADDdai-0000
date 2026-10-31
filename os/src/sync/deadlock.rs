//! Resource accounting shared by one process's mutexes or semaphores.
//! Algorithm: https://learningos.cn/rCore-Tutorial-Guide/chapter8/5exercise.html
//! Implemented with OpenAI Codex assistance (allocation, rollback and handoff).
use alloc::{vec, vec::Vec};

/// Per-process allocation and pending-request matrices for one resource family.
#[derive(Default)]
pub struct DeadlockDetector {
    /// Whether new requests are checked before acquiring or blocking.
    pub enabled: bool,
    available: Vec<usize>,
    allocation: Vec<Vec<usize>>,
    need: Vec<Vec<usize>>,
}

impl DeadlockDetector {
    /// Register a resource with its initial available count.
    pub fn add_resource(&mut self, count: usize) -> usize {
        let id = self.available.len();
        self.available.push(count);
        for row in self.allocation.iter_mut().chain(self.need.iter_mut()) {
            row.push(0);
        }
        id
    }

    fn ensure_thread(&mut self, tid: usize) {
        while self.need.len() <= tid {
            self.need.push(vec![0; self.available.len()]);
            self.allocation.push(vec![0; self.available.len()]);
        }
    }

    // Keep accounting even when detection is disabled, so enabling it later works.
    /// Record one request, rolling it back if it would cause deadlock.
    pub fn request(&mut self, tid: usize, resource: usize) -> bool {
        self.ensure_thread(tid);
        self.need[tid][resource] += 1;
        if self.enabled && !self.is_safe() {
            self.need[tid][resource] -= 1;
            false
        } else {
            true
        }
    }

    /// Convert an accepted request into an allocated resource.
    pub fn acquire(&mut self, tid: usize, resource: usize) {
        self.available[resource] -= 1;
        self.need[tid][resource] -= 1;
        self.allocation[tid][resource] += 1;
    }

    /// Return a permit, including a semaphore signal from a non-holder.
    pub fn release(&mut self, tid: usize, resource: usize) {
        self.ensure_thread(tid);
        // Semaphore up may be performed by a producer which never called down.
        self.allocation[tid][resource] = self.allocation[tid][resource].saturating_sub(1);
        self.available[resource] += 1;
    }

    /// Remove a retiring thread from the simulation without unlocking resources.
    pub fn forget_thread(&mut self, tid: usize) {
        if tid < self.need.len() {
            self.need[tid].fill(0);
            // Exiting does not unlock resources; do not increase available.
            self.allocation[tid].fill(0);
        }
    }

    fn is_safe(&self) -> bool {
        let mut work = self.available.clone();
        let mut finish = vec![false; self.need.len()];
        loop {
            let mut progress = false;
            for i in 0..finish.len() {
                if !finish[i] && self.need[i].iter().zip(&work).all(|(n, w)| n <= w) {
                    for (w, held) in work.iter_mut().zip(&self.allocation[i]) {
                        *w += held;
                    }
                    finish[i] = true;
                    progress = true;
                }
            }
            if finish.iter().all(|done| *done) {
                return true;
            }
            if !progress {
                return false;
            }
        }
    }
}
