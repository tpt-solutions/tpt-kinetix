//! Lock helper shared by the live server's state.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks a mutex, recovering the guard when an earlier holder panicked.
///
/// A panic while a lock is held poisons it, and a plain `lock().unwrap()` would
/// then panic in every other task that shares the lock, so one failed stream
/// would take down the whole server. The state behind these locks is updated in
/// small steps and stays usable after a panic, so recovering is the right call.
pub(crate) trait LockRecover<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::LockRecover;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let shared = Arc::new(Mutex::new(1));
        let poisoner = Arc::clone(&shared);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("holder dies with the lock held");
        })
        .join();
        assert!(shared.is_poisoned());
        *shared.lock_recover() += 1;
        assert_eq!(*shared.lock_recover(), 2);
    }
}
