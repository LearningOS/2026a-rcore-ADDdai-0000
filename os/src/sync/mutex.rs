//! Mutex (spin-like and blocking(sleep))

use super::{DeadlockDetector, UPSafeCell};
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_task, wakeup_task};
use alloc::{collections::VecDeque, sync::Arc};

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex
    fn lock(&self) -> isize;
    /// Unlock the mutex
    fn unlock(&self);
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    locked: UPSafeCell<bool>,
    detector: Arc<UPSafeCell<DeadlockDetector>>,
    resource: usize,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new(detector: Arc<UPSafeCell<DeadlockDetector>>, resource: usize) -> Self {
        Self {
            locked: unsafe { UPSafeCell::new(false) },
            detector,
            resource,
        }
    }
}

impl Mutex for MutexSpin {
    /// Lock the spinlock mutex
    fn lock(&self) -> isize {
        trace!("kernel: MutexSpin::lock");
        let tid = current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid;
        if !self.detector.exclusive_access().request(tid, self.resource) {
            return -0xDEAD;
        }
        loop {
            let mut locked = self.locked.exclusive_access();
            if *locked {
                drop(locked);
                suspend_current_and_run_next();
                continue;
            } else {
                *locked = true;
                self.detector.exclusive_access().acquire(tid, self.resource);
                return 0;
            }
        }
    }

    fn unlock(&self) {
        trace!("kernel: MutexSpin::unlock");
        let mut locked = self.locked.exclusive_access();
        *locked = false;
        let tid = current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid;
        self.detector.exclusive_access().release(tid, self.resource);
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
    detector: Arc<UPSafeCell<DeadlockDetector>>,
    resource: usize,
}

pub struct MutexBlockingInner {
    locked: bool,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new(detector: Arc<UPSafeCell<DeadlockDetector>>, resource: usize) -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            detector,
            resource,
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    locked: false,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }
}

impl Mutex for MutexBlocking {
    /// lock the blocking mutex
    fn lock(&self) -> isize {
        trace!("kernel: MutexBlocking::lock");
        let tid = current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid;
        if !self.detector.exclusive_access().request(tid, self.resource) {
            return -0xDEAD;
        }
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.locked {
            mutex_inner.wait_queue.push_back(current_task().unwrap());
            drop(mutex_inner);
            block_current_and_run_next();
        } else {
            mutex_inner.locked = true;
            self.detector.exclusive_access().acquire(tid, self.resource);
        }
        0
    }

    /// unlock the blocking mutex
    fn unlock(&self) {
        trace!("kernel: MutexBlocking::unlock");
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.locked);
        let tid = current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid;
        let mut detector = self.detector.exclusive_access();
        detector.release(tid, self.resource);
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            let tid = waking_task
                .inner_exclusive_access()
                .res
                .as_ref()
                .unwrap()
                .tid;
            detector.acquire(tid, self.resource);
            wakeup_task(waking_task);
        } else {
            mutex_inner.locked = false;
        }
    }
}
