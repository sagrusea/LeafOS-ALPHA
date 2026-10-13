use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use linked_list_allocator::Heap;
use spin::Mutex;
use x86_64::{
    structures::paging::{Page, PageTableFlags},
    VirtAddr,
};

use crate::memory::{self, MappingError, MemoryError};

const HEAP_SIZE: usize = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;
const HEAP_PAGES: usize = HEAP_SIZE / PAGE_SIZE as usize;

#[derive(Debug)]
pub enum HeapError {
    Memory(MemoryError),
    Mapping(MappingError),
    AlreadyInitialized,
    NoVirtualRange,
    RollbackFailed,
}

struct HeapState {
    heap: Heap,
    initialized: bool,
}

pub struct KernelAllocator {
    state: Mutex<HeapState>,
}

#[global_allocator]
pub static GLOBAL_ALLOCATOR: KernelAllocator = KernelAllocator {
    state: Mutex::new(HeapState {
        heap: Heap::empty(),
        initialized: false,
    }),
};

fn with_heap<R>(operation: impl FnOnce(&mut HeapState) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut state = GLOBAL_ALLOCATOR.state.lock();
        operation(&mut state)
    })
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        with_heap(|state| {
            if !state.initialized {
                return ptr::null_mut();
            }
            state
                .heap
                .allocate_first_fit(layout)
                .map_or(ptr::null_mut(), |allocation| allocation.as_ptr())
        })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        with_heap(|state| {
            assert!(state.initialized, "deallocation before heap initialization");
            let pointer = core::ptr::NonNull::new(pointer).expect("null heap deallocation");
            unsafe { state.heap.deallocate(pointer, layout) };
        });
    }
}

/// Initializes the fixed heap during single-CPU boot, before other CPUs start.
///
/// # Safety
///
/// Call exactly once after memory-manager initialization and before concurrent
/// allocation or starting other CPUs.
pub unsafe fn initialize() -> Result<(), HeapError> {
    if with_heap(|state| state.initialized) {
        return Err(HeapError::AlreadyInitialized);
    }

    let heap_start = memory::with_manager(|manager| {
        let first_page = manager
            .first_unused_low_half_page()
            .ok_or(HeapError::NoVirtualRange)?;
        let start = first_page.start_address();
        let _last_address = start
            .as_u64()
            .checked_add(HEAP_SIZE as u64)
            .and_then(|end| VirtAddr::try_new(end - 1).ok())
            .ok_or(HeapError::NoVirtualRange)?;

        let mut mapped_pages = 0;
        while mapped_pages < HEAP_PAGES {
            let address = start + (mapped_pages as u64 * PAGE_SIZE);
            let page = Page::containing_address(address);
            match unsafe {
                manager.allocate_and_map(
                    page,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                )
            } {
                Ok(_) => mapped_pages += 1,
                Err(error) => {
                    while mapped_pages != 0 {
                        mapped_pages -= 1;
                        let rollback_page =
                            Page::containing_address(start + (mapped_pages as u64 * PAGE_SIZE));
                        if unsafe { manager.unmap_and_deallocate(rollback_page) }.is_err() {
                            return Err(HeapError::RollbackFailed);
                        }
                    }
                    return Err(HeapError::Mapping(error));
                }
            }
        }

        Ok::<_, HeapError>(start)
    })
    .map_err(HeapError::Memory)??;

    with_heap(|state| {
        if state.initialized {
            return Err(HeapError::AlreadyInitialized);
        }
        unsafe { state.heap.init(heap_start.as_mut_ptr(), HEAP_SIZE) };
        state.initialized = true;
        Ok(())
    })
}
