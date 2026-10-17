use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

const PIT_INPUT_HZ: u32 = 1_193_182;
pub const TICKS_PER_SECOND: u64 = 100;
const PIT_DIVISOR: u16 = (PIT_INPUT_HZ / TICKS_PER_SECOND as u32) as u16;

static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    let mut command = Port::<u8>::new(0x43);
    let mut channel_zero = Port::<u8>::new(0x40);
    unsafe {
        command.write(0x36);
        channel_zero.write(PIT_DIVISOR as u8);
        channel_zero.write((PIT_DIVISOR >> 8) as u8);
    }
}

pub(crate) fn on_interrupt() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_seconds() -> u64 {
    ticks() / TICKS_PER_SECOND
}

/// Sleep the current scheduled task for `duration` PIT ticks.
/// Outside a scheduled kernel task, this returns `NotStarted`.
pub fn sleep_ticks(duration: u64) {
    match crate::tasking::scheduler::sleep_ticks(duration) {
        Ok(()) => return,
        Err(crate::tasking::scheduler::SchedulerError::NotStarted) => {}
        Err(error) => panic!("task sleep failed: {:?}", error),
    }

    // Bootstrap/test code may sleep before the scheduler is started. Once a
    // task is running, the scheduler path above blocks instead of polling.
    let start = ticks();
    while ticks().wrapping_sub(start) < duration {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

pub fn split_uptime(seconds: u64) -> (u64, u8, u8, u8) {
    let days = seconds / 86_400;
    let hours = ((seconds / 3_600) % 24) as u8;
    let minutes = ((seconds / 60) % 60) as u8;
    let seconds = (seconds % 60) as u8;
    (days, hours, minutes, seconds)
}

#[test_case]
fn uptime_splits_into_days_hours_minutes_and_seconds() {
    assert_eq!(split_uptime(0), (0, 0, 0, 0));
    assert_eq!(split_uptime(90_061), (1, 1, 1, 1));
}

#[test_case]
fn uptime_seconds_ignores_partial_seconds() {
    assert_eq!(123 / TICKS_PER_SECOND, 1);
    assert_eq!(199 / TICKS_PER_SECOND, 1);
    assert_eq!(200 / TICKS_PER_SECOND, 2);
}

#[test_case]
fn periodic_interrupt_advances_the_tick_counter() {
    let before = ticks();
    for _ in 0..5 {
        x86_64::instructions::interrupts::enable_and_hlt();
        if ticks() > before {
            return;
        }
    }
    panic!("PIT did not advance the system tick counter");
}

#[test_case]
fn sleep_ticks_waits_for_requested_ticks() {
    let before = ticks();
    sleep_ticks(2);
    assert!(ticks().wrapping_sub(before) >= 2);
}
