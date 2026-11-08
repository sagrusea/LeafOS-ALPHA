#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use core::panic::PanicInfo;
use x86::io::{outl, outw};

pub mod arch;
pub mod boot_info;
pub mod console;
pub mod drivers;
pub mod fs;
pub mod gui;
pub mod memory;
pub mod misc;
pub mod shell;
pub mod smp;
pub mod sync;
pub mod tasking;
pub mod time;
pub mod user_program;

pub use arch::{gdt, interrupts};
pub use console::{framebuffer, serial, vga_buffer};
pub use tasking::scheduler;
pub use time::timer;

pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) -> () {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    #[cfg(test)]
    tasking::scheduler::run_smp_work_stealing_test();
    exit_qemu(QemuExitCode::Success);
}

pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
    hlt_loop();
}

#[cfg(test)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start(boot_info: *const boot_info::BootInfo) -> ! {
    let boot_info = unsafe { boot_info::BootInfo::from_ptr(boot_info) }
        .expect("test boot requires valid BootInfo");
    unsafe { memory::initialize(boot_info) }.expect("initialize physical memory manager");
    drivers::acpi::initialize(boot_info.acpi_rsdp, boot_info.physical_memory_offset);
    unsafe { memory::heap::initialize() }.expect("initialize kernel heap");
    init();
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) {
    unsafe {
        outl(0xf4, exit_code as u32);
    }
}

pub fn shutdown() -> ! {
    x86_64::instructions::interrupts::disable();
    serial_println!("[INFO] Requesting ACPI S5 shutdown");
    unsafe {
        outw(0x604, 0x3400);
        outw(0xb004, 0x2000);
        outw(0x4004, 0x3400);
    }
    hlt_loop()
}

pub fn init() {
    gdt::init();
    interrupts::init_idt();
    let apic_enabled = drivers::apic::initialize();
    if apic_enabled {
        unsafe { interrupts::PICS.lock().disable() };
        serial_println!("[OK] Local APIC and I/O APIC enabled; legacy PIC masked");
        if let Some(apic_id) = drivers::apic::current_apic_id() {
            let cpu_index = drivers::acpi::processor_index(apic_id).unwrap_or(0);
            serial_println!(
                "[OK] Bootstrap CPU APIC ID={} topology index={}",
                apic_id,
                cpu_index
            );
        }
    } else {
        unsafe { interrupts::PICS.lock().initialize() };
        serial_println!("[WARN] APIC unavailable; using legacy PIC");
    }
    timer::init();
    gui::initialize();
    drivers::mouse::initialize();
    if !apic_enabled {
        unsafe { interrupts::PICS.lock().enable_irq(12) };
    }
    x86_64::instructions::interrupts::enable();
}

pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
