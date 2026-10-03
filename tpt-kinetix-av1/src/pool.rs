//! Thread-local pools of zero-filled scratch buffers for per-block work.
//!
//! The inter path used to allocate (and free) a residual or prediction buffer
//! for every transform / prediction block: millions of heap round-trips per
//! decode (`todo-perf.md`, 2026-10-03 profile). A [`Pooled`] derefs to a
//! `Vec<T>` and returns its storage to the calling thread's pool on drop.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};

use crate::Px;

/// Pools hold at most this many buffers per element type per thread.
const MAX_POOLED: usize = 8;

thread_local! {
    static POOL_I32: RefCell<Vec<Vec<i32>>> = const { RefCell::new(Vec::new()) };
    static POOL_PX: RefCell<Vec<Vec<Px>>> = const { RefCell::new(Vec::new()) };
}

/// Element types that have a pool.
pub trait Poolable: Copy + Default + 'static {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<Self>>) -> R) -> R;
}

impl Poolable for i32 {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<i32>>) -> R) -> R {
        POOL_I32.with(|p| f(&mut p.borrow_mut()))
    }
}

impl Poolable for Px {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<Px>>) -> R) -> R {
        POOL_PX.with(|p| f(&mut p.borrow_mut()))
    }
}

/// A zero-filled `Vec<T>` borrowed from the thread-local pool.
pub struct Pooled<T: Poolable>(Vec<T>);

impl<T: Poolable> Pooled<T> {
    /// A buffer of `len` default (zero) elements.
    pub fn zeroed(len: usize) -> Self {
        let mut v = T::with_pool(|p| p.pop()).unwrap_or_default();
        v.clear();
        v.resize(len, T::default());
        Pooled(v)
    }
}

impl<T: Poolable + std::fmt::Debug> std::fmt::Debug for Pooled<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Poolable + PartialEq> PartialEq<Vec<T>> for Pooled<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.0 == *other
    }
}

impl<T: Poolable> Clone for Pooled<T> {
    fn clone(&self) -> Self {
        let mut c = Pooled::zeroed(self.0.len());
        c.0.copy_from_slice(&self.0);
        c
    }
}

impl<T: Poolable> From<Vec<T>> for Pooled<T> {
    fn from(v: Vec<T>) -> Self {
        Pooled(v)
    }
}

impl<T: Poolable> Deref for Pooled<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T: Poolable> DerefMut for Pooled<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.0
    }
}

impl<T: Poolable> Drop for Pooled<T> {
    fn drop(&mut self) {
        let v = std::mem::take(&mut self.0);
        T::with_pool(|p| {
            if p.len() < MAX_POOLED {
                p.push(v);
            }
        });
    }
}
