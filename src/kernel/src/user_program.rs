use crate::memory::{
    self, AddressSpace, AddressSpaceError, MemoryError, USER_SPACE_START, UserPageAccess,
};
use crate::scheduler::{self, SchedulerError, TaskId};
use alloc::vec::Vec;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, Size4KiB},
};

const PAGE_SIZE: u64 = 4096;
pub const RAW_PROGRAM_BASE: u64 = USER_SPACE_START + PAGE_SIZE;
pub const RAW_PROGRAM_MAX_SIZE: usize = 64 * 1024;
const APP_MAGIC: &[u8; 8] = b"LEAFAPP\0";
const APP_VERSION: u16 = 1;
const APP_HEADER_SIZE: usize = 48;
const APP_MAX_MEMORY_SIZE: usize = 64 * 1024;
pub const MAX_PROGRAM_ARGUMENTS: usize = 16;
pub const MAX_PROGRAM_ARGUMENT_BYTES: usize = 1024;
pub const USER_STACK_SIZE: usize = 16 * 1024;
pub const USER_STACK_TOP: u64 = USER_SPACE_START + 0x20_0000;
const USER_STACK_ARGUMENT_RESERVE: u64 = 256;

#[derive(Debug)]
pub enum ProgramError {
    InvalidExtension,
    FileNotFound,
    FileSystem(crate::fs::FsError),
    Ext2(crate::fs::ext2::Ext2Error),
    EmptyImage,
    ImageTooLarge,
    InvalidAppFormat,
    UnsupportedAppVersion,
    InvalidArguments,
    Memory(MemoryError),
    AddressSpace(AddressSpaceError),
    AddressSpaceCleanup(AddressSpaceError),
    InvalidPhysicalAddress,
    Scheduler(SchedulerError),
}

pub struct LoadedProgram {
    address_space: AddressSpace,
    entry: VirtAddr,
    stack: VirtAddr,
    argument_count: u64,
    argument_vector: VirtAddr,
    data_start: Option<VirtAddr>,
}

impl LoadedProgram {
    pub fn user_page_access(&self, address: VirtAddr) -> Option<UserPageAccess> {
        self.address_space
            .user_page_access(Page::<Size4KiB>::containing_address(address))
    }

    pub fn data_start(&self) -> Option<VirtAddr> {
        self.data_start
    }

    pub fn spawn(self) -> Result<TaskId, ProgramError> {
        self.spawn_with_fd_mappings(&[])
    }

    pub fn spawn_with_fd_mappings(
        self,
        fd_mappings: &[scheduler::FdMapping],
    ) -> Result<TaskId, ProgramError> {
        self.spawn_with_fd_mappings_and_closes(fd_mappings, &[])
    }

    pub fn spawn_with_fd_mappings_and_closes(
        self,
        fd_mappings: &[scheduler::FdMapping],
        fd_closes: &[usize],
    ) -> Result<TaskId, ProgramError> {
        scheduler::spawn_user_with_arguments_and_fds(
            self.address_space,
            self.entry,
            self.stack,
            self.argument_count,
            self.argument_vector,
            fd_mappings,
            fd_closes,
        )
        .map_err(ProgramError::Scheduler)
    }
}

/// Load a raw `.bin` image or versioned `.app` image from the read-only initrd.
///
/// Both formats use fixed, non-relocatable user addresses. Code is read/execute,
/// initialized data and BSS are read/write, and the stack is read/write.
pub fn load(path: &str) -> Result<LoadedProgram, ProgramError> {
    load_with_arguments(path, &[])
}

/// Load a program and prepare its argv vector. Arguments exclude argv[0],
/// which is populated from `path`.
pub fn load_with_arguments(path: &str, arguments: &[&str]) -> Result<LoadedProgram, ProgramError> {
    if !path.ends_with(".bin") && !path.ends_with(".app") {
        return Err(ProgramError::InvalidExtension);
    }
    let image = crate::fs::read_file(path)
        .map_err(ProgramError::FileSystem)?
        .ok_or(ProgramError::FileNotFound)?;
    load_image(path, image, arguments)
}

/// Load a bounded raw `.bin` or versioned `.app` image from the ext2 data disk.
pub fn load_from_ext2(path: &str) -> Result<LoadedProgram, ProgramError> {
    load_from_ext2_with_arguments(path, &[])
}

pub fn load_from_ext2_with_arguments(
    path: &str,
    arguments: &[&str],
) -> Result<LoadedProgram, ProgramError> {
    let max_size = if path.ends_with(".bin") {
        RAW_PROGRAM_MAX_SIZE
    } else if path.ends_with(".app") {
        APP_HEADER_SIZE
            .checked_add(APP_MAX_MEMORY_SIZE)
            .ok_or(ProgramError::ImageTooLarge)?
    } else {
        return Err(ProgramError::InvalidExtension);
    };
    let image = crate::fs::ext2::read_file_vec(path, max_size)
        .map_err(|error| match error {
            crate::fs::ext2::Ext2Error::FileTooLarge => ProgramError::ImageTooLarge,
            error => ProgramError::Ext2(error),
        })?
        .ok_or(ProgramError::FileNotFound)?;
    load_image(path, &image, arguments)
}

fn load_image(path: &str, image: &[u8], arguments: &[&str]) -> Result<LoadedProgram, ProgramError> {
    if path.ends_with(".bin") {
        load_raw_image(image, path, arguments)
    } else if path.ends_with(".app") {
        load_app_image(image, path, arguments)
    } else {
        Err(ProgramError::InvalidExtension)
    }
}

pub fn launch(path: &str) -> Result<TaskId, ProgramError> {
    load(path)?.spawn()
}

pub fn launch_with_arguments(path: &str, arguments: &[&str]) -> Result<TaskId, ProgramError> {
    load_with_arguments(path, arguments)?.spawn()
}

pub fn launch_from_ext2(path: &str) -> Result<TaskId, ProgramError> {
    load_from_ext2(path)?.spawn()
}

pub fn launch_from_ext2_with_arguments(
    path: &str,
    arguments: &[&str],
) -> Result<TaskId, ProgramError> {
    load_from_ext2_with_arguments(path, arguments)?.spawn()
}

#[derive(Clone, Copy)]
struct AppLayout {
    entry_offset: usize,
    code_len: usize,
    data_offset: usize,
    data_len: usize,
    bss_len: usize,
}

fn read_u16(image: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        image.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(image: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        image.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(image: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        image.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn parse_app_image(image: &[u8]) -> Result<AppLayout, ProgramError> {
    if image.len() < APP_HEADER_SIZE || image.get(..8) != Some(APP_MAGIC.as_slice()) {
        return Err(ProgramError::InvalidAppFormat);
    }
    let version = read_u16(image, 8).ok_or(ProgramError::InvalidAppFormat)?;
    if version != APP_VERSION {
        return Err(ProgramError::UnsupportedAppVersion);
    }
    if read_u16(image, 10) != Some(APP_HEADER_SIZE as u16) || read_u32(image, 12) != Some(0) {
        return Err(ProgramError::InvalidAppFormat);
    }

    let entry_offset = usize::try_from(read_u64(image, 16).ok_or(ProgramError::InvalidAppFormat)?)
        .map_err(|_| ProgramError::InvalidAppFormat)?;
    let code_len = usize::try_from(read_u64(image, 24).ok_or(ProgramError::InvalidAppFormat)?)
        .map_err(|_| ProgramError::ImageTooLarge)?;
    let data_len = usize::try_from(read_u64(image, 32).ok_or(ProgramError::InvalidAppFormat)?)
        .map_err(|_| ProgramError::ImageTooLarge)?;
    let bss_len = usize::try_from(read_u64(image, 40).ok_or(ProgramError::InvalidAppFormat)?)
        .map_err(|_| ProgramError::ImageTooLarge)?;

    if code_len == 0 {
        return Err(ProgramError::EmptyImage);
    }
    if entry_offset >= code_len {
        return Err(ProgramError::InvalidAppFormat);
    }
    let memory_size = code_len
        .checked_add(data_len)
        .and_then(|size| size.checked_add(bss_len))
        .ok_or(ProgramError::ImageTooLarge)?;
    if memory_size > APP_MAX_MEMORY_SIZE {
        return Err(ProgramError::ImageTooLarge);
    }
    let data_offset = APP_HEADER_SIZE
        .checked_add(code_len)
        .ok_or(ProgramError::InvalidAppFormat)?;
    let file_size = data_offset
        .checked_add(data_len)
        .ok_or(ProgramError::InvalidAppFormat)?;
    if file_size != image.len() {
        return Err(ProgramError::InvalidAppFormat);
    }

    Ok(AppLayout {
        entry_offset,
        code_len,
        data_offset,
        data_len,
        bss_len,
    })
}

fn load_app_image(
    image: &[u8],
    path: &str,
    arguments: &[&str],
) -> Result<LoadedProgram, ProgramError> {
    let layout = parse_app_image(image)?;
    let code_start = APP_HEADER_SIZE;
    let data_end = layout
        .data_offset
        .checked_add(layout.data_len)
        .ok_or(ProgramError::InvalidAppFormat)?;
    let code = image
        .get(code_start..layout.data_offset)
        .ok_or(ProgramError::InvalidAppFormat)?;
    let data = image
        .get(layout.data_offset..data_end)
        .ok_or(ProgramError::InvalidAppFormat)?;
    load_segments(
        code,
        data,
        layout.bss_len,
        layout.entry_offset,
        path,
        arguments,
    )
}

fn load_raw_image(
    image: &[u8],
    path: &str,
    arguments: &[&str],
) -> Result<LoadedProgram, ProgramError> {
    if image.is_empty() {
        return Err(ProgramError::EmptyImage);
    }
    if image.len() > RAW_PROGRAM_MAX_SIZE {
        return Err(ProgramError::ImageTooLarge);
    }
    load_segments(image, &[], 0, 0, path, arguments)
}

fn build_argument_vector(path: &str, arguments: &[&str]) -> Result<(Vec<u8>, u64), ProgramError> {
    if arguments.len() > MAX_PROGRAM_ARGUMENTS
        || path.as_bytes().contains(&0)
        || arguments
            .iter()
            .any(|argument| argument.as_bytes().contains(&0))
    {
        return Err(ProgramError::InvalidArguments);
    }
    let argument_count = arguments
        .len()
        .checked_add(1)
        .ok_or(ProgramError::InvalidArguments)?;
    let pointer_bytes = argument_count
        .checked_add(1)
        .and_then(|count| count.checked_mul(core::mem::size_of::<u64>()))
        .ok_or(ProgramError::InvalidArguments)?;
    let strings_bytes = arguments
        .iter()
        .try_fold(
            path.len()
                .checked_add(1)
                .ok_or(ProgramError::InvalidArguments)?,
            |total, argument| total.checked_add(argument.len())?.checked_add(1),
        )
        .ok_or(ProgramError::InvalidArguments)?;
    if strings_bytes > MAX_PROGRAM_ARGUMENT_BYTES {
        return Err(ProgramError::InvalidArguments);
    }
    let unaligned_size = pointer_bytes
        .checked_add(strings_bytes)
        .ok_or(ProgramError::InvalidArguments)?;
    let total_size = unaligned_size
        .checked_add(7)
        .map(|size| size & !7)
        .ok_or(ProgramError::InvalidArguments)?;
    if total_size > USER_STACK_SIZE {
        return Err(ProgramError::InvalidArguments);
    }

    let stack_start = USER_STACK_TOP
        .checked_sub(USER_STACK_ARGUMENT_RESERVE)
        .and_then(|address| address.checked_sub(total_size as u64))
        .ok_or(ProgramError::InvalidArguments)?;
    let mut stack_data = Vec::new();
    stack_data
        .try_reserve_exact(total_size)
        .map_err(|_| ProgramError::InvalidArguments)?;
    stack_data.resize(total_size, 0);

    let mut string_offset = pointer_bytes;
    for (index, argument) in core::iter::once(path)
        .chain(arguments.iter().copied())
        .enumerate()
    {
        let string_address = stack_start
            .checked_add(string_offset as u64)
            .ok_or(ProgramError::InvalidArguments)?;
        let pointer_offset = index
            .checked_mul(core::mem::size_of::<u64>())
            .ok_or(ProgramError::InvalidArguments)?;
        stack_data[pointer_offset..pointer_offset + 8]
            .copy_from_slice(&string_address.to_le_bytes());
        let string_end = string_offset
            .checked_add(argument.len())
            .ok_or(ProgramError::InvalidArguments)?;
        stack_data[string_offset..string_end].copy_from_slice(argument.as_bytes());
        string_offset = string_end + 1;
    }
    Ok((
        stack_data,
        u64::try_from(argument_count).map_err(|_| ProgramError::InvalidArguments)?,
    ))
}

fn load_segments(
    code: &[u8],
    data: &[u8],
    bss_len: usize,
    entry_offset: usize,
    path: &str,
    arguments: &[&str],
) -> Result<LoadedProgram, ProgramError> {
    if code.is_empty() {
        return Err(ProgramError::EmptyImage);
    }
    let data_memory_len = data
        .len()
        .checked_add(bss_len)
        .ok_or(ProgramError::ImageTooLarge)?;
    let memory_size = code
        .len()
        .checked_add(data_memory_len)
        .ok_or(ProgramError::ImageTooLarge)?;
    if memory_size > APP_MAX_MEMORY_SIZE {
        return Err(ProgramError::ImageTooLarge);
    }
    if entry_offset >= code.len() {
        return Err(ProgramError::InvalidAppFormat);
    }
    let code_page_count = code.len().div_ceil(PAGE_SIZE as usize);
    let data_page_count = data_memory_len.div_ceil(PAGE_SIZE as usize);
    let data_start = RAW_PROGRAM_BASE
        .checked_add(
            u64::try_from(code_page_count)
                .map_err(|_| ProgramError::ImageTooLarge)?
                .checked_mul(PAGE_SIZE)
                .ok_or(ProgramError::ImageTooLarge)?,
        )
        .ok_or(ProgramError::ImageTooLarge)?;
    let data_mapped_size = u64::try_from(data_page_count)
        .map_err(|_| ProgramError::ImageTooLarge)?
        .checked_mul(PAGE_SIZE)
        .ok_or(ProgramError::ImageTooLarge)?;
    let data_end = data_start
        .checked_add(data_mapped_size)
        .ok_or(ProgramError::ImageTooLarge)?;
    let stack_start = USER_STACK_TOP
        .checked_sub(USER_STACK_SIZE as u64)
        .ok_or(ProgramError::ImageTooLarge)?;
    if data_end > stack_start.saturating_sub(PAGE_SIZE) {
        return Err(ProgramError::ImageTooLarge);
    }
    let (argument_data, argument_count) = build_argument_vector(path, arguments)?;
    let argument_vector = VirtAddr::new(
        USER_STACK_TOP
            .checked_sub(USER_STACK_ARGUMENT_RESERVE)
            .and_then(|address| address.checked_sub(argument_data.len() as u64))
            .ok_or(ProgramError::InvalidArguments)?,
    );

    let stack_page_count = USER_STACK_SIZE / PAGE_SIZE as usize;
    let mut code_pages = Vec::new();
    code_pages
        .try_reserve_exact(code_page_count)
        .map_err(|_| ProgramError::AddressSpace(AddressSpaceError::AllocationFailed))?;
    let mut data_pages = Vec::new();
    data_pages
        .try_reserve_exact(data_page_count)
        .map_err(|_| ProgramError::AddressSpace(AddressSpaceError::AllocationFailed))?;
    let mut stack_pages = Vec::new();
    stack_pages
        .try_reserve_exact(stack_page_count)
        .map_err(|_| ProgramError::AddressSpace(AddressSpaceError::AllocationFailed))?;

    let mut address_space = memory::create_address_space().map_err(ProgramError::AddressSpace)?;
    let physical_offset = match memory::with_manager(|manager| manager.physical_memory_offset()) {
        Ok(offset) => offset,
        Err(error) => {
            address_space
                .destroy()
                .map_err(ProgramError::AddressSpaceCleanup)?;
            return Err(ProgramError::Memory(error));
        }
    };

    let result = (|| {
        for page_index in 0..code_page_count {
            let address = RAW_PROGRAM_BASE + page_index as u64 * PAGE_SIZE;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(address));
            let frame = address_space
                .map_user_page(page, UserPageAccess::ReadExecute)
                .map_err(ProgramError::AddressSpace)?;
            code_pages.push((page, frame));
        }

        for page_index in 0..data_page_count {
            let address = data_start + page_index as u64 * PAGE_SIZE;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(address));
            let frame = address_space
                .map_user_page(page, UserPageAccess::ReadWrite)
                .map_err(ProgramError::AddressSpace)?;
            data_pages.push((page, frame));
        }

        for page_index in 0..stack_page_count {
            let address = stack_start + page_index as u64 * PAGE_SIZE;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(address));
            let frame = address_space
                .map_user_page(page, UserPageAccess::ReadWrite)
                .map_err(ProgramError::AddressSpace)?;
            stack_pages.push((page, frame));
        }

        copy_to_pages(
            code,
            &code_pages,
            physical_offset,
            VirtAddr::new(RAW_PROGRAM_BASE),
        )?;
        copy_to_pages(
            data,
            &data_pages,
            physical_offset,
            VirtAddr::new(data_start),
        )?;
        copy_to_pages(
            &argument_data,
            &stack_pages,
            physical_offset,
            argument_vector,
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => Ok(LoadedProgram {
            address_space,
            entry: VirtAddr::new(RAW_PROGRAM_BASE + entry_offset as u64),
            stack: VirtAddr::new(USER_STACK_TOP),
            argument_count,
            argument_vector,
            data_start: (data_page_count != 0).then_some(VirtAddr::new(data_start)),
        }),
        Err(error) => {
            address_space
                .destroy()
                .map_err(ProgramError::AddressSpaceCleanup)?;
            Err(error)
        }
    }
}

fn copy_to_pages(
    bytes: &[u8],
    pages: &[(Page<Size4KiB>, PhysAddr)],
    physical_offset: VirtAddr,
    destination_start: VirtAddr,
) -> Result<(), ProgramError> {
    let mut copied = 0;
    let mut current_address = destination_start.as_u64();
    for (page, frame) in pages {
        if copied == bytes.len() {
            break;
        }
        let page_start = page.start_address().as_u64();
        let page_end = page_start + PAGE_SIZE;
        if current_address >= page_end {
            continue;
        }
        if current_address < page_start {
            return Err(ProgramError::AddressSpace(
                AddressSpaceError::UserPageNotMapped,
            ));
        }
        let page_offset = (current_address - page_start) as usize;
        let chunk_len = (bytes.len() - copied).min(PAGE_SIZE as usize - page_offset);
        let destination = physical_offset
            .as_u64()
            .checked_add(frame.as_u64())
            .and_then(|address| address.checked_add(page_offset as u64))
            .and_then(|address| VirtAddr::try_new(address).ok())
            .ok_or(ProgramError::InvalidPhysicalAddress)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr().add(copied),
                destination.as_mut_ptr::<u8>(),
                chunk_len,
            );
        }
        copied += chunk_len;
        current_address += chunk_len as u64;
    }
    assert_eq!(copied, bytes.len(), "mapped pages must cover the segment");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn raw_image_bounds_reject_empty_and_oversized_images() {
        assert!(matches!(
            load_raw_image(&[], "empty.bin", &[]),
            Err(ProgramError::EmptyImage)
        ));
        let oversized = alloc::vec![0; RAW_PROGRAM_MAX_SIZE + 1];
        assert!(matches!(
            load_raw_image(&oversized, "large.bin", &[]),
            Err(ProgramError::ImageTooLarge)
        ));
    }

    #[test_case]
    fn app_parser_rejects_malformed_headers_and_bounds() {
        assert!(matches!(
            parse_app_image(&[]),
            Err(ProgramError::InvalidAppFormat)
        ));

        let mut image = test_app_image(b"\x90", b"", 0);
        image[8] = 2;
        assert!(matches!(
            parse_app_image(&image),
            Err(ProgramError::UnsupportedAppVersion)
        ));

        let mut image = test_app_image(b"\x90", b"", 0);
        image[12] = 1;
        assert!(matches!(
            parse_app_image(&image),
            Err(ProgramError::InvalidAppFormat)
        ));

        let mut image = test_app_image(b"\x90", b"", 0);
        image[16] = 1;
        assert!(matches!(
            parse_app_image(&image),
            Err(ProgramError::InvalidAppFormat)
        ));

        let mut image = test_app_image(b"\x90", b"", 0);
        image.push(0);
        assert!(matches!(
            parse_app_image(&image),
            Err(ProgramError::InvalidAppFormat)
        ));
    }

    #[test_case]
    fn app_parser_rejects_images_exceeding_memory_limit() {
        let mut image = test_app_image(b"\x90", b"", 0);
        image[40..48].copy_from_slice(&((APP_MAX_MEMORY_SIZE as u64).to_le_bytes()));
        assert!(matches!(
            parse_app_image(&image),
            Err(ProgramError::ImageTooLarge)
        ));
    }

    #[test_case]
    fn argument_vector_is_null_terminated_and_points_to_utf8_strings() {
        let (vector, argument_count) = build_argument_vector("apps/demo.app", &["hello", "world"])
            .expect("build app argument vector");
        assert_eq!(argument_count, 3);

        let vector_address = USER_STACK_TOP - USER_STACK_ARGUMENT_RESERVE - vector.len() as u64;
        let pointers: Vec<u64> = (0..=argument_count as usize)
            .map(|index| {
                u64::from_le_bytes(
                    vector[index * 8..index * 8 + 8]
                        .try_into()
                        .expect("argv pointer"),
                )
            })
            .collect();
        assert_eq!(pointers[3], 0);
        for (pointer, expected) in pointers[..3]
            .iter()
            .zip(["apps/demo.app", "hello", "world"])
        {
            let offset = usize::try_from(*pointer - vector_address).expect("argv offset");
            assert_eq!(
                &vector[offset..offset + expected.len()],
                expected.as_bytes()
            );
            assert_eq!(vector[offset + expected.len()], 0);
        }
        assert_eq!(vector_address % 8, 0);
    }

    #[test_case]
    fn argument_vector_rejects_too_many_arguments_and_embedded_nuls() {
        let too_many = alloc::vec![""; MAX_PROGRAM_ARGUMENTS + 1];
        assert!(matches!(
            build_argument_vector("demo.app", &too_many),
            Err(ProgramError::InvalidArguments)
        ));
        assert!(matches!(
            build_argument_vector("demo.app", &["bad\0argument"]),
            Err(ProgramError::InvalidArguments)
        ));
        assert!(matches!(
            build_argument_vector("demo.app", &[&"x".repeat(USER_STACK_SIZE)]),
            Err(ProgramError::InvalidArguments)
        ));
    }

    fn test_app_image(code: &[u8], data: &[u8], bss_len: usize) -> Vec<u8> {
        let mut image = Vec::with_capacity(APP_HEADER_SIZE + code.len() + data.len());
        image.extend_from_slice(APP_MAGIC);
        image.extend_from_slice(&APP_VERSION.to_le_bytes());
        image.extend_from_slice(&(APP_HEADER_SIZE as u16).to_le_bytes());
        image.extend_from_slice(&0u32.to_le_bytes());
        image.extend_from_slice(&0u64.to_le_bytes());
        image.extend_from_slice(&(code.len() as u64).to_le_bytes());
        image.extend_from_slice(&(data.len() as u64).to_le_bytes());
        image.extend_from_slice(&(bss_len as u64).to_le_bytes());
        image.extend_from_slice(code);
        image.extend_from_slice(data);
        image
    }
}
