#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use kernel::boot_info::BootInfo;
use kernel::framebuffer::{self, FramebufferInfo};
use kernel::{gop_print, gop_println, serial_println};


#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start(boot_info: *const BootInfo) -> ! {
    serial_println!("LeafOS kernel entered, boot_info={:p}", boot_info);

    let boot_info = unsafe { BootInfo::from_ptr(boot_info) };
    let framebuffer = boot_info.and_then(|info| unsafe { FramebufferInfo::from_boot_info(info) });

    match framebuffer {
        Some(fb_info) => {
            serial_println!(
                "Framebuffer {}x{} @ {:#x}",
                fb_info.width,
                fb_info.height,
                fb_info.buffer.addr()
            );
            framebuffer::init(fb_info);
            gop_println!("LEAFOS ALPHA build 0A");
            gop_println!("KERNEL BOOT OK");
        }
        None => {
            serial_println!("[WARN] No usable framebuffer -- BootInfo missing/invalid or GOP unavailable");
        }
    }

    kernel::init();

    fn stack_overflow() { 
        stack_overflow();
    }

    stack_overflow();
    gop_print!("NO CRASH!");

    loop {}
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
    loop {}
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
