use spin::{Mutex, Once};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags,
        PhysFrame, Size4KiB, Translate,
        mapper::CleanUp,
        mapper::{MapToError, UnmapError},
        page::PageRangeInclusive,
    },
};

use crate::boot_info::BootInfo;

pub mod heap;

const EFI_CONVENTIONAL_MEMORY: u32 = 7;
const PAGE_SIZE: u64 = 4096;
const PAGE_MASK: u64 = PAGE_SIZE - 1;
const EFI_DESCRIPTOR_MIN_SIZE: usize = 40;
const RECLAIMED_FRAME_CAPACITY: usize = 260;
const EFI_MEMORY_MAP_VERSION: u32 = 2;
const EFI_MEMORY_DESCRIPTOR_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    MissingMemoryMap,
    InvalidMemoryMap,
    InvalidPhysicalAddressWidth,
    InvalidBootInfo,
    InvalidPageTableRoot,
    InvalidPhysicalMemoryOffset,
    PhysicalAddressOutOfRange,
    FrameReclaimCapacityExceeded,
    NotInitialized,
}

#[derive(Debug)]
pub enum MappingError {
    PhysicalAddressOutOfRange,
    Mapper(MapToError<Size4KiB>),
}

#[derive(Debug)]
pub enum UnmappingError {
    PageNotMapped,
    FrameNotAllocatorOwned,
    ReclaimCapacityExceeded,
    Mapper(UnmapError),
}

#[derive(Clone, Copy, Debug)]
struct PhysicalRange {
    start: u64,
    end: u64,
}

impl PhysicalRange {
    const EMPTY: Self = Self { start: 0, end: 0 };

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    fn contains(self, address: u64) -> bool {
        self.start <= address && address < self.end
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EfiMemoryDescriptor {
    memory_type: u32,
    _padding: u32,
    physical_start: u64,
    _virtual_start: u64,
    number_of_pages: u64,
    _attributes: u64,
}

fn physical_address_limit() -> Result<u64, MemoryError> {
    x86::cpuid::CpuId::new()
        .get_processor_capacity_feature_info()
        .map(|info| info.physical_address_bits())
        .filter(|bits| (32..=52).contains(bits))
        .and_then(|bits| 1u64.checked_shl(u32::from(bits)))
        .ok_or(MemoryError::InvalidPhysicalAddressWidth)
}

fn read_descriptor(
    memory_map: &[u8],
    descriptor_size: usize,
    index: usize,
) -> Option<EfiMemoryDescriptor> {
    let offset = index.checked_mul(descriptor_size)?;
    let end = offset.checked_add(size_of::<EfiMemoryDescriptor>())?;
    let descriptor_bytes = memory_map.get(offset..end)?;
    // SAFETY: the slice bounds-check above guarantees the complete descriptor
    // is readable; UEFI descriptor records need not be naturally aligned.
    Some(unsafe {
        descriptor_bytes
            .as_ptr()
            .cast::<EfiMemoryDescriptor>()
            .read_unaligned()
    })
}

fn validate_memory_map(
    memory_map: &[u8],
    descriptor_size: usize,
    physical_limit: u64,
) -> Result<usize, MemoryError> {
    if descriptor_size < EFI_DESCRIPTOR_MIN_SIZE
        || memory_map.is_empty()
        || memory_map.len() % descriptor_size != 0
        || physical_limit <= PAGE_SIZE
    {
        return Err(MemoryError::InvalidMemoryMap);
    }

    let descriptor_count = memory_map.len() / descriptor_size;
    for index in 0..descriptor_count {
        let descriptor = read_descriptor(memory_map, descriptor_size, index)
            .ok_or(MemoryError::InvalidMemoryMap)?;
        let region_size = descriptor
            .number_of_pages
            .checked_mul(PAGE_SIZE)
            .ok_or(MemoryError::InvalidMemoryMap)?;
        let region_end = descriptor
            .physical_start
            .checked_add(region_size)
            .ok_or(MemoryError::InvalidMemoryMap)?;

        if descriptor.number_of_pages == 0
            || descriptor.physical_start & PAGE_MASK != 0
            || descriptor.physical_start >= physical_limit
            || region_end > physical_limit
        {
            return Err(MemoryError::InvalidMemoryMap);
        }

        let current = PhysicalRange {
            start: descriptor.physical_start,
            end: region_end,
        };
        for other_index in 0..index {
            let other = read_descriptor(memory_map, descriptor_size, other_index)
                .ok_or(MemoryError::InvalidMemoryMap)?;
            let other_size = other
                .number_of_pages
                .checked_mul(PAGE_SIZE)
                .ok_or(MemoryError::InvalidMemoryMap)?;
            let other_end = other
                .physical_start
                .checked_add(other_size)
                .ok_or(MemoryError::InvalidMemoryMap)?;
            if current.overlaps(PhysicalRange {
                start: other.physical_start,
                end: other_end,
            }) {
                return Err(MemoryError::InvalidMemoryMap);
            }
        }
    }

    Ok(descriptor_count)
}

fn page_covering_range(
    address: u64,
    size: u64,
    physical_limit: u64,
) -> Result<PhysicalRange, MemoryError> {
    if size == 0 {
        return Ok(PhysicalRange::EMPTY);
    }
    let end = address
        .checked_add(size)
        .ok_or(MemoryError::InvalidBootInfo)?;
    let aligned_start = address & !PAGE_MASK;
    let aligned_end = end
        .checked_add(PAGE_MASK)
        .ok_or(MemoryError::InvalidBootInfo)?
        & !PAGE_MASK;
    if aligned_end > physical_limit || aligned_start >= aligned_end {
        return Err(MemoryError::InvalidBootInfo);
    }
    Ok(PhysicalRange {
        start: aligned_start,
        end: aligned_end,
    })
}

fn reserved_ranges(
    boot_info: &BootInfo,
    boot_info_address: u64,
    physical_limit: u64,
) -> Result<[PhysicalRange; 6], MemoryError> {
    let map_address = boot_info.memory_map as usize as u64;
    let system_table_address = boot_info.system_table as usize as u64;

    Ok([
        page_covering_range(
            boot_info.kernel_base,
            boot_info.kernel_reserved_size,
            physical_limit,
        )?,
        page_covering_range(
            boot_info_address,
            size_of::<BootInfo>() as u64,
            physical_limit,
        )?,
        page_covering_range(
            map_address,
            u64::try_from(boot_info.memory_map_size).map_err(|_| MemoryError::InvalidBootInfo)?,
            physical_limit,
        )?,
        page_covering_range(boot_info.page_table_root, PAGE_SIZE, physical_limit)?,
        page_covering_range(system_table_address, PAGE_SIZE, physical_limit)?,
        page_covering_range(
            boot_info.framebuffer_base,
            boot_info.framebuffer_size,
            physical_limit,
        )?,
    ])
}

pub struct BootInfoFrameAllocator<'a> {
    memory_map: &'a [u8],
    descriptor_size: usize,
    descriptor_count: usize,
    descriptor_index: usize,
    page_index: u64,
    physical_limit: u64,
    reserved_ranges: [PhysicalRange; 6],
    reclaimed_frames: [Option<PhysFrame<Size4KiB>>; RECLAIMED_FRAME_CAPACITY],
    reclaimed_count: usize,
}

impl<'a> BootInfoFrameAllocator<'a> {
    unsafe fn new(
        boot_info: &'a BootInfo,
        boot_info_address: u64,
        physical_limit: u64,
    ) -> Result<Self, MemoryError> {
        let descriptor_size = boot_info.descriptor_size;
        let memory_map_size = boot_info.memory_map_size;

        if boot_info.memory_map.is_null() || memory_map_size == 0 {
            return Err(MemoryError::MissingMemoryMap);
        }
        if descriptor_size < EFI_DESCRIPTOR_MIN_SIZE
            || memory_map_size > isize::MAX as usize
            || memory_map_size % descriptor_size != 0
        {
            return Err(MemoryError::InvalidMemoryMap);
        }

        // UEFI keeps the loader-data memory map alive after ExitBootServices.
        // Its validity and mapping are part of this function's unsafe contract.
        let memory_map = unsafe {
            core::slice::from_raw_parts(boot_info.memory_map.cast::<u8>(), memory_map_size)
        };
        let descriptor_count = validate_memory_map(memory_map, descriptor_size, physical_limit)?;
        let reserved_ranges = reserved_ranges(boot_info, boot_info_address, physical_limit)?;

        for index in 0..descriptor_count {
            let descriptor = read_descriptor(memory_map, descriptor_size, index)
                .ok_or(MemoryError::InvalidMemoryMap)?;
            if descriptor.memory_type == EFI_CONVENTIONAL_MEMORY {
                let region_size = descriptor.number_of_pages * PAGE_SIZE;
                let range = PhysicalRange {
                    start: descriptor.physical_start,
                    end: descriptor.physical_start + region_size,
                };
                if reserved_ranges
                    .iter()
                    .any(|reserved| range.overlaps(*reserved))
                {
                    return Err(MemoryError::InvalidMemoryMap);
                }
            }
        }

        Ok(Self {
            memory_map,
            descriptor_size,
            descriptor_count,
            descriptor_index: 0,
            page_index: 0,
            physical_limit,
            reserved_ranges,
            reclaimed_frames: [None; RECLAIMED_FRAME_CAPACITY],
            reclaimed_count: 0,
        })
    }

    fn next_usable_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        if self.reclaimed_count != 0 {
            self.reclaimed_count -= 1;
            return self.reclaimed_frames[self.reclaimed_count].take();
        }

        while self.descriptor_index < self.descriptor_count {
            let descriptor =
                read_descriptor(self.memory_map, self.descriptor_size, self.descriptor_index)
                    .expect("validated UEFI memory-map descriptor");

            if descriptor.memory_type != EFI_CONVENTIONAL_MEMORY
                || self.page_index >= descriptor.number_of_pages
            {
                self.descriptor_index += 1;
                self.page_index = 0;
                continue;
            }

            let address = descriptor.physical_start + self.page_index * PAGE_SIZE;
            self.page_index += 1;
            if address >= self.physical_limit
                || self
                    .reserved_ranges
                    .iter()
                    .any(|range| range.contains(address))
            {
                continue;
            }
            if let Ok(frame) = PhysFrame::from_start_address(PhysAddr::new(address)) {
                return Some(frame);
            }
        }

        None
    }

    fn reclaim_frame(&mut self, frame: PhysFrame<Size4KiB>) -> bool {
        if self.reclaimed_count < RECLAIMED_FRAME_CAPACITY {
            self.reclaimed_frames[self.reclaimed_count] = Some(frame);
            self.reclaimed_count += 1;
            true
        } else {
            false
        }
    }

    fn is_allocatable_frame(&self, frame: PhysFrame<Size4KiB>) -> bool {
        let address = frame.start_address().as_u64();
        if address >= self.physical_limit
            || self
                .reserved_ranges
                .iter()
                .any(|range| range.contains(address))
        {
            return false;
        }

        (0..self.descriptor_count).any(|index| {
            let descriptor = read_descriptor(self.memory_map, self.descriptor_size, index)
                .expect("validated UEFI memory-map descriptor");
            descriptor.memory_type == EFI_CONVENTIONAL_MEMORY
                && descriptor.physical_start <= address
                && address < descriptor.physical_start + descriptor.number_of_pages * PAGE_SIZE
        })
    }
}

// SAFETY: construction validates all descriptors, excludes loader-owned
// ranges, and the cursor visits each conventional frame at most once.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.next_usable_frame()
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator<'_> {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        assert!(
            self.reclaim_frame(frame),
            "internal page-table frame reclaim capacity exceeded"
        );
    }
}

pub struct MemoryManager<'a> {
    mapper: OffsetPageTable<'static>,
    frame_allocator: BootInfoFrameAllocator<'a>,
    physical_limit: u64,
}

static MEMORY_MANAGER: Once<Result<Mutex<MemoryManager<'static>>, MemoryError>> = Once::new();

impl MemoryManager<'_> {
    pub fn translate_addr(&self, address: VirtAddr) -> Option<PhysAddr> {
        self.mapper.translate_addr(address)
    }

    pub fn physical_memory_offset(&self) -> VirtAddr {
        self.mapper.phys_offset()
    }

    pub fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.frame_allocator.allocate_frame()
    }

    /// Returns a frame previously allocated by this manager to its allocator.
    ///
    /// # Safety
    ///
    /// The frame must be owned by the caller, no longer mapped or otherwise in
    /// use, and must not already have been returned. Violating this contract
    /// can cause the same physical frame to be handed out more than once.
    pub unsafe fn deallocate_frame(
        &mut self,
        frame: PhysFrame<Size4KiB>,
    ) -> Result<(), MemoryError> {
        if !self.frame_allocator.is_allocatable_frame(frame) {
            return Err(MemoryError::InvalidMemoryMap);
        }
        if !self.frame_allocator.reclaim_frame(frame) {
            return Err(MemoryError::FrameReclaimCapacityExceeded);
        }
        Ok(())
    }

    pub fn first_unused_low_half_page(&self) -> Option<Page<Size4KiB>> {
        self.mapper
            .level_4_table()
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, entry)| entry.is_unused())
            .map(|(index, _)| Page::containing_address(VirtAddr::new((index as u64) << 39)))
    }

    /// Maps a caller-owned frame and invalidates this CPU's translation.
    ///
    /// On failure, newly created empty page tables are pruned and their frames
    /// returned to the allocator. The supplied frame remains caller-owned.
    ///
    /// # Safety
    ///
    /// The page must be unused, the frame must be exclusively owned or
    /// intentionally shared without creating Rust aliases, and the mapping
    /// flags must be valid for all existing aliases. This kernel currently
    /// runs on one CPU. SMP requires TLB shootdowns on every CPU that can use
    /// the address space before the mapping is used or reclaimed.
    pub unsafe fn map_to(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<(), MappingError> {
        let frame_start = frame.start_address().as_u64();
        if frame_start >= self.physical_limit || frame_start + PAGE_SIZE > self.physical_limit {
            return Err(MappingError::PhysicalAddressOutOfRange);
        }
        match unsafe {
            self.mapper
                .map_to(page, frame, flags, &mut self.frame_allocator)
        } {
            Ok(flush) => {
                flush.flush();
                Ok(())
            }
            Err(error) => {
                unsafe {
                    self.mapper.clean_up_addr_range(
                        PageRangeInclusive {
                            start: page,
                            end: page,
                        },
                        &mut self.frame_allocator,
                    );
                }
                x86_64::instructions::tlb::flush(page.start_address());
                Err(MappingError::Mapper(error))
            }
        }
    }

    /// Allocates a frame and maps it, returning the frame to the allocator if
    /// mapping fails.
    ///
    /// # Safety
    ///
    /// The virtual page must be unused and the flags must not create
    /// conflicting aliases. The same single-CPU and future TLB shootdown
    /// requirements as [`Self::map_to`] apply.
    pub unsafe fn allocate_and_map(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<PhysFrame<Size4KiB>, MappingError> {
        let frame = self
            .frame_allocator
            .allocate_frame()
            .ok_or(MappingError::Mapper(MapToError::FrameAllocationFailed))?;
        match unsafe { self.map_to(page, frame, flags) } {
            Ok(()) => Ok(frame),
            Err(error) => {
                assert!(
                    self.frame_allocator.reclaim_frame(frame),
                    "internal allocated-frame reclaim capacity exceeded"
                );
                Err(error)
            }
        }
    }

    /// Unmaps a frame allocated by this manager and returns it to the allocator.
    ///
    /// # Safety
    ///
    /// The mapping must refer to a frame allocated by this manager, with no
    /// other mappings or users. Keep use of this operation to the single CPU.
    pub unsafe fn unmap_and_deallocate(
        &mut self,
        page: Page<Size4KiB>,
    ) -> Result<(), UnmappingError> {
        if self.frame_allocator.reclaimed_count == RECLAIMED_FRAME_CAPACITY {
            return Err(UnmappingError::ReclaimCapacityExceeded);
        }
        let mapped_address = self
            .mapper
            .translate_addr(page.start_address())
            .ok_or(UnmappingError::PageNotMapped)?;
        let frame = PhysFrame::containing_address(mapped_address);
        if !self.frame_allocator.is_allocatable_frame(frame) {
            return Err(UnmappingError::FrameNotAllocatorOwned);
        }

        let (unmapped_frame, flush) = self.mapper.unmap(page).map_err(UnmappingError::Mapper)?;
        assert_eq!(unmapped_frame, frame, "mapping changed during unmap");
        flush.flush();
        assert!(self.frame_allocator.reclaim_frame(frame));
        Ok(())
    }
}

/// Returns exclusive access to the active level-4 page table.
///
/// # Safety
///
/// The caller must ensure `physical_memory_offset + CR3` is a canonical,
/// page-aligned virtual address mapped to the active root frame by the current
/// page tables. It must also guarantee the pointed-to table remains live and
/// that no mutable or shared Rust reference aliases it for the returned
/// lifetime. The memory manager calls this exactly once during one-shot
/// initialization while boot-time interrupts are disabled.
unsafe fn active_level_4_table(
    physical_memory_offset: VirtAddr,
) -> Result<&'static mut PageTable, MemoryError> {
    let (level_4_table_frame, _) = Cr3::read();
    let physical_address = level_4_table_frame.start_address();
    let virtual_address = physical_memory_offset
        .as_u64()
        .checked_add(physical_address.as_u64())
        .and_then(|address| VirtAddr::try_new(address).ok())
        .ok_or(MemoryError::InvalidPhysicalMemoryOffset)?;

    if physical_address.as_u64() & PAGE_MASK != 0 || virtual_address.as_u64() & PAGE_MASK != 0 {
        return Err(MemoryError::InvalidPageTableRoot);
    }

    let page_table_pointer = virtual_address.as_mut_ptr::<PageTable>();
    // SAFETY: guaranteed by this function's caller after validating the
    // loader-provided direct-map offset and CR3 root.
    Ok(unsafe { &mut *page_table_pointer })
}

/// Constructs a manager over the loader-installed direct map.
///
/// # Safety
///
/// `boot_info` and its memory map must be valid, correctly aligned/readable,
/// and remain alive for the kernel lifetime. The loader must have transferred
/// ownership of its final UEFI memory map and retained all non-conventional
/// allocations as reserved descriptors. The direct map must map every RAM
/// frame (including the active page-table root) at `physical_memory_offset`.
/// Initialization must happen once, before interrupts or other CPUs can
/// access the mapper.
unsafe fn create<'a>(
    boot_info: &'a BootInfo,
    boot_info_address: u64,
) -> Result<MemoryManager<'a>, MemoryError> {
    if boot_info.version != EFI_MEMORY_MAP_VERSION
        || boot_info.descriptor_version != EFI_MEMORY_DESCRIPTOR_VERSION
        || boot_info.page_table_root & PAGE_MASK != 0
    {
        return Err(MemoryError::InvalidBootInfo);
    }

    let (active_root, _) = Cr3::read();
    if active_root.start_address().as_u64() != boot_info.page_table_root {
        return Err(MemoryError::InvalidPageTableRoot);
    }

    let physical_limit = physical_address_limit()?;
    let root_address = PhysAddr::try_new(boot_info.page_table_root)
        .map_err(|_| MemoryError::InvalidPageTableRoot)?;
    if boot_info.page_table_root >= physical_limit {
        return Err(MemoryError::InvalidPageTableRoot);
    }

    let physical_memory_offset = VirtAddr::try_new(boot_info.physical_memory_offset)
        .map_err(|_| MemoryError::InvalidPhysicalMemoryOffset)?;
    let root_virtual_address = boot_info
        .physical_memory_offset
        .checked_add(root_address.as_u64())
        .and_then(|address| VirtAddr::try_new(address).ok())
        .ok_or(MemoryError::InvalidPhysicalMemoryOffset)?;

    let frame_allocator =
        unsafe { BootInfoFrameAllocator::new(boot_info, boot_info_address, physical_limit)? };
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset)? };
    let mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };

    if mapper.translate_addr(root_virtual_address) != Some(root_address) {
        return Err(MemoryError::InvalidPhysicalMemoryOffset);
    }

    Ok(MemoryManager {
        mapper,
        frame_allocator,
        physical_limit,
    })
}

/// Initializes the global memory manager exactly once during single-CPU boot.
///
/// Success or failure is permanent: a failed handoff is fatal for this boot,
/// so callers must not expect a retry with modified `BootInfo`.
///
/// # Safety
///
/// `boot_info` must be the live, loader-owned structure passed at kernel entry,
/// and its memory-map allocation must remain readable for the manager's
/// lifetime. Call only before enabling interrupts or starting other CPUs.
pub unsafe fn initialize(boot_info: &'static BootInfo) -> Result<(), MemoryError> {
    let boot_info_address = boot_info as *const BootInfo as u64;
    let result = MEMORY_MANAGER
        .call_once(|| unsafe { create(boot_info, boot_info_address) }.map(Mutex::new));
    result.as_ref().map(|_| ()).map_err(|error| *error)
}

/// Runs with local interrupts disabled while holding the manager lock.
///
/// SMP still requires TLB shootdowns before paging changes are safe.
pub fn with_manager<R>(
    operation: impl FnOnce(&mut MemoryManager<'static>) -> R,
) -> Result<R, MemoryError> {
    let initialized = MEMORY_MANAGER.get().ok_or(MemoryError::NotInitialized)?;
    let manager = initialized.as_ref().map_err(|error| *error)?;
    Ok(x86_64::instructions::interrupts::without_interrupts(|| {
        let mut manager = manager.lock();
        operation(&mut manager)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MAP_CAPACITY: usize = 192;

    fn descriptor(memory_type: u32, start: u64, pages: u64) -> EfiMemoryDescriptor {
        EfiMemoryDescriptor {
            memory_type,
            _padding: 0,
            physical_start: start,
            _virtual_start: 0,
            number_of_pages: pages,
            _attributes: 0,
        }
    }

    fn map_bytes(
        descriptors: &[EfiMemoryDescriptor],
        descriptor_size: usize,
    ) -> ([u8; TEST_MAP_CAPACITY], usize) {
        let mut bytes = [0; TEST_MAP_CAPACITY];
        let length = descriptors.len() * descriptor_size;
        assert!(length <= TEST_MAP_CAPACITY);
        for (index, descriptor) in descriptors.iter().enumerate() {
            unsafe {
                bytes
                    .as_mut_ptr()
                    .add(index * descriptor_size)
                    .cast::<EfiMemoryDescriptor>()
                    .write_unaligned(*descriptor);
            }
        }
        (bytes, length)
    }

    #[test_case]
    fn validates_descriptor_layout_and_ranges() {
        let (valid, length) = map_bytes(&[descriptor(EFI_CONVENTIONAL_MEMORY, 0x1000, 4)], 48);
        assert_eq!(validate_memory_map(&valid[..length], 48, 1 << 32), Ok(1));
        assert_eq!(
            validate_memory_map(&valid[..length], EFI_DESCRIPTOR_MIN_SIZE - 1, 1 << 32),
            Err(MemoryError::InvalidMemoryMap)
        );
        assert_eq!(
            validate_memory_map(&valid[..length - 1], 48, 1 << 32),
            Err(MemoryError::InvalidMemoryMap)
        );
    }

    #[test_case]
    fn rejects_misaligned_empty_overflowing_and_out_of_width_ranges() {
        for invalid in [
            descriptor(EFI_CONVENTIONAL_MEMORY, 1, 1),
            descriptor(EFI_CONVENTIONAL_MEMORY, 0x1000, 0),
            descriptor(EFI_CONVENTIONAL_MEMORY, u64::MAX - PAGE_MASK, 2),
            descriptor(EFI_CONVENTIONAL_MEMORY, 1 << 32, 1),
        ] {
            let (bytes, length) = map_bytes(&[invalid], 40);
            assert_eq!(
                validate_memory_map(&bytes[..length], 40, 1 << 32),
                Err(MemoryError::InvalidMemoryMap)
            );
        }
    }

    #[test_case]
    fn rejects_overlapping_descriptors_in_either_order() {
        for descriptors in [
            [
                descriptor(EFI_CONVENTIONAL_MEMORY, 0x1000, 3),
                descriptor(1, 0x3000, 2),
            ],
            [
                descriptor(1, 0x3000, 2),
                descriptor(EFI_CONVENTIONAL_MEMORY, 0x1000, 3),
            ],
        ] {
            let (bytes, length) = map_bytes(&descriptors, 40);
            assert_eq!(
                validate_memory_map(&bytes[..length], 40, 1 << 32),
                Err(MemoryError::InvalidMemoryMap)
            );
        }
    }

    #[test_case]
    fn allocator_skips_explicit_reserved_frames() {
        let (bytes, length) = map_bytes(
            &[descriptor(EFI_CONVENTIONAL_MEMORY, 0x1000, 4)],
            EFI_DESCRIPTOR_MIN_SIZE,
        );
        let reserved = [
            PhysicalRange {
                start: 0x2000,
                end: 0x3000,
            },
            PhysicalRange::EMPTY,
            PhysicalRange::EMPTY,
            PhysicalRange::EMPTY,
            PhysicalRange::EMPTY,
            PhysicalRange::EMPTY,
        ];
        let mut allocator = BootInfoFrameAllocator {
            memory_map: &bytes[..length],
            descriptor_size: EFI_DESCRIPTOR_MIN_SIZE,
            descriptor_count: 1,
            descriptor_index: 0,
            page_index: 0,
            physical_limit: 1 << 32,
            reserved_ranges: reserved,
            reclaimed_frames: [None; RECLAIMED_FRAME_CAPACITY],
            reclaimed_count: 0,
        };

        assert_eq!(
            allocator
                .allocate_frame()
                .map(|frame| frame.start_address().as_u64()),
            Some(0x1000)
        );
        assert_eq!(
            allocator
                .allocate_frame()
                .map(|frame| frame.start_address().as_u64()),
            Some(0x3000)
        );
        assert_eq!(
            allocator
                .allocate_frame()
                .map(|frame| frame.start_address().as_u64()),
            Some(0x4000)
        );
        assert_eq!(allocator.allocate_frame(), None);
    }
}
