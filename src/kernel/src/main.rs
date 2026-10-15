#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![feature(alloc_error_handler)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use kernel::boot_info::BootInfo;
use kernel::framebuffer::{self, FramebufferInfo};
use kernel::{gop_println, serial_println};

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start(boot_info: *const BootInfo) -> ! {
    serial_println!("LeafOS kernel entered, boot_info={:p}", boot_info);

    let boot_info: &'static BootInfo = match unsafe { BootInfo::from_ptr(boot_info) } {
        Some(info) => info,
        None => {
            serial_println!("[FATAL] Missing or invalid BootInfo");
            kernel::hlt_loop();
        }
    };

    if let Err(error) = unsafe { kernel::memory::initialize(boot_info) } {
        serial_println!("[FATAL] Memory initialization failed: {:?}", error);
        kernel::hlt_loop();
    }
    serial_println!("[OK] Physical memory manager initialized");

    if let Err(error) = unsafe { kernel::fs::initialize(boot_info) } {
        serial_println!("[FATAL] Initrd validation failed: {:?}", error);
        kernel::hlt_loop();
    }
    serial_println!("[OK] Read-only initrd mounted");

    if let Err(error) = unsafe { kernel::memory::heap::initialize() } {
        serial_println!("[FATAL] Heap initialization failed: {:?}", error);
        kernel::hlt_loop();
    }
    serial_println!("[OK] Kernel heap initialized");

    match kernel::fs::ext2::initialize() {
        Ok(()) => {
            serial_println!("[OK] Read-only ext2 data disk mounted");
        }
        Err(error) => {
            serial_println!("[WARN] ext2 data disk unavailable: {:?}", error);
        }
    }

    let framebuffer = unsafe { FramebufferInfo::from_boot_info(boot_info) };

    match framebuffer {
        Some(fb_info) => {
            serial_println!(
                "Framebuffer {}x{} @ {:#x}",
                fb_info.width,
                fb_info.height,
                fb_info.buffer.addr()
            );
            framebuffer::init(fb_info);
            gop_println!("LEAFOS ALPHA build 2A4b");
            gop_println!("KERNEL BOOT OK");
        }
        None => {
            serial_println!(
                "[WARN] No usable framebuffer -- BootInfo missing/invalid or GOP unavailable"
            );
        }
    }

    kernel::init();

    kernel::shell::run();
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!("--Kernel Panic--");
    serial_println!("{}", info.message());
    if framebuffer::is_initialized() {
        gop_println!("--KERNEL PANIC--");
        gop_println!("{}", info.message());
    }
    kernel::hlt_loop();
}

#[cfg(not(test))]
#[alloc_error_handler]
fn allocation_error(layout: core::alloc::Layout) -> ! {
    serial_println!("[FATAL] Heap allocation failed: {:?}", layout);
    kernel::hlt_loop();
}

#[cfg(test)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start() -> ! {
    test_main();
    loop {}
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info);
}

#[test_case]
fn trivial_assertion() {
    assert_eq!(1, 1);
}
