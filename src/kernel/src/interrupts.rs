use x86_64::structures::idt::{InterruptDescriptorTable,InterruptStackFrame};
use crate::gop_println;
use spin::Lazy;

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt
});

pub fn init_idt() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(
    stack_frame: InterruptStackFrame)
    {
        gop_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
    }