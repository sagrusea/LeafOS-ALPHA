use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use linked_list_allocator::Heap;
use spin::Mutex;
use x86_64::{
    VirtAddr,
    structures::paging::{Page, PageTableFlags},
};

use super::{MappingError, MemoryError};

const HEAP_SIZE: usize = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;
const HEAP_PAGES: usize = HEAP_SIZE / PAGE_SIZE as usize;
const HEAP_GROWTH_PAGES: usize = HEAP_PAGES;

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
    start: Option<VirtAddr>,
    mapped_pages: usize,
}

pub struct KernelAllocator {
    state: Mutex<HeapState>,
}

#[global_allocator]
pub static GLOBAL_ALLOCATOR: KernelAllocator = KernelAllocator {
    state: Mutex::new(HeapState {
        heap: Heap::empty(),
        initialized: false,
        start: None,
        mapped_pages: 0,
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
            loop {
                if let Ok(allocation) = state.heap.allocate_first_fit(layout) {
                    return allocation.as_ptr();
                }
                if grow(state).is_err() {
                    return ptr::null_mut();
                }
            }
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

    with_heap(|state| {
        if state.initialized {
            return Err(HeapError::AlreadyInitialized);
        }
        grow(state)?;
        state.initialized = true;
        Ok(())
    })
}

fn grow(state: &mut HeapState) -> Result<(), HeapError> {
    let pages = HEAP_GROWTH_PAGES;
    let growth_start = super::with_manager(|manager| {
        let start = match state.start {
            Some(start) => start + (state.mapped_pages as u64 * PAGE_SIZE),
            None => manager
                .first_unused_low_half_page()
                .ok_or(HeapError::NoVirtualRange)?
                .start_address(),
        };
        let end = start
            .as_u64()
            .checked_add((pages as u64) * PAGE_SIZE)
            .and_then(|address| VirtAddr::try_new(address - 1).ok())
            .ok_or(HeapError::NoVirtualRange)?;
        let _ = end;

        let mut mapped_pages = 0;
        while mapped_pages < pages {
            let page = Page::containing_address(start + mapped_pages as u64 * PAGE_SIZE);
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
                            Page::containing_address(start + mapped_pages as u64 * PAGE_SIZE);
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

    if let Some(start) = state.start {
        let expected = start + (state.mapped_pages as u64 * PAGE_SIZE);
        assert_eq!(growth_start, expected, "heap growth must be contiguous");
        unsafe { state.heap.extend(pages * PAGE_SIZE as usize) };
    } else {
        unsafe {
            state
                .heap
                .init(growth_start.as_mut_ptr(), pages * PAGE_SIZE as usize)
        };
        state.start = Some(growth_start);
    }
    state.mapped_pages += pages;
    Ok(())
}

pub fn mapped_bytes() -> usize {
    with_heap(|state| state.mapped_pages * PAGE_SIZE as usize)
}
