#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![feature(alloc_error_handler)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
extern crate alloc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};
use kernel::boot_info::BootInfo;
use kernel::serial_println;
use x86_64::{
    registers::control::Cr3,
    structures::paging::{Page, PageTableFlags},
    VirtAddr,
};

static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start(boot_info: *const BootInfo) -> ! {
    BOOT_INFO.store(boot_info.cast_mut(), Ordering::Relaxed);
    if let Some(boot_info) = unsafe { BootInfo::from_ptr(boot_info) } {
        unsafe { kernel::memory::initialize(boot_info) }.expect("initialize memory manager");
        unsafe { kernel::heap::initialize() }.expect("initialize kernel heap");
    }
    test_main();

    loop {}
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::test_panic_handler(info)
}

#[test_case]
fn test_println() {
    serial_println!("test println output");
}

#[test_case]
fn test_active_page_table_access() {
    let boot_info_ptr = BOOT_INFO.load(Ordering::Relaxed);
    let boot_info = unsafe { BootInfo::from_ptr(boot_info_ptr) }.expect("valid UEFI boot info");
    let (active_root, _) = Cr3::read();
    assert_eq!(
        boot_info.page_table_root,
        active_root.start_address().as_u64()
    );

    let kernel_address = VirtAddr::new(_start as *const () as usize as u64);
    let physical_address = kernel::memory::with_manager(|manager| {
        let kernel_physical_address = manager
            .translate_addr(kernel_address)
            .expect("kernel test code is mapped");
        let direct_map_address =
            VirtAddr::new(boot_info.physical_memory_offset + active_root.start_address().as_u64());
        let page_table_physical_address = manager
            .translate_addr(direct_map_address)
            .expect("page-table frame is direct-mapped");
        (kernel_physical_address, page_table_physical_address)
    })
    .expect("memory manager initialized");
    assert_eq!(
        physical_address.0,
        x86_64::PhysAddr::new(kernel_address.as_u64())
    );
    assert_eq!(physical_address.1, active_root.start_address());
}

#[test_case]
fn test_mapping_with_uefi_frame_allocator() {
    kernel::memory::with_manager(|manager| {
        let page = manager
            .first_unused_low_half_page()
            .expect("an unused low-half page-table slot");
        let frame = unsafe {
            manager.allocate_and_map(page, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)
        }
        .expect("allocate and map a conventional memory frame");

        let mapped_address = page.start_address();
        let translated_address = manager
            .translate_addr(mapped_address)
            .expect("new page mapping is present");
        assert_eq!(translated_address, frame.start_address());

        unsafe {
            mapped_address
                .as_mut_ptr::<u64>()
                .write_volatile(0x4C45_4146_4F53);
            assert_eq!(
                mapped_address.as_ptr::<u64>().read_volatile(),
                0x4C45_4146_4F53
            );
        }

        unsafe { manager.unmap_and_deallocate(page) }.expect("unmap and reclaim the frame");
        assert_eq!(
            manager.allocate_frame(),
            Some(frame),
            "unmapped physical frame should be reusable"
        );
    })
    .expect("memory manager initialized");
}

#[test_case]
fn test_failed_mapping_reclaims_allocated_frame() {
    let kernel_page = Page::containing_address(VirtAddr::new(_start as *const () as usize as u64));
    kernel::memory::with_manager(|manager| {
        let expected_frame = manager
            .allocate_frame()
            .expect("a conventional memory frame");
        unsafe { manager.deallocate_frame(expected_frame) }
            .expect("return the unused frame to the allocator");

        let result = unsafe {
            manager.allocate_and_map(
                kernel_page,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            )
        };
        assert!(matches!(
            result,
            Err(kernel::memory::MappingError::Mapper(_))
        ));
        assert_eq!(
            manager
                .allocate_frame()
                .expect("the failed mapping's frame was reclaimed"),
            expected_frame
        );
    })
    .expect("memory manager initialized");
}

#[test_case]
fn test_kernel_heap_allocates_and_reuses_memory() {
    let mut first = Vec::with_capacity(4096);
    for value in 0..4096 {
        first.push(value);
    }
    assert_eq!(first[2048], 2048);
    drop(first);

    let second = Vec::<u64>::with_capacity(4096);
    assert!(second.capacity() >= 4096);
}

#[alloc_error_handler]
fn allocation_error(_: core::alloc::Layout) -> ! {
    panic!("test heap allocation failed");
}
