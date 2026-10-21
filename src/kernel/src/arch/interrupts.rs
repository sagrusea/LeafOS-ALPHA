use crate::drivers::Pic8259;
use crate::gdt;
use crate::gop_println;
use crate::hlt_loop;
use crate::shell;
use crate::sync::{Lazy, Mutex};
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
    if interrupted_from_user(&stack_frame) {
        unsafe { core::arch::asm!("swapgs", options(nostack, preserves_flags)) };
        crate::serial_println!(
            "[USER FAULT] Task hit breakpoint at {:#x}",
            stack_frame.instruction_pointer.as_u64()
        );
        crate::tasking::scheduler::exit_current();
    }
    gop_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    if interrupted_from_user(&stack_frame) {
        unsafe { core::arch::asm!("swapgs", options(nostack, preserves_flags)) };
        crate::serial_println!(
            "[USER FAULT] Task double fault at {:#x}; terminating task",
            stack_frame.instruction_pointer.as_u64()
        );
        crate::tasking::scheduler::exit_current();
    }
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn timer_interrupt_handler(stack_frame: InterruptStackFrame) {
    let from_user = interrupted_from_user(&stack_frame);
    if from_user {
        unsafe { core::arch::asm!("swapgs", options(nostack, preserves_flags)) };
    }

    crate::timer::on_interrupt();

    unsafe {
        PICS.lock()
            .signal_eoi(InterruptIndex::Timer.as_u8() - PIC_1_OFFSET);
    }

    crate::tasking::scheduler::preempt_from_timer();
    if from_user {
        unsafe { core::arch::asm!("swapgs", options(nostack, preserves_flags)) };
    }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    if interrupted_from_user(&stack_frame) {
        unsafe { core::arch::asm!("swapgs", options(nostack, preserves_flags)) };
        crate::serial_println!(
            "[USER FAULT] Page fault at {:#x}, address {:?}, error {:?}; terminating task",
            stack_frame.instruction_pointer.as_u64(),
            Cr2::read(),
            error_code
        );
        crate::tasking::scheduler::exit_current();
    }

    gop_println!("EXCEPTION: PAGE FAULT");
    gop_println!("Accessed Adress: {:?}", Cr2::read());
    gop_println!("Error Code: {:?}", error_code);
    gop_println!("{:#?}", stack_frame);
    hlt_loop();
}

fn interrupted_from_user(stack_frame: &InterruptStackFrame) -> bool {
    stack_frame.code_segment.0 & 3 == 3
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use crate::drivers::{KeyCode, KeyState, Keyboard};
    use x86_64::instructions::port::Port;

    static KEYBOARD: Mutex<Keyboard> = Mutex::new(Keyboard::new());
    let user_task_running = crate::tasking::scheduler::current_task_is_user();

    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    let mut keyboard = KEYBOARD.lock();

    if let Some(event) = keyboard.process_scancode(scancode) {
        if event.state == KeyState::Pressed {
            if let Some(byte) = event.ascii {
                if matches!(byte, b'\n' | b'\t' | 0x08 | 0x03 | 0x11 | 0x13 | 0x1b)
                    || (0x20..=0x7e).contains(&byte)
                {
                    if user_task_running && byte == 0x03 {
                        unsafe {
                            PICS.lock()
                                .signal_eoi(InterruptIndex::Keyboard.as_u8() - PIC_1_OFFSET);
                        }
                        crate::tasking::scheduler::exit_current_group_with_code(130);
                    } else if user_task_running {
                        crate::drivers::input::enqueue(byte);
                        crate::tasking::scheduler::wake_all_blocked_tasks();
                    } else {
                        shell::enqueue_key(byte);
                    }
                }
            } else {
                let special_key = match event.code {
                    KeyCode::ArrowLeft => Some(0x80),
                    KeyCode::ArrowRight => Some(0x81),
                    KeyCode::ArrowUp => Some(0x82),
                    KeyCode::ArrowDown => Some(0x83),
                    KeyCode::Home => Some(0x84),
                    KeyCode::End => Some(0x85),
                    KeyCode::Delete => Some(0x86),
                    KeyCode::PageUp => Some(0x87),
                    KeyCode::PageDown => Some(0x88),
                    _ => None,
                };
                if let Some(key) = special_key {
                    if user_task_running {
                        crate::drivers::input::enqueue(key);
                    } else {
                        shell::enqueue_key(key);
                    }
                }
            }
        }
    }

    unsafe {
        PICS.lock()
            .signal_eoi(InterruptIndex::Keyboard.as_u8() - PIC_1_OFFSET);
    }
}

pub static PICS: Mutex<Pic8259> = Mutex::new(unsafe { Pic8259::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[test_case]
fn test_breakpoint_exception() {
    // invoke a breakpoint exception
    x86_64::instructions::interrupts::int3();
}
