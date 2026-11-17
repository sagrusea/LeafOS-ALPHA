use core::ops::{Deref, DerefMut};
use spin::Mutex as SpinMutex;

/// A spin mutex that prevents local interrupt preemption while it is held.
pub struct Mutex<T: ?Sized> {
    inner: SpinMutex<T>,
}

/// A mutex guard that restores the prior interrupt state after unlocking.
pub struct MutexGuard<'a, T: ?Sized> {
    guard: Option<spin::MutexGuard<'a, T>>,
    _interrupt_state: InterruptState,
}

/// A one-time initializer that cannot be interrupted while initializing.
pub struct Once<T> {
    inner: spin::Once<T>,
}

/// A lazily initialized value whose initializer runs with interrupts disabled.
pub struct Lazy<T, F = fn() -> T> {
    inner: spin::Lazy<T, F>,
}

struct InterruptState {
    was_enabled: bool,
}

impl InterruptState {
    fn disable() -> Self {
        let was_enabled = x86_64::instructions::interrupts::are_enabled();
        if was_enabled {
            x86_64::instructions::interrupts::disable();
        }
        Self { was_enabled }
    }
}

impl Drop for InterruptState {
    fn drop(&mut self) {
        if self.was_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: SpinMutex::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let interrupt_state = InterruptState::disable();
        let guard = self.inner.lock();
        MutexGuard {
            guard: Some(guard),
            _interrupt_state: interrupt_state,
        }
    }
}

impl<T> Once<T> {
    pub const fn new() -> Self {
        Self {
            inner: spin::Once::new(),
        }
    }

    pub fn call_once<F>(&self, initialize: F) -> &T
    where
        F: FnOnce() -> T,
    {
        let interrupt_state = InterruptState::disable();
        let value = self.inner.call_once(initialize);
        drop(interrupt_state);
        value
    }

    pub fn get(&self) -> Option<&T> {
        let interrupt_state = InterruptState::disable();
        let value = self.inner.get();
        drop(interrupt_state);
        value
    }
}

impl<T, F> Lazy<T, F>
where
    F: FnOnce() -> T,
{
    pub const fn new(initialize: F) -> Self {
        Self {
            inner: spin::Lazy::new(initialize),
        }
    }
}

impl<T, F> Deref for Lazy<T, F>
where
    F: FnOnce() -> T,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        let interrupt_state = InterruptState::disable();
        let value = &*self.inner;
        drop(interrupt_state);
        value
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.guard.as_deref().expect("mutex guard already released")
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_deref_mut()
            .expect("mutex guard already released")
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
    }
}
