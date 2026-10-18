#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(kernel::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use core::sync::atomic::{AtomicPtr, Ordering};
use kernel::boot_info::BootInfo;
use kernel::scheduler::{self, TaskId, TaskState};
use kernel::serial_println;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{Page, PageTable, PageTableFlags},
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
static USER_TASK_KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);
static USER_TASK_DIAGNOSTICS_CONTINUED: AtomicBool = AtomicBool::new(false);
static DYNAMIC_APP_TASK_ID: AtomicU64 = AtomicU64::new(0);

fn launch_app_from_kernel_task() {
    let task = kernel::user_program::launch("apps/smoke.app")
        .expect("launch LeafOS app while scheduler is running");
    DYNAMIC_APP_TASK_ID.store(task.get(), Ordering::SeqCst);
}

fn create_and_remove_ext2_directory_task() {
    kernel::fs::ext2::create_directory("TASKDIR").expect("create directory in scheduled task");
    TASKED_EXT2_DIRECTORY_CREATED.store(true, Ordering::SeqCst);
    kernel::fs::ext2::remove_empty_directory("TASKDIR")
        .expect("remove directory in scheduled task");
}

fn map_test_user_program(code: &[u8]) -> (kernel::memory::AddressSpace, PhysAddr) {
    use kernel::memory::{self, USER_SPACE_START, UserPageAccess};

    let mut address_space = memory::create_address_space().expect("create user test address space");
    let code_page = Page::containing_address(VirtAddr::new(USER_SPACE_START + 0x1000));
    let stack_page = Page::containing_address(VirtAddr::new(USER_SPACE_START + 0x3000));
    let code_frame = address_space
        .map_user_page(code_page, UserPageAccess::ReadExecute)
        .expect("map user test code");
    address_space
        .map_user_page(stack_page, UserPageAccess::ReadWrite)
        .expect("map user test stack");
    let offset = memory::with_manager(|manager| manager.physical_memory_offset())
        .expect("get physical-memory offset");
    let code_virtual = VirtAddr::try_new(offset.as_u64() + code_frame.as_u64())
        .expect("direct-map user test code frame");
    unsafe {
        core::ptr::copy_nonoverlapping(code.as_ptr(), code_virtual.as_mut_ptr::<u8>(), code.len());
    }
    (address_space, code_frame)
}

fn run_user_test_program(code: &[u8]) -> u64 {
    use kernel::memory::USER_SPACE_START;

    let (address_space, _code_frame) = map_test_user_program(code);
    let task = scheduler::spawn_user(
        address_space,
        VirtAddr::new(USER_SPACE_START + 0x1000),
        VirtAddr::new(USER_SPACE_START + 0x4000),
    )
    .expect("spawn syscall integration test task");
    scheduler::run_tasks().expect("run syscall integration test task");
    let exit_code = scheduler::task_exit_code(task)
        .expect("inspect syscall test task")
        .expect("syscall test task exited with a status");
    assert_eq!(scheduler::task_state(task), Ok(TaskState::Terminated));
    scheduler::reap_terminated(task).expect("reap syscall integration test task");
    exit_code
}

fn emit_mov_rdi_imm64(code: &mut Vec<u8>, value: u64) -> usize {
    code.extend_from_slice(&[0x48, 0xbf]); // mov rdi, imm64
    let immediate = code.len();
    code.extend_from_slice(&value.to_le_bytes());
    immediate
}

fn emit_mov_rdx_imm64(code: &mut Vec<u8>, value: u64) {
    code.extend_from_slice(&[0x48, 0xba]); // mov rdx, imm64
    code.extend_from_slice(&value.to_le_bytes());
}

fn emit_mov_esi_imm32(code: &mut Vec<u8>, value: u32) {
    code.push(0xbe); // mov esi, imm32
    code.extend_from_slice(&value.to_le_bytes());
}

fn emit_mov_r10d_imm32(code: &mut Vec<u8>, value: u32) {
    code.extend_from_slice(&[0x41, 0xba]); // mov r10d, imm32
    code.extend_from_slice(&value.to_le_bytes());
}

fn emit_mov_eax_imm32(code: &mut Vec<u8>, value: u32) {
    code.push(0xb8); // mov eax, imm32
    code.extend_from_slice(&value.to_le_bytes());
}

fn emit_syscall(code: &mut Vec<u8>, number: u32) {
    emit_mov_eax_imm32(code, number);
    code.extend_from_slice(&[0x0f, 0x05]); // syscall
}

fn emit_exit_with_rax(code: &mut Vec<u8>) {
    code.extend_from_slice(&[0x48, 0x89, 0xc7]); // mov rdi, rax
    emit_syscall(code, kernel::arch::syscall::SYS_EXIT as u32);
    code.extend_from_slice(&[0x0f, 0x0b]); // ud2 if exit unexpectedly returns
}

fn emit_exit_status(code: &mut Vec<u8>, status: u32) {
    code.push(0xbf); // mov edi, imm32
    code.extend_from_slice(&status.to_le_bytes());
    emit_syscall(code, kernel::arch::syscall::SYS_EXIT as u32);
    code.extend_from_slice(&[0x0f, 0x0b]);
}

fn emit_jne_placeholder(code: &mut Vec<u8>, branches: &mut Vec<usize>) {
    code.extend_from_slice(&[0x75, 0]); // jne rel8; patched once the failure label is known
    branches.push(code.len() - 1);
}

fn patch_jne_branches(code: &mut [u8], branches: &[usize], target: usize) {
    for displacement_index in branches {
        let next_instruction = displacement_index + 1;
        let displacement = target as isize - next_instruction as isize;
        assert!((-128..=127).contains(&displacement));
        code[*displacement_index] = displacement as i8 as u8;
    }
}

fn user_task_diagnostics_survivor() {
    USER_TASK_DIAGNOSTICS_CONTINUED.store(true, Ordering::SeqCst);
    USER_TASK_KERNEL_ROOT.store(Cr3::read().0.start_address().as_u64(), Ordering::SeqCst);
}

unsafe fn user_mapping_flags(
    root: PhysAddr,
    physical_memory_offset: VirtAddr,
    page: Page,
) -> [PageTableFlags; 4] {
    let mut flags = [PageTableFlags::empty(); 4];
    let mut table = unsafe {
        &*VirtAddr::try_new(physical_memory_offset.as_u64() + root.as_u64())
            .expect("canonical page-table address")
            .as_ptr::<PageTable>()
    };
    let indexes = [
        page.p4_index(),
        page.p3_index(),
        page.p2_index(),
        page.p1_index(),
    ];
    for (level, index) in indexes.into_iter().enumerate() {
        let entry = &table[index];
        flags[level] = entry.flags();
        if level != 3 {
            assert!(!entry.flags().contains(PageTableFlags::HUGE_PAGE));
            let next = VirtAddr::try_new(physical_memory_offset.as_u64() + entry.addr().as_u64())
                .expect("canonical next-level page-table address");
            table = unsafe { &*next.as_ptr::<PageTable>() };
        }
    }
    flags
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
    let events: [usize; 4] =
        core::array::from_fn(|index| SCHEDULER_EVENTS[index].load(Ordering::SeqCst));
    let position = |event| {
        events
            .iter()
            .position(|recorded| *recorded == event)
            .unwrap()
    };
    assert!(position(1) < position(3));
    assert!(position(2) < position(4));
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

    let free_blocks_before_reclaim_test =
        ext2::free_block_count().expect("read ext2 free block counter");
    ext2::write_file("RECLAIM.TXT", &[b'R'; 2500]).expect("write file that uses three data blocks");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after extending"),
        free_blocks_before_reclaim_test - 3
    );
    ext2::write_file("RECLAIM.TXT", b"short").expect("shrink file and release unused data blocks");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after shrinking"),
        free_blocks_before_reclaim_test - 1
    );
    ext2::write_file("RECLAIM.TXT", b"").expect("truncate file to zero and release its last block");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after truncating"),
        free_blocks_before_reclaim_test
    );
    ext2::write_file("RECLAIM.TXT", &[b'R'; 2500]).expect("reuse blocks released by truncation");
    ext2::remove_file("RECLAIM.TXT").expect("delete file and release its data blocks");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after deleting"),
        free_blocks_before_reclaim_test
    );

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

    let free_blocks_before_indirect_test =
        ext2::free_block_count().expect("count blocks before indirect write");
    let indirect_contents: Vec<u8> = (0..(13 * 1024 + 17))
        .map(|index| b'A' + (index % 26) as u8)
        .collect();
    ext2::write_file("INDIRECT-WRITE.TXT", &indirect_contents)
        .expect("write file across the single-indirect boundary");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after indirect write"),
        free_blocks_before_indirect_test - 15
    );
    let mut indirect_output = vec![0; indirect_contents.len()];
    let (copied, total) = ext2::read_file("INDIRECT-WRITE.TXT", &mut indirect_output)
        .expect("read file through single-indirect blocks")
        .expect("single-indirect test file exists");
    assert_eq!(copied, indirect_contents.len());
    assert_eq!(total, indirect_contents.len() as u64);
    assert_eq!(indirect_output, indirect_contents);

    ext2::write_file("INDIRECT-WRITE.TXT", &[b'S'; 2 * 1024])
        .expect("shrink file below the single-indirect boundary");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after indirect shrink"),
        free_blocks_before_indirect_test - 2
    );
    ext2::remove_file("INDIRECT-WRITE.TXT").expect("remove single-indirect file");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after indirect file removal"),
        free_blocks_before_indirect_test
    );

    let double_indirect_contents: Vec<u8> = (0..(300 * 1024 + 17))
        .map(|index| b'a' + (index % 26) as u8)
        .collect();
    ext2::write_file("DOUBLE-INDIRECT.TXT", &double_indirect_contents)
        .expect("write file across the double-indirect boundary");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after double-indirect write"),
        free_blocks_before_indirect_test - 304
    );
    let mut double_indirect_output = vec![0; double_indirect_contents.len()];
    let (copied, total) = ext2::read_file("DOUBLE-INDIRECT.TXT", &mut double_indirect_output)
        .expect("read file through double-indirect blocks")
        .expect("double-indirect test file exists");
    assert_eq!(copied, double_indirect_contents.len());
    assert_eq!(total, double_indirect_contents.len() as u64);
    assert_eq!(double_indirect_output, double_indirect_contents);

    ext2::write_file("DOUBLE-INDIRECT.TXT", &[b'D'; 20 * 1024])
        .expect("shrink file below the double-indirect boundary");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after double-indirect shrink"),
        free_blocks_before_indirect_test - 21
    );
    ext2::write_file("DOUBLE-INDIRECT.TXT", &[b'D'; 2 * 1024])
        .expect("shrink file below the single-indirect boundary");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after dropping indirect metadata"),
        free_blocks_before_indirect_test - 2
    );
    ext2::remove_file("DOUBLE-INDIRECT.TXT").expect("remove double-indirect file");
    assert_eq!(
        ext2::free_block_count().expect("count blocks after double-indirect removal"),
        free_blocks_before_indirect_test
    );

    assert_eq!(
        ext2::write_file("../OUTSIDE.TXT", b"x"),
        Err(Ext2Error::InvalidPath)
    );
    assert_eq!(ext2::write_file("docs", b"x"), Err(Ext2Error::InvalidPath));
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

#[test_case]
fn test_address_spaces_isolate_user_pages_and_protect_shared_kernel_mappings() {
    use kernel::memory::{self, USER_SPACE_START, UserPageAccess};

    let original_root = Cr3::read().0;
    let mut first = memory::create_address_space().expect("create first address space");
    let mut second = memory::create_address_space().expect("create second address space");
    let executable = Page::containing_address(VirtAddr::new(USER_SPACE_START + 0x1000));
    let data = Page::containing_address(VirtAddr::new(USER_SPACE_START + 0x2000));
    let stack_page = Page::containing_address(VirtAddr::new(USER_SPACE_START + 0x3000));

    let first_exec_frame = first
        .map_user_page(executable, UserPageAccess::ReadExecute)
        .expect("map executable page in first process");
    let first_data_frame = first
        .map_user_page(data, UserPageAccess::ReadWrite)
        .expect("map writable data page in first process");
    first
        .map_user_page(stack_page, UserPageAccess::ReadWrite)
        .expect("map stack page in first process");
    let second_data_frame = second
        .map_user_page(data, UserPageAccess::ReadWrite)
        .expect("map writable data page in second process");
    assert_ne!(first_exec_frame, first_data_frame);
    assert_ne!(first_data_frame, second_data_frame);

    let stack_top = VirtAddr::new(USER_SPACE_START + 0x4000);
    kernel::arch::user::UserContext::for_address_space(
        &first,
        VirtAddr::new(USER_SPACE_START + 0x1000),
        stack_top,
    )
    .expect("validate executable entry and writable user stack");
    assert_eq!(
        kernel::arch::user::UserContext::for_address_space(
            &first,
            VirtAddr::new(USER_SPACE_START + 0x2000),
            stack_top,
        ),
        Err(kernel::arch::user::UserContextError::EntryNotExecutable)
    );

    let physical_memory_offset = memory::with_manager(|manager| manager.physical_memory_offset())
        .expect("read physical-memory offset");
    let first_root = first
        .root_physical_address()
        .expect("first root remains live");
    let second_root = second
        .root_physical_address()
        .expect("second root remains live");
    assert_ne!(first_root, second_root);
    let user_flags = unsafe { user_mapping_flags(first_root, physical_memory_offset, data) };
    assert!(
        user_flags
            .iter()
            .all(|flags| flags.contains(PageTableFlags::USER_ACCESSIBLE))
    );
    assert!(user_flags[3].contains(PageTableFlags::WRITABLE));
    assert!(user_flags[3].contains(PageTableFlags::NO_EXECUTE));
    let kernel_page = Page::<x86_64::structures::paging::Size4KiB>::containing_address(
        VirtAddr::from_ptr(memory::create_address_space as *const ()),
    );
    let kernel_root = original_root.start_address();
    let kernel_root_virtual =
        VirtAddr::try_new(physical_memory_offset.as_u64() + kernel_root.as_u64())
            .expect("canonical kernel root address");
    let kernel_root_table = unsafe { &*kernel_root_virtual.as_ptr::<PageTable>() };
    assert!(
        !kernel_root_table[kernel_page.p4_index()]
            .flags()
            .contains(PageTableFlags::USER_ACCESSIBLE)
    );
    let first_root_virtual =
        VirtAddr::try_new(physical_memory_offset.as_u64() + first_root.as_u64())
            .expect("canonical first root address");
    let first_root_table = unsafe { &*first_root_virtual.as_ptr::<PageTable>() };
    assert!(
        !first_root_table[kernel_page.p4_index()]
            .flags()
            .contains(PageTableFlags::USER_ACCESSIBLE)
    );
    assert!(
        first_root_table[usize::from(data.p4_index())]
            .flags()
            .contains(PageTableFlags::USER_ACCESSIBLE)
    );

    let interrupts_were_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let first_value = 0x1357_9bdf_2468_ace0u64;
    let observed_first;
    let observed_second;
    unsafe {
        first
            .activate()
            .expect("activate first process address space");
        (USER_SPACE_START as *mut u64)
            .add(0x2000 / core::mem::size_of::<u64>())
            .write_volatile(first_value);
        observed_first = (USER_SPACE_START as *const u64)
            .add(0x2000 / core::mem::size_of::<u64>())
            .read_volatile();

        second
            .activate()
            .expect("activate second process address space");
        observed_second = (USER_SPACE_START as *const u64)
            .add(0x2000 / core::mem::size_of::<u64>())
            .read_volatile();
        memory::activate_kernel_address_space().expect("restore kernel address space");
    }
    if interrupts_were_enabled {
        x86_64::instructions::interrupts::enable();
    }
    assert_eq!(observed_first, first_value);
    assert_eq!(observed_second, 0);
    first.destroy().expect("destroy first address space");
    second.destroy().expect("destroy second address space");
}

#[test_case]
fn test_scheduled_user_tasks_enter_syscall_exit_and_isolate_faults() {
    use kernel::memory::USER_SPACE_START;

    USER_TASK_DIAGNOSTICS_CONTINUED.store(false, Ordering::SeqCst);
    USER_TASK_KERNEL_ROOT.store(0, Ordering::SeqCst);
    let kernel_root = Cr3::read().0.start_address().as_u64();

    let mut faulting_code = [
        0xb8, 0, 0, 0, 0, 0x0f, 0x05, 0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0x48, 0x8b, 0x18, 0x0f,
        0x0b,
    ];
    let kernel_address = kernel::memory::create_address_space as *const () as u64;
    faulting_code[9..17].copy_from_slice(&kernel_address.to_le_bytes());
    let (faulting_space, faulting_code_frame) = map_test_user_program(&faulting_code);

    let exiting_code = [
        0xb8, 0, 0, 0, 0, 0x0f, 0x05, 0xbf, 7, 0, 0, 0, 0xb8, 1, 0, 0, 0, 0x0f, 0x05, 0x0f, 0x0b,
    ];
    let (exiting_space, exiting_code_frame) = map_test_user_program(&exiting_code);
    assert_ne!(faulting_code_frame, exiting_code_frame);

    let mut invalid_return_code = [
        0x48, 0xbc, 0, 0, 0, 0, 0, 0, 0, 0, 0xb8, 0, 0, 0, 0, 0x0f, 0x05, 0x0f, 0x0b,
    ];
    invalid_return_code[2..10].copy_from_slice(&kernel_address.to_le_bytes());
    let (invalid_return_space, invalid_return_code_frame) =
        map_test_user_program(&invalid_return_code);
    assert_ne!(faulting_code_frame, invalid_return_code_frame);
    assert_ne!(exiting_code_frame, invalid_return_code_frame);

    let entry = VirtAddr::new(USER_SPACE_START + 0x1000);
    let stack = VirtAddr::new(USER_SPACE_START + 0x4000);
    let faulting_task = scheduler::spawn_user(faulting_space, entry, stack)
        .expect("schedule faulting ring-three task");
    let exiting_task = scheduler::spawn_user(exiting_space, entry, stack)
        .expect("schedule exiting ring-three task");
    let invalid_return_task = scheduler::spawn_user(invalid_return_space, entry, stack)
        .expect("schedule invalid-return ring-three task");
    let diagnostics_task =
        scheduler::spawn(user_task_diagnostics_survivor).expect("schedule kernel diagnostics task");

    scheduler::run_tasks().expect("user tasks should return control to the kernel");

    assert_eq!(
        scheduler::task_state(faulting_task),
        Ok(TaskState::Terminated)
    );
    assert_eq!(
        scheduler::task_state(exiting_task),
        Ok(TaskState::Terminated)
    );
    assert_eq!(scheduler::task_exit_code(exiting_task), Ok(Some(7)));
    assert_eq!(
        scheduler::task_state(invalid_return_task),
        Ok(TaskState::Terminated)
    );
    assert_eq!(
        scheduler::task_state(diagnostics_task),
        Ok(TaskState::Terminated)
    );
    assert!(USER_TASK_DIAGNOSTICS_CONTINUED.load(Ordering::SeqCst));
    assert_eq!(USER_TASK_KERNEL_ROOT.load(Ordering::SeqCst), kernel_root);
    assert_eq!(Cr3::read().0.start_address().as_u64(), kernel_root);

    scheduler::reap_terminated(faulting_task).expect("reap faulting user task");
    scheduler::reap_terminated(exiting_task).expect("reap exiting user task");
    scheduler::reap_terminated(invalid_return_task).expect("reap invalid-return user task");
    scheduler::reap_terminated(diagnostics_task).expect("reap diagnostics task");
}

#[test_case]
fn test_user_console_write_syscall_bounds_and_validates_user_memory() {
    use kernel::{arch::syscall, memory::USER_SPACE_START};

    // A readable user buffer produces the requested byte count and exercises
    // the actual console-write path from ring three.
    let mut normal = Vec::new();
    let buffer_immediate = emit_mov_rdi_imm64(&mut normal, 0);
    emit_mov_esi_imm32(&mut normal, 5);
    emit_syscall(&mut normal, syscall::SYS_WRITE as u32);
    emit_exit_with_rax(&mut normal);
    let message_offset = normal.len();
    normal.extend_from_slice(b"hello");
    normal[buffer_immediate..buffer_immediate + 8].copy_from_slice(
        &(USER_SPACE_START + 0x1000 + message_offset as u64).to_le_bytes(),
    );
    assert_eq!(run_user_test_program(&normal), 5);

    // Empty writes are harmless and do not inspect the supplied pointer.
    let mut empty = Vec::new();
    emit_mov_rdi_imm64(&mut empty, 0);
    emit_mov_esi_imm32(&mut empty, 0);
    emit_syscall(&mut empty, syscall::SYS_WRITE as u32);
    emit_exit_with_rax(&mut empty);
    assert_eq!(run_user_test_program(&empty), 0);

    // Oversized requests fail before the pointer is touched.
    let mut oversized = Vec::new();
    emit_mov_rdi_imm64(&mut oversized, 0);
    emit_mov_esi_imm32(&mut oversized, syscall::MAX_SYSCALL_BUFFER as u32 + 1);
    emit_syscall(&mut oversized, syscall::SYS_WRITE as u32);
    emit_exit_with_rax(&mut oversized);
    assert_eq!(run_user_test_program(&oversized), syscall::SYS_ERR_E2BIG);

    // Null, unmapped, and page-crossing buffers are rejected without a kernel
    // dereference. The final range starts in a mapped code page but crosses
    // into an unmapped page, so checking only the first byte would be unsafe.
    for (bad_pointer, length) in [
        (0, 1),
        (USER_SPACE_START + 0x5000, 1),
        (USER_SPACE_START + 0x1ffe, 8),
    ] {
        let mut invalid = Vec::new();
        emit_mov_rdi_imm64(&mut invalid, bad_pointer);
        emit_mov_esi_imm32(&mut invalid, length);
        emit_syscall(&mut invalid, syscall::SYS_WRITE as u32);
        emit_exit_with_rax(&mut invalid);
        assert_eq!(run_user_test_program(&invalid), syscall::SYS_ERR_EFAULT);
    }
}

#[test_case]
fn test_user_input_syscall_is_nonblocking_and_checks_writable_buffers() {
    use kernel::{arch::syscall, drivers::input, memory::USER_SPACE_START};

    // Start from a known queue state so this is deterministic even if the test
    // runner received keyboard events before the test began.
    let mut discard = [0u8; input::INPUT_CAPACITY];
    while input::read_available(&mut discard) != 0 {}

    let mut empty = Vec::new();
    emit_mov_rdi_imm64(&mut empty, USER_SPACE_START + 0x3000);
    emit_mov_esi_imm32(&mut empty, 1);
    emit_syscall(&mut empty, syscall::SYS_READ as u32);
    emit_exit_with_rax(&mut empty);
    assert_eq!(run_user_test_program(&empty), 0, "input must not block when the queue is empty");

    input::enqueue(b'Z');

    let mut invalid = Vec::new();
    emit_mov_rdi_imm64(&mut invalid, USER_SPACE_START + 0x1000); // RX, not writable
    emit_mov_esi_imm32(&mut invalid, 1);
    emit_syscall(&mut invalid, syscall::SYS_READ as u32);
    emit_exit_with_rax(&mut invalid);
    assert_eq!(run_user_test_program(&invalid), syscall::SYS_ERR_EFAULT);

    let destination = USER_SPACE_START + 0x3000;
    let mut valid = Vec::new();
    emit_mov_rdi_imm64(&mut valid, destination);
    emit_mov_esi_imm32(&mut valid, 1);
    emit_syscall(&mut valid, syscall::SYS_READ as u32);
    valid.extend_from_slice(&[0x48, 0x83, 0xf8, 1]); // cmp rax, 1
    let mut branches = Vec::new();
    emit_jne_placeholder(&mut valid, &mut branches);
    emit_mov_rdi_imm64(&mut valid, destination);
    valid.extend_from_slice(&[0x80, 0x3f, b'Z']); // cmp byte ptr [rdi], 'Z'
    emit_jne_placeholder(&mut valid, &mut branches);
    emit_exit_status(&mut valid, 42);
    let failure = valid.len();
    emit_exit_status(&mut valid, 1);
    patch_jne_branches(&mut valid, &branches, failure);

    let status = run_user_test_program(&valid);
    let _ = input::read_available(&mut discard);
    assert_eq!(status, 42, "input syscall should copy the queued byte");
}

#[test_case]
fn test_user_file_read_syscall_reads_only_bounded_ext2_files() {
    use kernel::{arch::syscall, fs::ext2, memory::USER_SPACE_START};

    const PATH: &[u8] = b"SYSCALL.TXT";
    ext2::write_file("SYSCALL.TXT", b"hello").expect("create file for syscall read test");

    let destination = USER_SPACE_START + 0x3000;
    let mut code = Vec::new();
    let path_immediate = emit_mov_rdi_imm64(&mut code, 0);
    emit_mov_esi_imm32(&mut code, PATH.len() as u32);
    emit_mov_rdx_imm64(&mut code, destination);
    emit_mov_r10d_imm32(&mut code, 16);
    emit_syscall(&mut code, syscall::SYS_READ_FILE as u32);
    code.extend_from_slice(&[0x48, 0x83, 0xf8, 5]); // cmp rax, 5
    let mut branches = Vec::new();
    emit_jne_placeholder(&mut code, &mut branches);
    emit_mov_rdi_imm64(&mut code, destination);
    for (index, byte) in b"hello".iter().copied().enumerate() {
        if index == 0 {
            code.extend_from_slice(&[0x80, 0x3f, byte]); // cmp byte ptr [rdi], imm8
        } else {
            code.extend_from_slice(&[0x80, 0x7f, index as u8, byte]);
        }
        emit_jne_placeholder(&mut code, &mut branches);
    }
    emit_exit_status(&mut code, 42);
    let failure = code.len();
    emit_exit_status(&mut code, 1);
    patch_jne_branches(&mut code, &branches, failure);

    let path_offset = code.len();
    code.extend_from_slice(PATH);
    code[path_immediate..path_immediate + 8].copy_from_slice(
        &(USER_SPACE_START + 0x1000 + path_offset as u64).to_le_bytes(),
    );

    let status = run_user_test_program(&code);
    ext2::remove_file("SYSCALL.TXT").expect("remove file from syscall read test");
    assert_eq!(status, 42, "file syscall should copy the file contents to user memory");
}

#[test_case]
fn test_raw_user_program_can_be_relaunched_after_reaping() {
    use kernel::memory::UserPageAccess;

    assert!(matches!(
        kernel::user_program::load("apps/smoke.exe"),
        Err(kernel::user_program::ProgramError::InvalidExtension)
    ));

    let program =
        kernel::user_program::load("apps/smoke.bin").expect("load bounded raw program from initrd");
    assert_eq!(
        program.user_page_access(VirtAddr::new(kernel::user_program::RAW_PROGRAM_BASE)),
        Some(UserPageAccess::ReadExecute)
    );
    assert_eq!(
        program.user_page_access(VirtAddr::new(kernel::user_program::USER_STACK_TOP - 1)),
        Some(UserPageAccess::ReadWrite)
    );

    let tasks_before = scheduler::task_snapshot().into_iter().flatten().count();
    let mut previous_id = None;
    for _ in 0..3 {
        let task = kernel::user_program::launch("apps/smoke.bin")
            .expect("load and schedule raw user program");
        if let Some(previous_id) = previous_id {
            assert!(
                task.get() > previous_id,
                "task IDs remain unique and monotonically increasing"
            );
        }
        scheduler::run_tasks().expect("run loaded user program");

        assert_eq!(scheduler::task_state(task), Ok(TaskState::Terminated));
        assert_eq!(scheduler::task_exit_code(task), Ok(Some(42)));
        scheduler::reap_terminated(task).expect("reap loaded user program");
        assert_eq!(
            scheduler::task_state(task),
            Err(scheduler::SchedulerError::UnknownTask)
        );
        assert_eq!(
            scheduler::task_snapshot().into_iter().flatten().count(),
            tasks_before,
            "reaping returns the task slot for later launches"
        );
        previous_id = Some(task.get());
    }
}

#[test_case]
fn test_running_kernel_task_can_launch_user_app_dynamically() {
    DYNAMIC_APP_TASK_ID.store(0, Ordering::SeqCst);
    let launcher =
        scheduler::spawn(launch_app_from_kernel_task).expect("spawn dynamic app launcher");
    scheduler::run_tasks().expect("run dynamic app launcher and user app");

    assert_eq!(scheduler::task_state(launcher), Ok(TaskState::Terminated));
    let app_id = DYNAMIC_APP_TASK_ID.load(Ordering::SeqCst);
    assert_ne!(app_id, 0, "kernel task should have launched the app");
    let app = scheduler::task_snapshot()
        .into_iter()
        .flatten()
        .find(|task| task.id.get() == app_id)
        .expect("launched app task remains available for status inspection");
    assert!(app.is_user);
    assert_eq!(app.state, TaskState::Terminated);
    assert_eq!(scheduler::task_exit_code(app.id), Ok(Some(42)));

    scheduler::reap_terminated(app.id).expect("reap dynamically launched app");
    scheduler::reap_terminated(launcher).expect("reap dynamic app launcher");
}

#[test_case]
fn test_leaf_app_maps_code_and_data_and_exits_through_syscall() {
    use kernel::memory::UserPageAccess;

    let program = kernel::user_program::load("apps/smoke.app")
        .expect("load versioned LeafOS application from initrd");
    assert_eq!(
        program.user_page_access(VirtAddr::new(kernel::user_program::RAW_PROGRAM_BASE)),
        Some(UserPageAccess::ReadExecute)
    );
    let data_start = program
        .data_start()
        .expect("smoke app contains an initialized data segment");
    assert_eq!(
        program.user_page_access(data_start),
        Some(UserPageAccess::ReadWrite)
    );
    assert_eq!(
        program.user_page_access(VirtAddr::new(kernel::user_program::USER_STACK_TOP - 1)),
        Some(UserPageAccess::ReadWrite)
    );

    let task = program.spawn().expect("schedule LeafOS application");
    scheduler::run_tasks().expect("run LeafOS application");

    assert_eq!(scheduler::task_state(task), Ok(TaskState::Terminated));
    assert_eq!(scheduler::task_exit_code(task), Ok(Some(42)));
    scheduler::reap_terminated(task).expect("reap LeafOS application");
}

#[test_case]
fn test_leaf_app_loads_and_runs_from_ext2_data_disk() {
    use kernel::memory::UserPageAccess;

    let program = kernel::user_program::load_from_ext2("apps/smoke.app")
        .expect("load bounded LeafOS application from ext2");
    assert_eq!(
        program.user_page_access(VirtAddr::new(kernel::user_program::RAW_PROGRAM_BASE)),
        Some(UserPageAccess::ReadExecute)
    );
    let data_start = program
        .data_start()
        .expect("ext2 smoke app contains initialized data");
    assert_eq!(
        program.user_page_access(data_start),
        Some(UserPageAccess::ReadWrite)
    );

    let task = program.spawn().expect("schedule ext2 application");
    scheduler::run_tasks().expect("run ext2 application");
    assert_eq!(scheduler::task_state(task), Ok(TaskState::Terminated));
    assert_eq!(scheduler::task_exit_code(task), Ok(Some(42)));
    scheduler::reap_terminated(task).expect("reap ext2 application");
}

#[test_case]
fn test_user_app_receives_argument_count_and_vector() {
    use kernel::fs::ext2;

    let code = [
        0x48, 0x83, 0xff, 0x02, // cmp rdi, 2
        0x75, 0x23, // jne fail
        0x48, 0x8b, 0x46, 0x08, // mov rax, [rsi + 8]
        0x80, 0x38, b'o', // cmp byte ptr [rax], 'o'
        0x75, 0x1a, // jne fail
        0x80, 0x78, 0x01, b'k', // cmp byte ptr [rax + 1], 'k'
        0x75, 0x14, // jne fail
        0x80, 0x78, 0x02, 0x00, // cmp byte ptr [rax + 2], 0
        0x75, 0x0e, // jne fail
        0xbf, 42, 0, 0, 0, // mov edi, 42
        0xb8, 1, 0, 0, 0, // mov eax, SYS_EXIT
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
        0xbf, 1, 0, 0, 0, // fail: mov edi, 1
        0xb8, 1, 0, 0, 0, // mov eax, SYS_EXIT
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    ext2::write_file("apps/ARGS.bin", &code).expect("write argument ABI test program");

    let task = kernel::user_program::launch_from_ext2_with_arguments("apps/ARGS.bin", &["ok"])
        .expect("launch app with an argument");
    scheduler::run_tasks().expect("run app and verify argv");
    assert_eq!(scheduler::task_state(task), Ok(TaskState::Terminated));
    assert_eq!(
        scheduler::task_exit_code(task),
        Ok(Some(42)),
        "app should receive argc=2 and argv[1]=\"ok\""
    );
    scheduler::reap_terminated(task).expect("reap argument test app");
    ext2::remove_file("apps/ARGS.bin").expect("remove argument ABI test program");
}

#[test_case]
fn test_ext2_app_loader_rejects_invalid_and_oversized_images() {
    use kernel::fs::ext2::{self, Ext2Error};
    use kernel::user_program::ProgramError;

    assert!(matches!(
        kernel::user_program::load_from_ext2("apps/missing.app"),
        Err(ProgramError::FileNotFound)
    ));
    assert!(matches!(
        kernel::user_program::load_from_ext2("apps/smoke.exe"),
        Err(ProgramError::InvalidExtension)
    ));

    let free_blocks_before =
        ext2::free_block_count().expect("read free-space count before loader bound test");
    ext2::write_file(
        "apps/TOO-LARGE.bin",
        &vec![0; kernel::user_program::RAW_PROGRAM_MAX_SIZE + 1],
    )
    .expect("create oversized ext2 image");
    let free_blocks_after_write =
        ext2::free_block_count().expect("read free-space count after oversized image write");
    assert!(matches!(
        kernel::user_program::load_from_ext2("apps/TOO-LARGE.bin"),
        Err(ProgramError::ImageTooLarge)
    ));
    assert_eq!(
        ext2::free_block_count().expect("check loader did not allocate ext2 blocks"),
        free_blocks_after_write,
        "rejecting an oversized app must not change disk allocation"
    );
    ext2::remove_file("apps/TOO-LARGE.bin").expect("remove oversized ext2 image");
    assert_eq!(
        ext2::free_block_count().expect("check oversized image cleanup"),
        free_blocks_before
    );

    assert_eq!(
        ext2::read_file_vec("apps/smoke.app", 1),
        Err(Ext2Error::FileTooLarge)
    );
}

#[test_case]
#[cfg(feature = "test-hooks")]
fn test_ext2_replacement_data_write_failure_preserves_previous_file() {
    use kernel::fs::ext2::{self, Ext2Error};

    let original = [b'P'; 100];
    let replacement = [b'N'; 100];
    ext2::write_file("FAULT-SAFE.TXT", &original).expect("create original file");
    let free_blocks_before =
        ext2::free_block_count().expect("read free blocks before failed replacement");

    for successful_writes in 0..8 {
        ext2::fail_next_write_after(successful_writes);
        assert_eq!(
            ext2::write_file("FAULT-SAFE.TXT", &replacement),
            Err(Ext2Error::Disk(kernel::drivers::ata::AtaError::Device)),
            "failure after {} successful block writes",
            successful_writes
        );

        let mut output = [0; 100];
        let (copied, total) = ext2::read_file("FAULT-SAFE.TXT", &mut output)
            .expect("read file after failed replacement")
            .expect("original file should remain present");
        assert_eq!(copied, original.len());
        assert_eq!(total, original.len() as u64);
        assert_eq!(output, original);
        assert_eq!(
            ext2::free_block_count().expect("read free blocks after failed replacement"),
            free_blocks_before,
            "rollback must restore allocation metadata"
        );
    }

    ext2::write_file("FAULT-SAFE.TXT", &replacement)
        .expect("write succeeds after failed transactions roll back");
    let mut output = [0; 100];
    let (copied, total) = ext2::read_file("FAULT-SAFE.TXT", &mut output)
        .expect("read successfully replaced file")
        .expect("file remains present");
    assert_eq!(copied, replacement.len());
    assert_eq!(total, replacement.len() as u64);
    assert_eq!(output, replacement);
    assert_eq!(
        ext2::free_block_count().expect("read free blocks after successful replacement"),
        free_blocks_before
    );
    ext2::remove_file("FAULT-SAFE.TXT").expect("remove failure test file");
}

#[test_case]
#[cfg(feature = "test-hooks")]
fn test_ext2_new_file_commit_failure_leaves_no_directory_entry_or_allocations() {
    use kernel::fs::ext2::{self, Ext2Error};

    let contents = b"transactional create";
    let free_blocks_before =
        ext2::free_block_count().expect("read free blocks before create failures");
    for successful_writes in 0..7 {
        ext2::fail_next_write_after(successful_writes);
        assert_eq!(
            ext2::write_file("apps/CREATE-FAIL.TXT", contents),
            Err(Ext2Error::Disk(kernel::drivers::ata::AtaError::Device)),
            "failure after {} successful block writes",
            successful_writes
        );
        let mut output = [0; 64];
        assert_eq!(
            ext2::read_file("apps/CREATE-FAIL.TXT", &mut output).expect("check failed create path"),
            None,
            "failed create must not publish a directory entry"
        );
        assert_eq!(
            ext2::free_block_count().expect("check free count after failed create"),
            free_blocks_before,
            "rollback must release all allocations from failed create"
        );
    }

    ext2::write_file("apps/CREATE-FAIL.TXT", contents)
        .expect("create succeeds after failed transactions roll back");
    ext2::remove_file("apps/CREATE-FAIL.TXT").expect("remove successful create test file");
    assert_eq!(
        ext2::free_block_count().expect("check successful create cleanup"),
        free_blocks_before
    );
}

#[unsafe(no_mangle)]
extern "Rust" fn __rust_alloc_error_handler(size: usize, align: usize) -> ! {
    let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(size, align) };
    allocation_error(layout)
}

fn allocation_error(_: core::alloc::Layout) -> ! {
    panic!("test heap allocation failed");
}
