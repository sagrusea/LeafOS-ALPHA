use x86_64::structures::idt::{InterruptDescriptorTable,InterruptStackFrame};
use crate::drivers::Pic8259;
use crate::gop_print;
use crate::gop_println;
use crate::gdt;
use spin::Lazy;
use spin;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    unsafe {
        idt.double_fault.set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt[InterruptIndex::Timer.as_u8()]
        .set_handler_fn(timer_interrupt_handler);

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

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame, _error_code: u64) -> !{
        panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
    }


extern "x86-interrupt" fn timer_interrupt_handler(
    _stack_frame: InterruptStackFrame,
) {
    gop_print!(".");

    unsafe {
        PICS.lock()
            .signal_eoi(InterruptIndex::Timer.as_u8() - PIC_1_OFFSET);
    }
}


pub static PICS: spin::Mutex<Pic8259> = 
    spin::Mutex::new(unsafe { Pic8259::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[test_case]
fn test_breakpoint_exception() {
    // invoke a breakpoint exception
    x86_64::instructions::interrupts::int3();
}