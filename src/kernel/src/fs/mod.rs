use core::slice;
use spin::Once;

use crate::boot_info::BootInfo;

const MAX_ARCHIVE_SIZE: usize = 16 * 1024 * 1024;
const BLOCK_SIZE: usize = 512;
const MAX_FILES: usize = 256;
const MAX_PATH_LEN: usize = 255;
const MAX_COMPONENT_LEN: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    MissingArchive,
    InvalidArchive,
    TooManyFiles,
    NotInitialized,
}

#[derive(Clone, Copy)]
struct TarEntry<'a> {
    name: [u8; MAX_PATH_LEN],
    name_len: usize,
    data: &'a [u8],
    next_offset: usize,
    is_dir: bool,
}

impl TarEntry<'_> {
    fn name(&self) -> &[u8] {
        &self.name[..self.name_len]
    }
}

static ARCHIVE: Once<Result<&'static [u8], FsError>> = Once::new();

fn octal(field: &[u8]) -> Option<usize> {
    let mut value = 0usize;
    let mut started = false;
    for byte in field {
        match *byte {
            b' ' | 0 if !started => {}
            b'0'..=b'7' => {
                started = true;
                value = value.checked_mul(8)?.checked_add((*byte - b'0') as usize)?;
            }
            b' ' | 0 => break,
            _ => return None,
        }
    }
    Some(value)
}

fn safe_path(path: &[u8], allow_empty: bool) -> bool {
    if path.is_empty() {
        return allow_empty;
    }

    path.split(|byte| *byte == b'/').all(|component| {
        !component.is_empty()
            && component.len() <= MAX_COMPONENT_LEN
            && component != b"."
            && component != b".."
            && component
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
    })
}

fn field(field: &[u8]) -> &[u8] {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    &field[..end]
}

fn entry_at(archive: &[u8], offset: usize) -> Result<Option<TarEntry<'_>>, FsError> {
    let header_end = offset
        .checked_add(BLOCK_SIZE)
        .ok_or(FsError::InvalidArchive)?;
    let header = archive
        .get(offset..header_end)
        .ok_or(FsError::InvalidArchive)?;
    if header.iter().all(|byte| *byte == 0) {
        return Ok(None);
    }

    let expected_checksum = octal(&header[148..156]).ok_or(FsError::InvalidArchive)?;
    let checksum: usize = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                b' ' as usize
            } else {
                *byte as usize
            }
        })
        .sum();
    if checksum != expected_checksum || !header[257..262].eq(b"ustar") {
        return Err(FsError::InvalidArchive);
    }

    let is_dir = match header[156] {
        0 | b'0' => false,
        b'5' => true,
        _ => return Err(FsError::InvalidArchive),
    };
    let name = field(&header[..100]);
    let prefix = field(&header[345..500]);
    if name.is_empty() {
        return Err(FsError::InvalidArchive);
    }

    let raw_len = prefix
        .len()
        .checked_add(usize::from(!prefix.is_empty()))
        .and_then(|length| length.checked_add(name.len()))
        .ok_or(FsError::InvalidArchive)?;
    if raw_len > MAX_PATH_LEN {
        return Err(FsError::InvalidArchive);
    }

    let mut full_name = [0; MAX_PATH_LEN];
    let mut name_len = 0;
    if !prefix.is_empty() {
        full_name[..prefix.len()].copy_from_slice(prefix);
        name_len = prefix.len();
        full_name[name_len] = b'/';
        name_len += 1;
    }
    full_name[name_len..name_len + name.len()].copy_from_slice(name);
    name_len += name.len();

    if full_name[name_len - 1] == b'/' {
        if !is_dir {
            return Err(FsError::InvalidArchive);
        }
        name_len -= 1;
    }
    if !safe_path(&full_name[..name_len], false) {
        return Err(FsError::InvalidArchive);
    }

    let size = octal(&header[124..136]).ok_or(FsError::InvalidArchive)?;
    if is_dir && size != 0 {
        return Err(FsError::InvalidArchive);
    }
    let padded_size = size
        .checked_add(BLOCK_SIZE - 1)
        .ok_or(FsError::InvalidArchive)?
        / BLOCK_SIZE
        * BLOCK_SIZE;
    let data_start = header_end;
    let data_end = data_start
        .checked_add(size)
        .ok_or(FsError::InvalidArchive)?;
    let next_offset = data_start
        .checked_add(padded_size)
        .ok_or(FsError::InvalidArchive)?;
    let data = archive
        .get(data_start..data_end)
        .ok_or(FsError::InvalidArchive)?;
    if next_offset > archive.len() {
        return Err(FsError::InvalidArchive);
    }
    Ok(Some(TarEntry {
        name: full_name,
        name_len,
        data,
        next_offset,
        is_dir,
    }))
}

fn validate(archive: &[u8]) -> Result<(), FsError> {
    if archive.is_empty() || archive.len() > MAX_ARCHIVE_SIZE || archive.len() % BLOCK_SIZE != 0 {
        return Err(FsError::InvalidArchive);
    }
    let mut offset = 0;
    let mut count = 0;
    loop {
        match entry_at(archive, offset)? {
            Some(entry) => {
                count += 1;
                if count > MAX_FILES {
                    return Err(FsError::TooManyFiles);
                }
                let mut previous_offset = 0;
                while previous_offset < offset {
                    let previous =
                        entry_at(archive, previous_offset)?.ok_or(FsError::InvalidArchive)?;
                    let previous_name = previous.name();
                    let current_name = entry.name();
                    let previous_parent = current_name
                        .strip_prefix(previous_name)
                        .is_some_and(|suffix| suffix.first() == Some(&b'/'));
                    let current_parent = previous_name
                        .strip_prefix(current_name)
                        .is_some_and(|suffix| suffix.first() == Some(&b'/'));
                    if previous_name == current_name
                        || (previous_parent && !previous.is_dir)
                        || (current_parent && !entry.is_dir)
                    {
                        return Err(FsError::InvalidArchive);
                    }
                    previous_offset = previous.next_offset;
                }
                offset = entry.next_offset;
            }
            None => {
                if archive[offset..].iter().any(|byte| *byte != 0) {
                    return Err(FsError::InvalidArchive);
                }
                return Ok(());
            }
        }
    }
}

/// Validates and mounts the loader-provided read-only USTAR archive.
///
/// # Safety
///
/// The loader must keep `initrd_base..initrd_base + initrd_size` readable and
/// unchanged for the kernel lifetime.
pub unsafe fn initialize(boot_info: &'static BootInfo) -> Result<(), FsError> {
    ARCHIVE
        .call_once(|| {
            if boot_info.initrd_base.is_null()
                || boot_info.initrd_size == 0
                || boot_info.initrd_size > MAX_ARCHIVE_SIZE
                || boot_info.initrd_size > isize::MAX as usize
            {
                return Err(FsError::MissingArchive);
            }
            // SAFETY: required by this function's caller contract.
            let archive =
                unsafe { slice::from_raw_parts(boot_info.initrd_base, boot_info.initrd_size) };
            validate(archive)?;
            Ok(archive)
        })
        .as_ref()
        .map(|_| ())
        .map_err(|error| *error)
}

fn archive() -> Result<&'static [u8], FsError> {
    ARCHIVE
        .get()
        .ok_or(FsError::NotInitialized)?
        .as_ref()
        .copied()
        .map_err(|error| *error)
}

pub fn directory_exists(path: &str) -> Result<bool, FsError> {
    let archive = archive()?;
    let path = path.as_bytes();
    if !safe_path(path, true) {
        return Ok(false);
    }
    if path.is_empty() {
        return Ok(true);
    }

    let mut offset = 0;
    while let Some(entry) = entry_at(archive, offset)? {
        let name = entry.name();
        if name == path {
            return Ok(entry.is_dir);
        }
        if name.len() > path.len() && name.starts_with(path) && name[path.len()] == b'/' {
            return Ok(true);
        }
        offset = entry.next_offset;
    }
    Ok(false)
}

fn child_entry<'a>(entry: &'a TarEntry<'_>, directory: &[u8]) -> Option<(&'a [u8], bool, usize)> {
    let name = entry.name();
    let relative = if directory.is_empty() {
        name
    } else {
        let prefix_len = directory.len() + 1;
        if name.len() <= prefix_len || !name.starts_with(directory) || name[directory.len()] != b'/'
        {
            return None;
        }
        &name[prefix_len..]
    };
    let child_end = relative
        .iter()
        .position(|byte| *byte == b'/')
        .unwrap_or(relative.len());
    let is_dir = child_end < relative.len() || entry.is_dir;
    Some((
        &relative[..child_end],
        is_dir,
        if is_dir { 0 } else { entry.data.len() },
    ))
}

pub fn for_each_entry(
    directory: &str,
    mut visit: impl FnMut(&str, bool, usize),
) -> Result<(), FsError> {
    if !directory_exists(directory)? {
        return Ok(());
    }

    let archive = archive()?;
    let directory = directory.as_bytes();
    let mut offset = 0;
    while let Some(entry) = entry_at(archive, offset)? {
        if let Some((child, is_dir, size)) = child_entry(&entry, directory) {
            let mut previous_offset = 0;
            let mut seen = false;
            while previous_offset < offset {
                let previous =
                    entry_at(archive, previous_offset)?.ok_or(FsError::InvalidArchive)?;
                if child_entry(&previous, directory)
                    .is_some_and(|(previous_child, _, _)| previous_child == child)
                {
                    seen = true;
                    break;
                }
                previous_offset = previous.next_offset;
            }
            if !seen {
                let name = core::str::from_utf8(child).map_err(|_| FsError::InvalidArchive)?;
                visit(name, is_dir, size);
            }
        }
        offset = entry.next_offset;
    }
    Ok(())
}

pub fn for_each_file(mut visit: impl FnMut(&str, usize)) -> Result<(), FsError> {
    let archive = archive()?;
    let mut offset = 0;
    while let Some(entry) = entry_at(archive, offset)? {
        if !entry.is_dir {
            let name = core::str::from_utf8(entry.name()).map_err(|_| FsError::InvalidArchive)?;
            visit(name, entry.data.len());
        }
        offset = entry.next_offset;
    }
    Ok(())
}

pub fn read_file(name: &str) -> Result<Option<&'static [u8]>, FsError> {
    if !safe_path(name.as_bytes(), false) {
        return Ok(None);
    }
    let archive = archive()?;
    let mut offset = 0;
    while let Some(entry) = entry_at(archive, offset)? {
        if !entry.is_dir && entry.name() == name.as_bytes() {
            return Ok(Some(entry.data));
        }
        offset = entry.next_offset;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn validates_relative_paths() {
        assert!(safe_path(b"README.TXT", false));
        assert!(safe_path(b"docs/ABOUT.TXT", false));
        assert!(safe_path(b"", true));
        assert!(!safe_path(b"../README.TXT", false));
        assert!(!safe_path(b"/README.TXT", false));
        assert!(!safe_path(b"folder//file.txt", false));
        assert!(!safe_path(b"folder/./file.txt", false));
        assert!(!safe_path(b"folder/../file.txt", false));
    }

    #[test_case]
    fn rejects_invalid_octal_fields() {
        assert_eq!(octal(b"0000012\0"), Some(10));
        assert_eq!(octal(b"000008\0"), None);
        assert_eq!(octal(b"9999999"), None);
    }

    #[test_case]
    fn rejects_invalid_archives() {
        assert_eq!(validate(&[]), Err(FsError::InvalidArchive));
        let corrupt_header = [1; BLOCK_SIZE];
        assert_eq!(validate(&corrupt_header), Err(FsError::InvalidArchive));
    }
}
