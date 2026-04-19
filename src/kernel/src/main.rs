#![no_std]
#![no_main]

use core::panic::PanicInfo;

mod vga_buffer;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    vga_buffer::print_idk();

    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}