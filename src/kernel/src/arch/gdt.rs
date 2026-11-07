use crate::sync::{Lazy, Mutex};
use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::VirtAddr;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const USER_DATA_SELECTOR: u16 = 0x2b;
pub const USER_CODE_SELECTOR: u16 = 0x33;
const STACK_SIZE: usize = 64 * 1024;
const MAX_SECONDARY_CPUS: usize = crate::arch::syscall::MAX_CPUS;

static mut DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
static SECONDARY_TSS: [AtomicU64; MAX_SECONDARY_CPUS] =
    [const { AtomicU64::new(0) }; MAX_SECONDARY_CPUS];

static TSS: Lazy<Mutex<TaskStateSegment>> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    let stack_start = VirtAddr::from_ptr(&raw const DOUBLE_FAULT_STACK);
    let stack_end = stack_start + STACK_SIZE as u64;
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_end;
    Mutex::new(tss)
});

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let data_selector = gdt.append(Descriptor::kernel_data_segment());
    let tss = TSS.lock();
    let tss_pointer = &*tss as *const TaskStateSegment;
    let tss_selector = gdt.append(unsafe { Descriptor::tss_segment_unchecked(tss_pointer) });
    drop(tss);
    let user_data_selector = gdt.append(Descriptor::user_data_segment());
    let user_code_selector = gdt.append(Descriptor::user_code_segment());
    (
        gdt,
        Selectors {
            code_selector,
            data_selector,
            tss_selector,
            user_data_selector,
            user_code_selector,
        },
    )
});

struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
    user_data_selector: SegmentSelector,
    user_code_selector: SegmentSelector,
}

pub fn init() {
    use x86_64::instructions::segmentation::{CS, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        SS::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }
}

pub fn init_secondary(
    cpu_index: usize,
    double_fault_stack: VirtAddr,
    kernel_stack: VirtAddr,
) {
    use x86_64::instructions::segmentation::{CS, SS, Segment};
    use x86_64::instructions::tables::load_tss;

    let mut tss = Box::new(TaskStateSegment::new());
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = double_fault_stack;
    tss.privilege_stack_table[0] = kernel_stack;
    let tss = Box::leak(tss);
    SECONDARY_TSS[cpu_index].store(tss as *mut TaskStateSegment as u64, Ordering::Release);
    let mut gdt = Box::new(GlobalDescriptorTable::new());
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let data_selector = gdt.append(Descriptor::kernel_data_segment());
    let tss_selector = gdt.append(unsafe {
        Descriptor::tss_segment_unchecked(tss as *const TaskStateSegment)
    });
    let user_data_selector = gdt.append(Descriptor::user_data_segment());
    let user_code_selector = gdt.append(Descriptor::user_code_segment());
    let gdt = Box::leak(gdt);
    gdt.load();
    unsafe {
        CS::set_reg(code_selector);
        SS::set_reg(data_selector);
        load_tss(tss_selector);
    }
    assert_eq!(user_data_selector.0, USER_DATA_SELECTOR);
    assert_eq!(user_code_selector.0, USER_CODE_SELECTOR);
}

pub fn user_data_selector() -> u16 {
    assert_eq!(GDT.1.user_data_selector.0, USER_DATA_SELECTOR);
    GDT.1.user_data_selector.0
}

pub fn user_code_selector() -> u16 {
    assert_eq!(GDT.1.user_code_selector.0, USER_CODE_SELECTOR);
    GDT.1.user_code_selector.0
}

pub fn set_kernel_stack(stack_top: VirtAddr) {
    let cpu_index = crate::arch::syscall::current_cpu_index();
    if cpu_index == 0 {
        TSS.lock().privilege_stack_table[0] = stack_top;
    } else {
        let tss = SECONDARY_TSS
            .get(cpu_index)
            .expect("secondary CPU index is in range")
            .load(Ordering::Acquire) as *mut TaskStateSegment;
        assert!(!tss.is_null(), "secondary CPU TSS is not initialized");
        unsafe { (*tss).privilege_stack_table[0] = stack_top };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::PrivilegeLevel;

    #[test_case]
    fn gdt_contains_ring_three_segments() {
        assert_eq!(GDT.1.user_data_selector.rpl(), PrivilegeLevel::Ring3);
        assert_eq!(GDT.1.user_code_selector.rpl(), PrivilegeLevel::Ring3);
        assert_eq!(user_data_selector() & 3, 3);
        assert_eq!(user_code_selector() & 3, 3);
    }
}
