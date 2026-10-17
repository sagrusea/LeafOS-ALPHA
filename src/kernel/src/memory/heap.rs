use crate::sync::Mutex;
use core::alloc::{GlobalAlloc, Layout};
use core::convert::TryFrom;
use core::ptr;
use linked_list_allocator::Heap;
use x86_64::{
    VirtAddr,
    structures::paging::{Page, PageTableFlags},
};

use super::{MappingError, MemoryError};

const HEAP_SIZE: usize = 1024 * 1024;
const PAGE_SIZE: u64 = 4096;
const HEAP_PAGES: usize = HEAP_SIZE / PAGE_SIZE as usize;
const HEAP_GROWTH_PAGES: usize = HEAP_PAGES;
const PML4_SLOT_SIZE: u64 = 1 << 39;
const LOWER_HALF_PML4_ENTRIES: u64 = 256;
const PML4_SLOT_PAGES: usize = (PML4_SLOT_SIZE / PAGE_SIZE) as usize;
const MIN_HOLE_SIZE: usize = 2 * core::mem::size_of::<usize>();
const PAGES_PER_PAGE_TABLE: usize = 512; // one 4 KiB page table maps 2 MiB
const PAGES_PER_PAGE_DIRECTORY: usize = 1 << 18; // one page directory maps 1 GiB

#[derive(Debug)]
pub enum HeapError {
    Memory(MemoryError),
    Mapping(MappingError),
    AlreadyInitialized,
    NoVirtualRange,
    InsufficientPhysicalMemory,
    RollbackFailed,
}

struct HeapState {
    heap: Heap,
    initialized: bool,
    poisoned: bool,
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
        poisoned: false,
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

/// Returns (minimum heap pages, conservative target heap pages) for `layout`.
/// The target includes alignment slack and free-list metadata. Mapping is still
/// committed in bounded chunks so each rollback fits the reclaim cache.
fn growth_pages_for_layout(layout: Layout) -> Option<(usize, usize)> {
    let allocation_bytes = layout.size().max(MIN_HOLE_SIZE);
    let minimum_bytes = allocation_bytes.checked_add(PAGE_SIZE as usize - 1)?;
    let minimum_pages = minimum_bytes / PAGE_SIZE as usize;
    let aligned_bytes = allocation_bytes.checked_add(layout.align().checked_sub(1)?)?;
    let rounded_aligned_bytes = aligned_bytes.checked_add(PAGE_SIZE as usize - 1)?;
    let target_pages = rounded_aligned_bytes / PAGE_SIZE as usize;
    Some((minimum_pages, target_pages))
}

fn ceil_div(value: usize, divisor: usize) -> Option<usize> {
    value.checked_add(divisor.checked_sub(1)?)?.checked_div(divisor)
}

fn added_table_pages(
    previous_pages: usize,
    growth_pages: usize,
    pages_per_table: usize,
) -> Option<usize> {
    let end_pages = previous_pages.checked_add(growth_pages)?;
    ceil_div(end_pages, pages_per_table)?.checked_sub(ceil_div(previous_pages, pages_per_table)?)
}

/// Upper bound on frames consumed by this growth: all leaf frames plus the page
/// tables needed to cover the newly mapped 2 MiB and 1 GiB regions. Checking
/// this before mapping avoids mapping-and-rolling-back gigabytes for an
/// allocation that cannot fit in physical memory.
fn required_frames_for_growth(previous_pages: usize, growth_pages: usize) -> Option<usize> {
    if growth_pages == 0 {
        return None;
    }
    let leaf_and_pt = growth_pages.checked_add(added_table_pages(
        previous_pages,
        growth_pages,
        PAGES_PER_PAGE_TABLE,
    )?)?;
    let with_pd = leaf_and_pt.checked_add(added_table_pages(
        previous_pages,
        growth_pages,
        PAGES_PER_PAGE_DIRECTORY,
    )?)?;
    with_pd.checked_add(if previous_pages == 0 { 1 } else { 0 })
}

fn growth_chunk_pages(
    target_total_pages: usize,
    already_mapped_pages: usize,
    available_pages: usize,
) -> usize {
    let desired_pages = if target_total_pages > already_mapped_pages {
        target_total_pages - already_mapped_pages
    } else {
        HEAP_GROWTH_PAGES
    };
    desired_pages.min(HEAP_GROWTH_PAGES).min(available_pages)
}

fn available_pages_in_slot(heap_start: VirtAddr, mapped_pages: usize) -> Option<usize> {
    let start_address = heap_start.as_u64();
    if start_address % PML4_SLOT_SIZE != 0 {
        return None;
    }
    let slot_index = start_address / PML4_SLOT_SIZE;
    if slot_index == 0 || slot_index >= LOWER_HALF_PML4_ENTRIES {
        return None;
    }
    let slot_pages = usize::try_from(PML4_SLOT_SIZE / PAGE_SIZE).ok()?;
    slot_pages.checked_sub(mapped_pages)
}

/// Returns the growth start only when the entire heap remains in one canonical
/// lower-half PML4 slot. This prevents both non-canonical addresses and accidental
/// growth into a neighboring slot owned by another subsystem.
fn checked_growth_start(
    heap_start: VirtAddr,
    mapped_pages: usize,
    additional_pages: usize,
) -> Option<VirtAddr> {
    if additional_pages == 0 {
        return None;
    }

    let start_address = heap_start.as_u64();
    if start_address % PML4_SLOT_SIZE != 0 {
        return None;
    }
    let slot_index = start_address / PML4_SLOT_SIZE;
    if slot_index == 0 || slot_index >= LOWER_HALF_PML4_ENTRIES {
        return None;
    }

    let total_pages = mapped_pages.checked_add(additional_pages)?;
    let total_bytes = u64::try_from(total_pages).ok()?.checked_mul(PAGE_SIZE)?;
    let end_exclusive = start_address.checked_add(total_bytes)?;
    let slot_end = slot_index.checked_add(1)?.checked_mul(PML4_SLOT_SIZE)?;
    if end_exclusive > slot_end || VirtAddr::try_new(end_exclusive.checked_sub(1)?).is_err() {
        return None;
    }

    let offset = u64::try_from(mapped_pages).ok()?.checked_mul(PAGE_SIZE)?;
    VirtAddr::try_new(start_address.checked_add(offset)?).ok()
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        with_heap(|state| {
            if !state.initialized || state.poisoned {
                return ptr::null_mut();
            }
            if let Ok(allocation) = state.heap.allocate_first_fit(layout) {
                return allocation.as_ptr();
            }

            let (minimum_total_pages, target_total_pages) = match growth_pages_for_layout(layout) {
                Some(pages) => pages,
                None => return ptr::null_mut(),
            };
            // Bound work for oversized requests before mapping anything. The
            // target includes alignment slack; the minimum is a hard lower bound.
            if target_total_pages > PML4_SLOT_PAGES {
                return ptr::null_mut();
            }
            let minimum_additional_pages = minimum_total_pages.saturating_sub(state.mapped_pages);
            if minimum_additional_pages != 0 {
                let required_frames = match required_frames_for_growth(
                    state.mapped_pages,
                    minimum_additional_pages,
                ) {
                    Some(count) => count,
                    None => return ptr::null_mut(),
                };
                let available_frames = match super::with_manager(|manager| manager.available_frame_count()) {
                    Ok(count) => count,
                    Err(_) => return ptr::null_mut(),
                };
                if available_frames < required_frames {
                    return ptr::null_mut();
                }
            }

            let initial_mapped_pages = state.mapped_pages;
            let target_requires_growth = target_total_pages > initial_mapped_pages;
            loop {
                if grow(state, target_total_pages).is_err() {
                    return ptr::null_mut();
                }
                if let Ok(allocation) = state.heap.allocate_first_fit(layout) {
                    return allocation.as_ptr();
                }
                // A request that triggered growth beyond the existing extent has
                // already received its conservative size/alignment allowance.
                // If it still cannot fit, stop instead of expanding by 1 MiB
                // forever on every retry. Small requests against a fragmented
                // existing heap may continue to trigger bounded growth.
                if target_requires_growth && state.mapped_pages >= target_total_pages {
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

/// Initializes the heap during single-CPU boot, before other CPUs start.
///
/// # Safety
///
/// Call exactly once after memory-manager initialization and before concurrent
/// allocation or starting other CPUs.
pub unsafe fn initialize() -> Result<(), HeapError> {
    with_heap(|state| {
        if state.initialized {
            return Err(HeapError::AlreadyInitialized);
        }
        if state.poisoned {
            return Err(HeapError::RollbackFailed);
        }
        grow(state, HEAP_GROWTH_PAGES)?;
        state.initialized = true;
        Ok(())
    })
}

fn grow(state: &mut HeapState, target_total_pages: usize) -> Result<(), HeapError> {
    if state.poisoned {
        return Err(HeapError::RollbackFailed);
    }

    // Copy the committed extent before entering the manager closure, so state is
    // mutated only after the mapping operation commits successfully.
    let heap_start = state.start;
    let already_mapped_pages = state.mapped_pages;

    let result = super::with_manager(|manager| {
        let base = match heap_start {
            Some(start) => start,
            None => manager
                .first_unused_low_half_page()
                .ok_or(HeapError::NoVirtualRange)?
                .start_address(),
        };
        let available_pages = available_pages_in_slot(base, already_mapped_pages)
            .ok_or(HeapError::NoVirtualRange)?;
        let pages = growth_chunk_pages(
            target_total_pages,
            already_mapped_pages,
            available_pages,
        );
        if pages == 0 {
            return Err(HeapError::NoVirtualRange);
        }
        let growth_start = checked_growth_start(base, already_mapped_pages, pages)
            .ok_or(HeapError::NoVirtualRange)?;
        let required_frames = required_frames_for_growth(already_mapped_pages, pages)
            .ok_or(HeapError::InsufficientPhysicalMemory)?;
        if manager.available_frame_count() < required_frames {
            return Err(HeapError::InsufficientPhysicalMemory);
        }

        let mut mapped_pages = 0;
        while mapped_pages < pages {
            let page_address = growth_start + (mapped_pages as u64 * PAGE_SIZE);
            let page = Page::containing_address(page_address);
            match unsafe {
                manager.allocate_and_map(
                    page,
                    PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::NO_EXECUTE,
                )
            } {
                Ok(_) => mapped_pages += 1,
                Err(error) => {
                    let mut rollback_failed = false;
                    while mapped_pages != 0 {
                        mapped_pages -= 1;
                        let rollback_page = Page::containing_address(
                            growth_start + (mapped_pages as u64 * PAGE_SIZE),
                        );
                        if unsafe { manager.unmap_and_deallocate(rollback_page) }.is_err() {
                            rollback_failed = true;
                        }
                    }
                    return if rollback_failed {
                        Err(HeapError::RollbackFailed)
                    } else {
                        Err(HeapError::Mapping(error))
                    };
                }
            }
        }
        Ok::<_, HeapError>((growth_start, pages))
    });

    let (growth_start, pages) = match result {
        Err(error) => return Err(HeapError::Memory(error)),
        Ok(Ok(growth)) => growth,
        Ok(Err(HeapError::RollbackFailed)) => {
            // A failed rollback means some mappings may remain outside the heap's
            // committed extent. Never retry into an unknown mapping state.
            state.poisoned = true;
            return Err(HeapError::RollbackFailed);
        }
        Ok(Err(error)) => return Err(error),
    };

    if state.start.is_some() {
        // The range check and copied state guarantee this is the contiguous tail.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn growth_sizing_handles_small_large_and_overaligned_layouts() {
        let small = Layout::from_size_align(32, 8).unwrap();
        assert_eq!(growth_pages_for_layout(small), Some((1, 1)));

        let large = Layout::from_size_align(HEAP_SIZE * 3 + 1, 16).unwrap();
        let minimum_bytes = large.size().max(MIN_HOLE_SIZE);
        let minimum = (minimum_bytes + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
        let aligned_bytes = minimum_bytes + large.align() - 1;
        let target = (aligned_bytes + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
        assert_eq!(growth_pages_for_layout(large), Some((minimum, target)));

        let overaligned = Layout::from_size_align(8, 2 * 1024 * 1024).unwrap();
        let minimum_bytes = overaligned.size().max(MIN_HOLE_SIZE);
        let minimum = (minimum_bytes + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
        let aligned_bytes = minimum_bytes + overaligned.align() - 1;
        let target = (aligned_bytes + PAGE_SIZE as usize - 1) / PAGE_SIZE as usize;
        assert_eq!(growth_pages_for_layout(overaligned), Some((minimum, target)));
    }

    #[test_case]
    fn large_requests_grow_in_rollback_safe_chunks() {
        assert_eq!(growth_chunk_pages(2048, 256, PML4_SLOT_PAGES), HEAP_GROWTH_PAGES);
        assert_eq!(growth_chunk_pages(769, 768, PML4_SLOT_PAGES), 1);
        assert_eq!(growth_chunk_pages(100, 512, 17), 17);
        assert_eq!(growth_chunk_pages(100, 512, 0), 0);
    }

    #[test_case]
    fn growth_preflight_counts_leaf_and_new_page_table_frames() {
        assert_eq!(required_frames_for_growth(0, 1), Some(4));
        assert_eq!(
            required_frames_for_growth(0, HEAP_GROWTH_PAGES),
            Some(HEAP_GROWTH_PAGES + 3)
        );
        assert_eq!(required_frames_for_growth(511, 1), Some(1));
        assert_eq!(required_frames_for_growth(512, 1), Some(2));
        assert_eq!(required_frames_for_growth(PAGES_PER_PAGE_DIRECTORY, 1), Some(3));
        assert_eq!(required_frames_for_growth(usize::MAX, 1), None);
    }

    #[test_case]
    fn growth_can_use_remaining_slot_space_without_crossing_into_another_slot() {
        let slot_start = VirtAddr::try_new(255 * PML4_SLOT_SIZE).unwrap();
        let slot_pages = (PML4_SLOT_SIZE / PAGE_SIZE) as usize;
        assert_eq!(available_pages_in_slot(slot_start, slot_pages - 17), Some(17));
        assert_eq!(available_pages_in_slot(slot_start, slot_pages), Some(0));
        assert_eq!(available_pages_in_slot(slot_start, slot_pages + 1), None);
    }

    #[test_case]
    fn growth_range_cannot_cross_a_pml4_slot_or_the_canonical_boundary() {
        let slot_start = VirtAddr::try_new(255 * PML4_SLOT_SIZE).unwrap();
        let slot_pages = (PML4_SLOT_SIZE / PAGE_SIZE) as usize;

        assert_eq!(
            checked_growth_start(slot_start, 0, HEAP_GROWTH_PAGES),
            Some(slot_start)
        );
        assert!(checked_growth_start(slot_start, slot_pages - 1, 1).is_some());
        assert_eq!(checked_growth_start(slot_start, slot_pages, 1), None);
        assert_eq!(checked_growth_start(slot_start, 0, slot_pages + 1), None);

        let slot_zero = VirtAddr::try_new(0).unwrap();
        assert_eq!(checked_growth_start(slot_zero, 0, 1), None);

        let high_half = VirtAddr::try_new(0xffff_8000_0000_0000).unwrap();
        assert_eq!(checked_growth_start(high_half, 0, 1), None);
    }
}
