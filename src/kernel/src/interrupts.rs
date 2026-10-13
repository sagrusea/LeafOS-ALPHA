use crate::drivers::Pic8259;
use crate::gdt;
use crate::gop_println;
use crate::hlt_loop;
use crate::shell;
use spin;
use spin::Lazy;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
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
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
    idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt
});

pub fn init_idt() {
    IDT.load();
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    gop_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    //gop_print!(".");

    unsafe {
        PICS.lock()
            .signal_eoi(InterruptIndex::Timer.as_u8() - PIC_1_OFFSET);
    }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    gop_println!("EXCEPTION: PAGE FAULT");
    gop_println!("Accessed Adress: {:?}", Cr2::read());
    gop_println!("Error Code: {:?}", error_code);
    gop_println!("{:#?}", stack_frame);
    hlt_loop();
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use crate::drivers::{KeyState, Keyboard};
    use x86_64::instructions::port::Port;

    static KEYBOARD: Mutex<Keyboard> = Mutex::new(Keyboard::new());

    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    let mut keyboard = KEYBOARD.lock();

    if let Some(event) = keyboard.process_scancode(scancode) {
        if event.state == KeyState::Pressed {
            if let Some(byte) = event.ascii {
                if byte == b'\n' || byte == 0x08 || byte == 0x03 || (0x20..=0x7e).contains(&byte) {
                    shell::enqueue_key(byte);
                }
            }
        }
    }

    unsafe {
        PICS.lock()
            .signal_eoi(InterruptIndex::Keyboard.as_u8() - PIC_1_OFFSET);
    }
}

pub static PICS: spin::Mutex<Pic8259> =
    spin::Mutex::new(unsafe { Pic8259::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[test_case]
fn test_breakpoint_exception() {
    // invoke a breakpoint exception
    x86_64::instructions::interrupts::int3();
}
