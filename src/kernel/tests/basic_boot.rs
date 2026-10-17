#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
extern crate alloc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use core::sync::atomic::{AtomicPtr, Ordering};
use kernel::boot_info::BootInfo;
use kernel::scheduler::{self, TaskId, TaskState};
use kernel::serial_println;
use x86_64::{
    VirtAddr,
    registers::control::Cr3,
    structures::paging::{Page, PageTableFlags},
};

static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());
static SCHEDULER_EVENT_INDEX: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER_EVENTS: [AtomicUsize; 4] = [const { AtomicUsize::new(0) }; 4];
static SCHEDULER_TASK_ENTERED_BLOCK: AtomicBool = AtomicBool::new(false);
static SCHEDULER_TASK_RESUMED: AtomicBool = AtomicBool::new(false);
static SCHEDULER_WAKER_RESUMED: AtomicBool = AtomicBool::new(false);
static BLOCKED_TASK_ID: kernel::sync::Mutex<Option<TaskId>> = kernel::sync::Mutex::new(None);
static PREEMPT_FIRST_STARTED: AtomicBool = AtomicBool::new(false);
static PREEMPT_SECOND_RAN: AtomicBool = AtomicBool::new(false);
static PREEMPT_FIRST_FINISHED: AtomicBool = AtomicBool::new(false);
static PREEMPT_FIRST_LOCAL: AtomicU64 = AtomicU64::new(0);
static TASKED_EXT2_DIRECTORY_CREATED: AtomicBool = AtomicBool::new(false);

fn create_and_remove_ext2_directory_task() {
    kernel::fs::ext2::create_directory("TASKDIR").expect("create directory in scheduled task");
    TASKED_EXT2_DIRECTORY_CREATED.store(true, Ordering::SeqCst);
    kernel::fs::ext2::remove_empty_directory("TASKDIR")
        .expect("remove directory in scheduled task");
}

fn preempted_cpu_bound_task() {
    PREEMPT_FIRST_STARTED.store(true, Ordering::SeqCst);
    let mut local = 0x1234_5678_9abc_def0u64;
    while !PREEMPT_SECOND_RAN.load(Ordering::SeqCst) {
        local = local.rotate_left(7) ^ 0xfeed_face_cafe_beef;
        core::hint::spin_loop();
    }
    PREEMPT_FIRST_LOCAL.store(local, Ordering::SeqCst);
    PREEMPT_FIRST_FINISHED.store(true, Ordering::SeqCst);
}

fn preempting_peer_task() {
    while !PREEMPT_FIRST_STARTED.load(Ordering::SeqCst) {
        core::hint::spin_loop();
    }
    PREEMPT_SECOND_RAN.store(true, Ordering::SeqCst);
}

fn record_scheduler_event(value: usize) {
    let index = SCHEDULER_EVENT_INDEX.fetch_add(1, Ordering::SeqCst);
    assert!(index < SCHEDULER_EVENTS.len());
    SCHEDULER_EVENTS[index].store(value, Ordering::SeqCst);
}

fn first_kernel_task() {
    record_scheduler_event(1);
    scheduler::yield_now().expect("first task should yield");
    record_scheduler_event(3);
}

fn second_kernel_task() {
    record_scheduler_event(2);
    scheduler::yield_now().expect("second task should yield");
    record_scheduler_event(4);
}

fn blocking_kernel_task() {
    SCHEDULER_TASK_ENTERED_BLOCK.store(true, Ordering::SeqCst);
    scheduler::block_now().expect("block and switch to waker task");
    SCHEDULER_TASK_RESUMED.store(true, Ordering::SeqCst);
}

fn waking_kernel_task() {
    let blocked = (*BLOCKED_TASK_ID.lock()).expect("blocked task ID was not set");
    scheduler::wake_task(blocked).expect("wake blocked task");
    scheduler::yield_now().expect("yield to the unblocked task");
    SCHEDULER_WAKER_RESUMED.store(true, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start(boot_info: *const BootInfo) -> ! {
    BOOT_INFO.store(boot_info.cast_mut(), Ordering::Relaxed);
    if let Some(boot_info) = unsafe { BootInfo::from_ptr(boot_info) } {
        unsafe { kernel::memory::initialize(boot_info) }.expect("initialize memory manager");
        unsafe { kernel::fs::initialize(boot_info) }.expect("validate initrd");
        unsafe { kernel::memory::heap::initialize() }.expect("initialize kernel heap");
        kernel::fs::ext2::initialize().expect("mount ext2 test disk");
    }
    kernel::init();
    test_main();

    loop {}
}

#[test_case]
fn test_cooperative_tasks_yield_terminate_and_return_to_bootstrap() {
    SCHEDULER_EVENT_INDEX.store(0, Ordering::SeqCst);
    for event in &SCHEDULER_EVENTS {
        event.store(0, Ordering::SeqCst);
    }

    let first: TaskId = scheduler::spawn(first_kernel_task).expect("spawn first task");
    let second: TaskId = scheduler::spawn(second_kernel_task).expect("spawn second task");
    scheduler::run_tasks().expect("run cooperative tasks");

    assert_eq!(SCHEDULER_EVENT_INDEX.load(Ordering::SeqCst), 4);
    assert_eq!(
        [
            SCHEDULER_EVENTS[0].load(Ordering::SeqCst),
            SCHEDULER_EVENTS[1].load(Ordering::SeqCst),
            SCHEDULER_EVENTS[2].load(Ordering::SeqCst),
            SCHEDULER_EVENTS[3].load(Ordering::SeqCst),
        ],
        [1, 2, 3, 4]
    );
    assert_eq!(scheduler::task_state(first), Ok(TaskState::Terminated));
    assert_eq!(scheduler::task_state(second), Ok(TaskState::Terminated));
    scheduler::reap_terminated(first).expect("reap first task");
    scheduler::reap_terminated(second).expect("reap second task");

    SCHEDULER_TASK_ENTERED_BLOCK.store(false, Ordering::SeqCst);
    SCHEDULER_TASK_RESUMED.store(false, Ordering::SeqCst);
    SCHEDULER_WAKER_RESUMED.store(false, Ordering::SeqCst);
    let blocked = scheduler::spawn(blocking_kernel_task).expect("spawn blocking task");
    *BLOCKED_TASK_ID.lock() = Some(blocked);
    let waker = scheduler::spawn(waking_kernel_task).expect("spawn waking task");

    scheduler::run_tasks().expect("blocked task wakes and returns to bootstrap");
    assert!(SCHEDULER_TASK_ENTERED_BLOCK.load(Ordering::SeqCst));
    assert!(SCHEDULER_TASK_RESUMED.load(Ordering::SeqCst));
    assert!(SCHEDULER_WAKER_RESUMED.load(Ordering::SeqCst));
    assert_eq!(scheduler::task_state(blocked), Ok(TaskState::Terminated));
    assert_eq!(scheduler::task_state(waker), Ok(TaskState::Terminated));
    scheduler::reap_terminated(blocked).expect("reap blocked task");
    scheduler::reap_terminated(waker).expect("reap waker task");
    *BLOCKED_TASK_ID.lock() = None;
    assert_eq!(
        scheduler::task_state(blocked),
        Err(scheduler::SchedulerError::UnknownTask)
    );
}

#[test_case]
fn test_timer_preempts_cpu_bound_tasks_and_restores_their_context() {
    PREEMPT_FIRST_STARTED.store(false, Ordering::SeqCst);
    PREEMPT_SECOND_RAN.store(false, Ordering::SeqCst);
    PREEMPT_FIRST_FINISHED.store(false, Ordering::SeqCst);
    PREEMPT_FIRST_LOCAL.store(0, Ordering::SeqCst);
    let tick_before = kernel::timer::ticks();

    let first = scheduler::spawn(preempted_cpu_bound_task).expect("spawn CPU-bound task");
    let second = scheduler::spawn(preempting_peer_task).expect("spawn peer task");
    scheduler::run_tasks().expect("timer-preempted tasks should finish");

    assert!(PREEMPT_FIRST_STARTED.load(Ordering::SeqCst));
    assert!(PREEMPT_SECOND_RAN.load(Ordering::SeqCst));
    assert!(PREEMPT_FIRST_FINISHED.load(Ordering::SeqCst));
    assert_ne!(PREEMPT_FIRST_LOCAL.load(Ordering::SeqCst), 0);
    assert!(kernel::timer::ticks().wrapping_sub(tick_before) > 0);
    assert_eq!(scheduler::task_state(first), Ok(TaskState::Terminated));
    assert_eq!(scheduler::task_state(second), Ok(TaskState::Terminated));
    scheduler::reap_terminated(first).expect("reap first preempted task");
    scheduler::reap_terminated(second).expect("reap second preempted task");
}

#[test_case]
fn test_kernel_mutex_disables_interrupts_until_unlock() {
    let mutex = kernel::sync::Mutex::new(());
    assert!(x86_64::instructions::interrupts::are_enabled());
    {
        let _guard = mutex.lock();
        assert!(!x86_64::instructions::interrupts::are_enabled());
    }
    assert!(x86_64::instructions::interrupts::are_enabled());

    x86_64::instructions::interrupts::disable();
    {
        let _guard = mutex.lock();
        assert!(!x86_64::instructions::interrupts::are_enabled());
    }
    assert!(!x86_64::instructions::interrupts::are_enabled());
    x86_64::instructions::interrupts::enable();
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
        let mut reclaimed = [None; 4];
        for slot in &mut reclaimed {
            *slot = manager.allocate_frame();
        }
        let frame_was_reclaimed = reclaimed.contains(&Some(frame));
        for reclaimed_frame in reclaimed.into_iter().flatten() {
            unsafe { manager.deallocate_frame(reclaimed_frame) }
                .expect("return inspected frame to the allocator");
        }
        assert!(
            frame_was_reclaimed,
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
    let mut first = Vec::with_capacity(300_000);
    for value in 0..300_000 {
        first.push(value);
    }
    assert_eq!(first[150_000], 150_000);
    drop(first);

    let second = Vec::<u64>::with_capacity(300_000);
    assert!(second.capacity() >= 300_000);
}

#[test_case]
fn test_initrd_lists_and_reads_files() {
    let mut found_readme = false;
    kernel::fs::for_each_file(|name, _| {
        found_readme |= name == "README.TXT";
    })
    .expect("list initrd files");
    assert!(found_readme);

    let contents = kernel::fs::read_file("README.TXT")
        .expect("read initrd file")
        .expect("README.TXT exists");
    assert!(
        core::str::from_utf8(contents)
            .expect("README is UTF-8")
            .contains("LeafOS")
    );
    assert!(
        kernel::fs::read_file("../README.TXT")
            .expect("reject unsafe path")
            .is_none()
    );

    assert!(kernel::fs::directory_exists("docs").expect("check nested directory"));
    let mut found_docs = false;
    kernel::fs::for_each_entry("", |name, is_dir, _| {
        found_docs |= name == "docs" && is_dir;
    })
    .expect("list root directory");
    assert!(found_docs);

    let mut found_about = false;
    kernel::fs::for_each_entry("docs", |name, is_dir, _| {
        found_about |= name == "ABOUT.TXT" && !is_dir;
    })
    .expect("list nested directory");
    assert!(found_about);
    let nested_contents = kernel::fs::read_file("docs/ABOUT.TXT")
        .expect("read nested initrd file")
        .expect("nested file exists");
    assert!(
        core::str::from_utf8(nested_contents)
            .expect("nested file is UTF-8")
            .contains("directories")
    );
    assert!(!kernel::fs::directory_exists("../docs").expect("reject traversal path"));
}

#[test_case]
fn test_ext2_lists_and_reads_files() {
    let mut found_docs = false;
    kernel::fs::ext2::for_each_entry("", |name, is_dir, _| {
        found_docs |= name == "docs" && is_dir;
    })
    .expect("list ext2 root");
    assert!(found_docs);

    let mut found_about = false;
    kernel::fs::ext2::for_each_entry("docs", |name, is_dir, _| {
        found_about |= name == "ABOUT.TXT" && !is_dir;
    })
    .expect("list ext2 nested directory");
    assert!(found_about);

    let mut contents = [0; 512];
    let (copied, total) = kernel::fs::ext2::read_file("docs/ABOUT.TXT", &mut contents)
        .expect("read ext2 file")
        .expect("ext2 file exists");
    assert_eq!(usize::try_from(total).unwrap(), copied);
    assert!(
        core::str::from_utf8(&contents[..copied])
            .expect("ext2 file is UTF-8")
            .contains("directories")
    );
    assert!(
        kernel::fs::ext2::read_file("../README.TXT", &mut contents)
            .expect("reject ext2 path traversal")
            .is_none()
    );

    let mut indirect_contents = Vec::new();
    indirect_contents.resize(300 * 1024, 0);
    let (copied, total) =
        kernel::fs::ext2::read_file("__ext2_test__/INDIRECT.TXT", &mut indirect_contents)
            .expect("read ext2 double-indirect file")
            .expect("double-indirect test file exists");
    assert_eq!(copied, indirect_contents.len());
    assert_eq!(usize::try_from(total).unwrap(), copied);
    for index in [0, 12 * 1024, 268 * 1024, copied - 1] {
        assert_eq!(indirect_contents[index], b'A' + (index % 26) as u8);
    }

    let mut wide_count = 0;
    let mut found_last = false;
    kernel::fs::ext2::for_each_entry("__ext2_test__/wide", |name, is_dir, _| {
        assert!(!is_dir);
        wide_count += 1;
        found_last |= name == "ENTRY69.TXT";
    })
    .expect("list multi-block ext2 directory");
    assert_eq!(wide_count, 70);
    assert!(found_last);
}

#[test_case]
fn test_ext2_creates_and_replaces_small_root_files() {
    use kernel::fs::ext2::{self, Ext2Error};

    ext2::write_file("WRITE.TXT", b"first write").expect("create ext2 root file");
    ext2::write_file("WRITE.TXT", b"replaced").expect("replace ext2 root file");

    let mut contents = [0; 32];
    let (copied, total) = ext2::read_file("WRITE.TXT", &mut contents)
        .expect("read newly written ext2 file")
        .expect("new file exists");
    assert_eq!(total, 8);
    assert_eq!(&contents[..copied], b"replaced");

    ext2::write_file("EMPTY.TXT", b"").expect("create empty ext2 file");
    assert_eq!(
        ext2::read_file("EMPTY.TXT", &mut contents)
            .expect("read empty ext2 file")
            .expect("empty file exists"),
        (0, 0)
    );

    let large_contents = [b'X'; 2500];
    ext2::write_file("WRITE.TXT", &large_contents).expect("extend file across ext2 blocks");
    let mut large_output = [0; 2500];
    let (copied, total) = ext2::read_file("WRITE.TXT", &mut large_output)
        .expect("read multi-block ext2 file")
        .expect("multi-block file exists");
    assert_eq!(copied, large_contents.len());
    assert_eq!(total, large_contents.len() as u64);
    assert_eq!(large_output, large_contents);

    ext2::write_file("WRITE.TXT", b"short replacement")
        .expect("shrink existing multi-block ext2 file");
    let (copied, total) = ext2::read_file("WRITE.TXT", &mut contents)
        .expect("read shrunk ext2 file")
        .expect("shrunk file exists");
    assert_eq!(total, 17);
    assert_eq!(&contents[..copied], b"short replacement");

    let max_contents = [b'Y'; 12 * 1024];
    ext2::write_file("MAXIMUM.TXT", &max_contents).expect("write twelve direct blocks");
    let mut max_output = [0; 12 * 1024];
    let (copied, total) = ext2::read_file("MAXIMUM.TXT", &mut max_output)
        .expect("read twelve direct blocks")
        .expect("maximum-size file exists");
    assert_eq!(copied, max_contents.len());
    assert_eq!(total, max_contents.len() as u64);
    assert_eq!(max_output, max_contents);

    assert_eq!(
        ext2::write_file("../OUTSIDE.TXT", b"x"),
        Err(Ext2Error::InvalidPath)
    );
    assert_eq!(ext2::write_file("docs", b"x"), Err(Ext2Error::InvalidPath));
    assert_eq!(
        ext2::write_file("TOO-LARGE.TXT", &[0; 12 * 1024 + 1]),
        Err(Ext2Error::UnsupportedFeature)
    );
}

#[test_case]
fn test_ext2_creates_and_removes_root_entries_safely() {
    use kernel::fs::ext2::{self, Ext2Error};

    ext2::create_directory("EMPTYDIR").expect("create ext2 directory");
    assert!(ext2::directory_exists("EMPTYDIR").expect("check new directory"));
    assert_eq!(
        ext2::create_directory("EMPTYDIR"),
        Err(Ext2Error::AlreadyExists)
    );
    assert_eq!(
        ext2::remove_empty_directory("docs"),
        Err(Ext2Error::NotEmpty)
    );
    ext2::remove_empty_directory("EMPTYDIR").expect("remove empty directory");
    assert!(!ext2::directory_exists("EMPTYDIR").expect("check removed directory"));
    assert_eq!(
        ext2::remove_empty_directory("EMPTYDIR"),
        Err(Ext2Error::NotFound)
    );
    assert_eq!(
        ext2::create_directory("../OUTSIDE"),
        Err(Ext2Error::InvalidPath)
    );

    ext2::create_directory("PARENT").expect("create parent directory");
    ext2::create_directory("PARENT/CHILD").expect("create nested directory");
    assert!(ext2::directory_exists("docs").expect("root directory remains accessible"));
    let mut found_parent = false;
    ext2::for_each_entry("", |name, is_dir, _| {
        if name == "PARENT" && is_dir {
            found_parent = true;
        }
    })
    .expect("list ext2 root after nested directory creation");
    assert!(
        found_parent,
        "ext2 root listing should retain the parent directory"
    );
    assert!(ext2::directory_exists("PARENT/CHILD").expect("check nested directory"));
    assert!(ext2::directory_exists("PARENT").expect("check nested parent directory"));
    let mut found_child = false;
    ext2::for_each_entry("PARENT", |name, is_dir, _| {
        if name == "CHILD" && is_dir {
            found_child = true;
        }
    })
    .expect("list nested parent directory");
    assert!(
        found_child,
        "nested parent listing should include its child"
    );
    ext2::write_file("PARENT/CHILD/EDIT.TXT", b"nested editor save")
        .expect("create file for nested editor path");
    let mut nested_contents = [0; 32];
    let (copied, total) = ext2::read_file("PARENT/CHILD/EDIT.TXT", &mut nested_contents)
        .expect("read nested editor path")
        .expect("nested file exists");
    assert_eq!(total, b"nested editor save".len() as u64);
    assert_eq!(&nested_contents[..copied], b"nested editor save");
    ext2::write_file("PARENT/CHILD/EDIT.TXT", b"saved again")
        .expect("save changes to existing nested file");
    let (copied, total) = ext2::read_file("PARENT/CHILD/EDIT.TXT", &mut nested_contents)
        .expect("read updated nested editor path")
        .expect("updated nested file exists");
    assert_eq!(total, b"saved again".len() as u64);
    assert_eq!(&nested_contents[..copied], b"saved again");
    ext2::remove_file("PARENT/CHILD/EDIT.TXT").expect("remove nested test file");
    assert_eq!(
        ext2::remove_empty_directory("PARENT"),
        Err(Ext2Error::NotEmpty)
    );
    ext2::remove_empty_directory("PARENT/CHILD").expect("remove nested directory");
    ext2::remove_empty_directory("PARENT").expect("remove parent directory");

    ext2::write_file("DELETE.TXT", b"delete me").expect("create file for removal");
    assert_eq!(
        ext2::remove_empty_directory("DELETE.TXT"),
        Err(Ext2Error::InvalidPath)
    );
    ext2::remove_file("DELETE.TXT").expect("remove file");
    let mut contents = [0; 16];
    assert_eq!(
        ext2::read_file("DELETE.TXT", &mut contents).expect("read removed file"),
        None
    );
    assert_eq!(ext2::remove_file("DELETE.TXT"), Err(Ext2Error::NotFound));
    assert_eq!(ext2::remove_file("docs"), Err(Ext2Error::InvalidPath));
    assert_eq!(
        ext2::remove_file("../DELETE.TXT"),
        Err(Ext2Error::InvalidPath)
    );
}

#[test_case]
fn test_scheduled_task_can_create_and_remove_ext2_directory() {
    TASKED_EXT2_DIRECTORY_CREATED.store(false, Ordering::SeqCst);
    let task =
        scheduler::spawn(create_and_remove_ext2_directory_task).expect("spawn ext2 directory task");

    scheduler::run_tasks().expect("run ext2 directory task");

    assert!(TASKED_EXT2_DIRECTORY_CREATED.load(Ordering::SeqCst));
    assert_eq!(scheduler::task_state(task), Ok(TaskState::Terminated));
    assert!(!kernel::fs::ext2::directory_exists("TASKDIR").expect("check removed directory"));
    scheduler::reap_terminated(task).expect("reap ext2 directory task");
}

#[unsafe(no_mangle)]
extern "Rust" fn __rust_alloc_error_handler(size: usize, align: usize) -> ! {
    let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(size, align) };
    allocation_error(layout)
}

fn allocation_error(_: core::alloc::Layout) -> ! {
    panic!("test heap allocation failed");
}
