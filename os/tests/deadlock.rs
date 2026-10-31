extern crate alloc;
#[path = "../src/sync/deadlock.rs"]
mod deadlock;
use deadlock::DeadlockDetector;

#[test]
fn rescans_when_later_thread_can_release_resource() {
    let mut d = DeadlockDetector::default();
    let a = d.add_resource(1);
    let b = d.add_resource(1);
    d.enabled = true;
    assert!(d.request(0, a));
    d.acquire(0, a);
    assert!(d.request(1, b));
    d.acquire(1, b);
    assert!(d.request(0, b)); // thread 1 can finish, then thread 0
    assert!(!d.request(1, a)); // circular wait is rejected
    d.release(1, b);
    d.acquire(0, b);
    d.release(0, a);
    d.release(0, b);
    assert!(d.request(1, a)); // rejected request was rolled back
    d.acquire(1, a);
}

#[test]
fn tracks_acquisitions_before_detection_is_enabled() {
    let mut d = DeadlockDetector::default();
    let lock = d.add_resource(1);
    assert!(d.request(3, lock));
    d.acquire(3, lock);
    d.enabled = true;
    assert!(!d.request(3, lock));
    d.release(3, lock);
    assert!(d.request(3, lock));
    d.acquire(3, lock);
}

#[test]
fn wakeup_reserves_resource_before_thread_runs() {
    let mut d = DeadlockDetector::default();
    let sem = d.add_resource(1);
    assert!(d.request(0, sem));
    d.acquire(0, sem);
    assert!(d.request(1, sem));
    d.release(0, sem);
    d.acquire(1, sem); // ownership handed to waiter
    d.enabled = true;
    assert!(!d.request(1, sem));
    d.release(1, sem);
    assert!(d.request(0, sem));
}

#[test]
fn zero_semaphore_accepts_signal_from_non_holder() {
    let mut d = DeadlockDetector::default();
    let sem = d.add_resource(0);
    assert!(d.request(1, sem));
    d.release(0, sem);
    d.acquire(1, sem);
    d.enabled = true;
    assert!(!d.request(1, sem));
    d.forget_thread(1); // reused TID must not inherit holdings
    d.release(0, sem);
    assert!(d.request(1, sem));
    d.acquire(1, sem);
}
