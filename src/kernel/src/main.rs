#![no_std]
#![no_main]

use core::panic::PanicInfo;

mod vga_buffer;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("Welcome to LeafOS $@<^~)v:{}", 0.1);
    panic!("help");
    loop {}
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("--Kernel Panic--");
    
    loop {}
}