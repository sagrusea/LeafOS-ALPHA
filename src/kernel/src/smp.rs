use crate::arch::syscall;
use crate::drivers::{acpi, apic};
use alloc::boxed::Box;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr3;

const TRAMPOLINE_BASE: usize = 0x9000;
const TRAMPOLINE_SIZE: usize = 0x200;
const TRAMPOLINE_CR3: usize = 0x90d0;
const TRAMPOLINE_ENTRY: usize = 0x90d8;
const TRAMPOLINE_STACK: usize = 0x90e0;
const TRAMPOLINE_INDEX: usize = 0x90e8;
const TRAMPOLINE_CR4: usize = 0x90f0;
const STARTUP_VECTOR: u8 = (TRAMPOLINE_BASE >> 12) as u8;
const AP_STACK_SIZE: usize = 32 * 1024;
const AP_DOUBLE_FAULT_STACK_SIZE: usize = 8 * 1024;
const AP_START_TIMEOUT_TICKS: u64 = 10;

static AP_READY: [AtomicU64; syscall::MAX_CPUS] =
    [const { AtomicU64::new(0) }; syscall::MAX_CPUS];
static APIC_IDS: [AtomicU64; syscall::MAX_CPUS] =
    [const { AtomicU64::new(u64::MAX) }; syscall::MAX_CPUS];
static AP_STACK_TOPS: [AtomicU64; syscall::MAX_CPUS] =
    [const { AtomicU64::new(0) }; syscall::MAX_CPUS];
static STARTUP_COMPLETE: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
.section .text.smp_trampoline, "ax"
.global smp_trampoline_start
.global smp_trampoline_end
.code16
smp_trampoline_start:
    cli
    lgdt [0x90a0]
    mov eax, cr0
    or eax, 1
    mov cr0, eax
    .byte 0xea
    .short 0x9015
    .short 0x0008
.code32
smp_trampoline_protected:
    mov ax, 0x0010
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov eax, [0x90f0]
    or eax, 0x20
    mov cr4, eax
    mov eax, [0x90d0]
    mov cr3, eax
    mov ecx, 0xc0000080
    rdmsr
    or eax, 0x900
    wrmsr
    mov eax, cr0
    or eax, 0x80000000
    mov cr0, eax
    .byte 0xea
    .long 0x9053
    .short 0x0018
.code64
smp_trampoline_long:
    mov rsp, qword ptr [0x90e0]
    mov edi, dword ptr [0x90e8]
    mov rax, qword ptr [0x90d8]
    jmp rax
.hang:
    hlt
    jmp .hang
.org 0x00a0
.word 0x001f
.long 0x90b0
.org 0x00b0
.quad 0
.quad 0x00cf9a000000ffff
.quad 0x00cf93000000ffff
.quad 0x00af9a000000ffff
.org 0x00d0
.long 0
.long 0
.long 0
.long 0
.org 0x00f0
.long 0
.long 0
smp_trampoline_end:
"#
);

unsafe extern "C" {
    static smp_trampoline_start: u8;
    static smp_trampoline_end: u8;
}

pub fn start_secondaries() -> usize {
    let Some(processors) = acpi::processor_ids() else {
        return 0;
    };
    let Some(bsp_id) = apic::current_apic_id() else {
        return 0;
    };
    crate::serial_println!(
        "[INFO] SMP topology: BSP APIC ID={}, processors={:?}",
        bsp_id,
        processors
    );
    let mut cpu_index = 1;
    let mut started = 0;
    for &apic_id in processors.iter().filter(|&&id| id != bsp_id) {
        if cpu_index >= syscall::MAX_CPUS {
            break;
        }
        if start_secondary(cpu_index, apic_id) {
            started += 1;
            crate::serial_println!(
                "[OK] Secondary CPU APIC ID={} running scheduler as CPU {}",
                apic_id,
                cpu_index
            );
        } else {
            crate::serial_println!(
                "[WARN] Secondary CPU APIC ID={} failed to start",
                apic_id
            );
        }
        cpu_index += 1;
    }
    STARTUP_COMPLETE.store(1, Ordering::Release);
    wake_secondaries();
    started
}

fn start_secondary(index: usize, apic_id: u32) -> bool {
    let stack = Box::new([0u8; AP_STACK_SIZE]);
    let stack_top = ((stack.as_ptr() as usize + AP_STACK_SIZE) & !0xf) - 8;
    Box::leak(stack);
    AP_STACK_TOPS[index].store(stack_top as u64, Ordering::Release);

    let start = unsafe { &smp_trampoline_start as *const u8 as usize };
    let end = unsafe { &smp_trampoline_end as *const u8 as usize };
    let length = end.saturating_sub(start);
    if length == 0 || length > TRAMPOLINE_SIZE {
        return false;
    }
    let destination = TRAMPOLINE_BASE as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, destination, length);
        crate::serial_println!(
            "[INFO] AP trampoline bytes={:02x} {:02x} {:02x} {:02x}",
            destination.read_volatile(),
            destination.add(1).read_volatile(),
            destination.add(2).read_volatile(),
            destination.add(3).read_volatile()
        );
        let patch = |address: usize| destination.add(address - TRAMPOLINE_BASE);
        patch(TRAMPOLINE_CR3)
            .cast::<u64>()
            .write_unaligned(Cr3::read().0.start_address().as_u64());
        patch(TRAMPOLINE_ENTRY).cast::<u64>().write_unaligned(
            leafos_secondary_entry as *const () as usize as u64,
        );
        patch(TRAMPOLINE_STACK)
            .cast::<u64>()
            .write_unaligned(stack_top as u64);
        patch(TRAMPOLINE_INDEX)
            .cast::<u32>()
            .write_unaligned(index as u32);
        patch(TRAMPOLINE_CR4)
            .cast::<u64>()
            .write_unaligned(x86_64::registers::control::Cr4::read().bits());
    }

    AP_READY[index].store(0, Ordering::Release);
    APIC_IDS[index].store(u64::from(apic_id), Ordering::Release);
    crate::serial_println!("[INFO] Sending INIT/SIPI to APIC ID {}", apic_id);
    if !apic::send_init_ipi(apic_id) {
        crate::serial_println!("[WARN] AP INIT IPI failed for APIC ID {}", apic_id);
        return false;
    }
    if !apic::send_startup_ipi(apic_id, STARTUP_VECTOR) {
        crate::serial_println!("[WARN] AP STARTUP IPI failed for APIC ID {}", apic_id);
        return false;
    }
    crate::serial_println!("[INFO] SIPI sent to APIC ID {}", apic_id);
    if wait_for_ap(index, 1) {
        return true;
    }

    crate::serial_println!("[INFO] Retrying SIPI for APIC ID {}", apic_id);
    if !apic::send_startup_ipi(apic_id, STARTUP_VECTOR) {
        crate::serial_println!("[WARN] AP retry SIPI failed for APIC ID {}", apic_id);
        return false;
    }
    if wait_for_ap(index, AP_START_TIMEOUT_TICKS) {
        true
    } else {
        crate::serial_println!("[WARN] AP startup timed out for APIC ID {}", apic_id);
        false
    }
}

fn wait_for_ap(index: usize, timeout_ticks: u64) -> bool {
    let start = crate::timer::ticks();
    loop {
        if AP_READY[index].load(Ordering::Acquire) != 0 {
            return true;
        }
        if crate::timer::ticks().wrapping_sub(start) >= timeout_ticks {
            return AP_READY[index].load(Ordering::Acquire) != 0;
        }
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

pub fn wake_secondaries() {
    let current_cpu = syscall::current_cpu_index();
    for (cpu_index, apic_id) in APIC_IDS.iter().enumerate().skip(1) {
        if cpu_index == current_cpu
            || AP_READY[cpu_index].load(Ordering::Acquire) != 2
        {
            continue;
        }
        let apic_id = apic_id.load(Ordering::Acquire);
        if apic_id != u64::MAX && !apic::send_reschedule_ipi(apic_id as u32) {
            crate::serial_println!(
                "[WARN] Could not notify secondary CPU {} (APIC ID {})",
                cpu_index,
                apic_id
            );
        }
    }
}

#[inline(never)]
#[unsafe(no_mangle)]
extern "C" fn leafos_secondary_entry(index: u32) -> ! {
    x86_64::instructions::interrupts::disable();
    let index = index as usize;
    crate::serial_println!("[INFO] AP entry reached for CPU {}", index);
    if index == 0 || index >= syscall::MAX_CPUS {
        crate::serial_println!("[FATAL] Invalid secondary CPU index {}", index);
        crate::hlt_loop();
    }
    let stack_top = AP_STACK_TOPS[index].load(Ordering::Acquire);
    if stack_top == 0 {
        crate::serial_println!("[FATAL] Missing secondary CPU stack {}", index);
        crate::hlt_loop();
    }
    let double_fault_stack = Box::leak(Box::new([0u8; AP_DOUBLE_FAULT_STACK_SIZE]));
    let double_fault_stack_top = (double_fault_stack.as_ptr() as usize
        + AP_DOUBLE_FAULT_STACK_SIZE) as u64;
    crate::gdt::init_secondary(
        index,
        x86_64::VirtAddr::new(double_fault_stack_top),
        x86_64::VirtAddr::new(stack_top),
    );
    crate::serial_println!("[INFO] AP {} loaded its GDT/TSS", index);
    crate::arch::interrupts::init_idt();
    crate::serial_println!("[INFO] AP {} loaded the IDT", index);
    if unsafe { syscall::install_cpu_local(index) }.is_err()
        || !apic::initialize_secondary_local()
    {
        crate::serial_println!("[FATAL] Secondary CPU {} initialization failed", index);
        crate::hlt_loop();
    }
    crate::serial_println!("[INFO] AP {} enabled local APIC", index);
    x86_64::instructions::interrupts::enable();
    AP_READY[index].store(2, Ordering::Release);
    crate::serial_println!("[OK] Secondary CPU {} online", index);
    while STARTUP_COMPLETE.load(Ordering::Acquire) == 0 {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
    crate::tasking::scheduler::run_secondary(index)
}
