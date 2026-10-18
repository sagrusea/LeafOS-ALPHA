use crate::sync::Mutex;
use crate::sync::Once;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
#[cfg(feature = "test-hooks")]
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::drivers::ata::{self, AtaError};

const SUPERBLOCK_OFFSET: u32 = 1024;
const SUPERBLOCK_SIZE: usize = 1024;
const ROOT_INODE: u32 = 2;
const DIRECT_BLOCKS: usize = 12;
const INODE_BLOCK_POINTERS_OFFSET: usize = 40;
const MAX_GROUPS: u32 = 4096;
const FEATURE_COMPAT_EXT_ATTR: u32 = 0x0008;
const FEATURE_COMPAT_RESIZE_INODE: u32 = 0x0010;
const FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
const FEATURE_RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const FEATURE_RO_COMPAT_LARGE_FILE: u32 = 0x0002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext2Error {
    Disk(AtaError),
    InvalidFilesystem,
    InvalidPath,
    NotFound,
    AlreadyExists,
    NotEmpty,
    NoSpace,
    AllocationFailed,
    FileTooLarge,
    RecoveryFailed,
    UnsupportedFeature,
    NotInitialized,
}

#[derive(Clone, Copy)]
struct Metadata {
    block_size: usize,
    blocks_count: u32,
    inodes_count: u32,
    first_data_block: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: usize,
    group_count: u32,
    incompat_features: u32,
}

#[derive(Clone, Copy)]
struct Inode {
    mode: u16,
    links: u16,
    size: u64,
    allocated_blocks_512: u32,
    blocks: [u32; DIRECT_BLOCKS],
    single_indirect: u32,
    double_indirect: u32,
}

static EXT2: Once<Result<Metadata, Ext2Error>> = Once::new();
static EXT2_WRITE_LOCK: Mutex<()> = Mutex::new(());
#[cfg(feature = "test-hooks")]
static WRITE_FAILURE_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);

struct BlockTransaction {
    undo: Vec<(u32, Box<[u8]>)>,
}

impl BlockTransaction {
    fn new() -> Self {
        Self { undo: Vec::new() }
    }

    fn write(&mut self, metadata: Metadata, block: u32, contents: &[u8]) -> Result<(), Ext2Error> {
        if !self
            .undo
            .iter()
            .any(|(saved_block, _)| *saved_block == block)
        {
            self.undo
                .try_reserve(1)
                .map_err(|_| Ext2Error::AllocationFailed)?;
            let mut original = zeroed_block();
            read_block(metadata, block, &mut original)?;
            self.undo.push((block, original));
        }
        write_block(metadata, block, contents)
    }

    fn rollback(self, metadata: Metadata) -> Result<(), Ext2Error> {
        let mut failed = false;
        for (block, contents) in self.undo.into_iter().rev() {
            if write_block(metadata, block, &contents).is_err() {
                failed = true;
            }
        }
        if failed {
            Err(Ext2Error::RecoveryFailed)
        } else {
            Ok(())
        }
    }
}

fn zeroed_block() -> Box<[u8]> {
    vec![0; 4096].into_boxed_slice()
}

fn le16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn disk_block_size(block_size: usize) -> Result<u8, Ext2Error> {
    u8::try_from(block_size / 512).map_err(|_| Ext2Error::InvalidFilesystem)
}

fn read_block(metadata: Metadata, block: u32, output: &mut [u8]) -> Result<(), Ext2Error> {
    if block >= metadata.blocks_count || output.len() < metadata.block_size {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let length = metadata.block_size;
    let sectors = disk_block_size(length)?;
    let lba = block
        .checked_mul(u32::from(sectors))
        .ok_or(Ext2Error::InvalidFilesystem)?;
    ata::read_sectors(lba, sectors, &mut output[..length]).map_err(Ext2Error::Disk)
}

fn write_block(metadata: Metadata, block: u32, input: &[u8]) -> Result<(), Ext2Error> {
    if block >= metadata.blocks_count || input.len() < metadata.block_size {
        return Err(Ext2Error::InvalidFilesystem);
    }
    #[cfg(feature = "test-hooks")]
    if fail_test_write() {
        return Err(Ext2Error::Disk(AtaError::Device));
    }
    let length = metadata.block_size;
    let sectors = disk_block_size(length)?;
    let lba = block
        .checked_mul(u32::from(sectors))
        .ok_or(Ext2Error::InvalidFilesystem)?;
    ata::write_sectors(lba, sectors, &input[..length]).map_err(Ext2Error::Disk)
}

#[cfg(feature = "test-hooks")]
fn fail_test_write() -> bool {
    let mut remaining = WRITE_FAILURE_AFTER.load(Ordering::Relaxed);
    while remaining != usize::MAX {
        let next = if remaining == 0 {
            usize::MAX
        } else {
            remaining - 1
        };
        match WRITE_FAILURE_AFTER.compare_exchange_weak(
            remaining,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return remaining == 0,
            Err(current) => remaining = current,
        }
    }
    false
}

#[cfg(feature = "test-hooks")]
/// Inject one block-write failure after the requested successful writes.
pub fn fail_next_write_after(successful_writes: usize) {
    WRITE_FAILURE_AFTER.store(successful_writes, Ordering::Relaxed);
}

fn mount() -> Result<Metadata, Ext2Error> {
    let mut superblock = [0; SUPERBLOCK_SIZE];
    let superblock_lba = SUPERBLOCK_OFFSET / 512;
    ata::read_sectors(superblock_lba, 2, &mut superblock).map_err(Ext2Error::Disk)?;

    if le16(&superblock, 56) != 0xef53 {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let log_block_size = le32(&superblock, 24);
    let log_fragment_size = le32(&superblock, 28);
    if log_block_size > 2 {
        return Err(Ext2Error::UnsupportedFeature);
    }
    if log_fragment_size != log_block_size {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let block_size = 1024usize
        .checked_shl(log_block_size)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let blocks_count = le32(&superblock, 4);
    let inodes_count = le32(&superblock, 0);
    let first_data_block = le32(&superblock, 20);
    let blocks_per_group = le32(&superblock, 32);
    let inodes_per_group = le32(&superblock, 40);
    let revision = le32(&superblock, 76);
    let inode_size = if revision == 0 {
        128
    } else {
        usize::from(le16(&superblock, 88))
    };
    let incompat_features = if revision == 0 {
        0
    } else {
        le32(&superblock, 96)
    };
    let compat_features = if revision == 0 {
        0
    } else {
        le32(&superblock, 92)
    };
    let read_only_features = if revision == 0 {
        0
    } else {
        le32(&superblock, 100)
    };

    if blocks_count == 0
        || inodes_count < ROOT_INODE
        || blocks_per_group == 0
        || inodes_per_group == 0
        || first_data_block >= blocks_count
        || blocks_per_group > block_size as u32 * 8
        || inodes_per_group > block_size as u32 * 8
        || !(128..=block_size).contains(&inode_size)
        || !block_size.is_multiple_of(inode_size)
        || revision > 1
        || (block_size == 1024 && first_data_block != 1)
        || (block_size > 1024 && first_data_block != 0)
    {
        return Err(Ext2Error::InvalidFilesystem);
    }

    let known_compat = FEATURE_COMPAT_EXT_ATTR | FEATURE_COMPAT_RESIZE_INODE;
    let known_ro_compat = FEATURE_RO_COMPAT_SPARSE_SUPER | FEATURE_RO_COMPAT_LARGE_FILE;
    if incompat_features & !FEATURE_INCOMPAT_FILETYPE != 0
        || compat_features & !known_compat != 0
        || read_only_features & !known_ro_compat != 0
    {
        return Err(Ext2Error::UnsupportedFeature);
    }

    let block_groups = blocks_count
        .checked_sub(first_data_block)
        .and_then(|count| count.checked_add(blocks_per_group - 1))
        .ok_or(Ext2Error::InvalidFilesystem)?
        / blocks_per_group;
    let inode_groups = inodes_count
        .checked_add(inodes_per_group - 1)
        .ok_or(Ext2Error::InvalidFilesystem)?
        / inodes_per_group;
    let group_count = block_groups.max(inode_groups);
    if group_count == 0 || group_count > MAX_GROUPS {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let descriptor_blocks = group_count
        .checked_mul(32)
        .and_then(|bytes| bytes.checked_add(block_size as u32 - 1))
        .ok_or(Ext2Error::InvalidFilesystem)?
        / block_size as u32;
    if first_data_block
        .checked_add(1)
        .and_then(|block| block.checked_add(descriptor_blocks))
        .is_none_or(|end| end > blocks_count)
    {
        return Err(Ext2Error::InvalidFilesystem);
    }

    let sectors_per_block = u32::from(disk_block_size(block_size)?);
    let disk_sectors = blocks_count
        .checked_mul(sectors_per_block)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    if disk_sectors > 0x1000_0000 {
        return Err(Ext2Error::UnsupportedFeature);
    }

    Ok(Metadata {
        block_size,
        blocks_count,
        inodes_count,
        first_data_block,
        blocks_per_group,
        inodes_per_group,
        inode_size,
        group_count,
        incompat_features,
    })
}

pub fn initialize() -> Result<(), Ext2Error> {
    EXT2.call_once(mount)
        .as_ref()
        .map(|_| ())
        .map_err(|error| *error)
}

fn metadata() -> Result<Metadata, Ext2Error> {
    EXT2.get()
        .ok_or(Ext2Error::NotInitialized)?
        .as_ref()
        .copied()
        .map_err(|error| *error)
}

fn read_group_descriptor(
    metadata: Metadata,
    group: u32,
    output: &mut [u8],
) -> Result<u32, Ext2Error> {
    if group >= metadata.group_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let descriptor_table = metadata
        .first_data_block
        .checked_add(1)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let byte_offset = usize::try_from(group)
        .ok()
        .and_then(|value| value.checked_mul(32))
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let block_offset = byte_offset / metadata.block_size;
    let descriptor_offset = byte_offset % metadata.block_size;
    let descriptor_block = descriptor_table
        .checked_add(u32::try_from(block_offset).map_err(|_| Ext2Error::InvalidFilesystem)?)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    read_block(metadata, descriptor_block, output)?;
    let inode_table = le32(output, descriptor_offset + 8);
    if inode_table >= metadata.blocks_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    Ok(inode_table)
}

fn read_inode(metadata: Metadata, inode_number: u32) -> Result<Inode, Ext2Error> {
    if inode_number == 0 || inode_number > metadata.inodes_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let inode_index = inode_number - 1;
    let group = inode_index / metadata.inodes_per_group;
    let local_index = inode_index % metadata.inodes_per_group;
    let mut block = zeroed_block();
    let inode_table = read_group_descriptor(metadata, group, &mut block)?;
    let inode_table_blocks = usize::try_from(metadata.inodes_per_group)
        .ok()
        .and_then(|count| count.checked_mul(metadata.inode_size))
        .and_then(|bytes| bytes.checked_add(metadata.block_size - 1))
        .ok_or(Ext2Error::InvalidFilesystem)?
        / metadata.block_size;
    if usize::try_from(inode_table)
        .ok()
        .and_then(|start| start.checked_add(inode_table_blocks))
        .is_none_or(|end| end > metadata.blocks_count as usize)
    {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let inode_offset = usize::try_from(local_index)
        .ok()
        .and_then(|index| index.checked_mul(metadata.inode_size))
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let block_delta = inode_offset / metadata.block_size;
    let within_block = inode_offset % metadata.block_size;
    let block_number = inode_table
        .checked_add(u32::try_from(block_delta).map_err(|_| Ext2Error::InvalidFilesystem)?)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    read_block(metadata, block_number, &mut block)?;
    if within_block + metadata.inode_size > metadata.block_size {
        return Err(Ext2Error::InvalidFilesystem);
    }

    let raw = &block[within_block..within_block + metadata.inode_size];
    let low_size = u64::from(le32(raw, 4));
    let size = if le16(raw, 0) & 0xf000 == 0x8000 && metadata.inode_size >= 112 {
        low_size | (u64::from(le32(raw, 108)) << 32)
    } else {
        low_size
    };
    let mut blocks = [0; DIRECT_BLOCKS];
    for (index, block_number) in blocks.iter_mut().enumerate() {
        *block_number = le32(raw, INODE_BLOCK_POINTERS_OFFSET + index * 4);
        if *block_number != 0 && *block_number >= metadata.blocks_count {
            return Err(Ext2Error::InvalidFilesystem);
        }
    }
    let single_indirect = le32(raw, INODE_BLOCK_POINTERS_OFFSET + DIRECT_BLOCKS * 4);
    if single_indirect != 0 && single_indirect >= metadata.blocks_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let double_indirect = le32(raw, INODE_BLOCK_POINTERS_OFFSET + (DIRECT_BLOCKS + 1) * 4);
    if double_indirect != 0 && double_indirect >= metadata.blocks_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    Ok(Inode {
        mode: le16(raw, 0),
        links: le16(raw, 26),
        size,
        allocated_blocks_512: le32(raw, 28),
        blocks,
        single_indirect,
        double_indirect,
    })
}

fn inode_block_count(metadata: Metadata, inode: Inode) -> Result<usize, Ext2Error> {
    let size = usize::try_from(inode.size).map_err(|_| Ext2Error::UnsupportedFeature)?;
    let block_count = size / metadata.block_size + usize::from(size % metadata.block_size != 0);
    let pointers_per_block = metadata.block_size / 4;
    let max_blocks = DIRECT_BLOCKS
        .checked_add(pointers_per_block)
        .and_then(|count| count.checked_add(pointers_per_block.checked_mul(pointers_per_block)?))
        .ok_or(Ext2Error::UnsupportedFeature)?;
    if block_count > max_blocks {
        return Err(Ext2Error::UnsupportedFeature);
    }
    Ok(block_count)
}

fn inode_data_block(
    metadata: Metadata,
    inode: Inode,
    logical_index: usize,
    indirect_data: &mut [u8],
    cached_indirect_block: &mut u32,
    double_indirect_data: &mut [u8],
    cached_double_indirect_block: &mut u32,
) -> Result<u32, Ext2Error> {
    if logical_index < DIRECT_BLOCKS {
        return Ok(inode.blocks[logical_index]);
    }
    let indirect_index = logical_index - DIRECT_BLOCKS;
    let pointers_per_block = metadata.block_size / 4;
    let block = if indirect_index < pointers_per_block {
        if inode.single_indirect == 0 {
            return Ok(0);
        }
        if *cached_indirect_block != inode.single_indirect {
            read_block(metadata, inode.single_indirect, indirect_data)?;
            *cached_indirect_block = inode.single_indirect;
        }
        le32(indirect_data, indirect_index * 4)
    } else {
        let double_index = indirect_index - pointers_per_block;
        let outer_index = double_index / pointers_per_block;
        let inner_index = double_index % pointers_per_block;
        if outer_index >= pointers_per_block {
            return Err(Ext2Error::UnsupportedFeature);
        }
        if inode.double_indirect == 0 {
            return Ok(0);
        }
        if *cached_double_indirect_block != inode.double_indirect {
            read_block(metadata, inode.double_indirect, double_indirect_data)?;
            *cached_double_indirect_block = inode.double_indirect;
            *cached_indirect_block = 0;
        }
        let indirect_block = le32(double_indirect_data, outer_index * 4);
        if indirect_block == 0 {
            return Ok(0);
        }
        if indirect_block >= metadata.blocks_count {
            return Err(Ext2Error::InvalidFilesystem);
        }
        if *cached_indirect_block != indirect_block {
            read_block(metadata, indirect_block, indirect_data)?;
            *cached_indirect_block = indirect_block;
        }
        le32(indirect_data, inner_index * 4)
    };
    if block != 0 && block >= metadata.blocks_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    Ok(block)
}

fn safe_component(component: &[u8]) -> bool {
    !component.is_empty()
        && component.len() <= 255
        && component != b"."
        && component != b".."
        && component
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
}

fn find_child(
    metadata: Metadata,
    directory: Inode,
    child: &[u8],
) -> Result<Option<u32>, Ext2Error> {
    if directory.mode & 0xf000 != 0x4000 {
        return Ok(None);
    }
    let block_count = inode_block_count(metadata, directory)?;
    let mut remaining =
        usize::try_from(directory.size).map_err(|_| Ext2Error::UnsupportedFeature)?;
    let mut block = zeroed_block();
    let mut indirect_data = zeroed_block();
    let mut double_indirect_data = zeroed_block();
    let mut cached_indirect_block = 0;
    let mut cached_double_indirect_block = 0;
    for logical_index in 0..block_count {
        let block_number = inode_data_block(
            metadata,
            directory,
            logical_index,
            &mut indirect_data,
            &mut cached_indirect_block,
            &mut double_indirect_data,
            &mut cached_double_indirect_block,
        )?;
        if block_number == 0 {
            return Err(Ext2Error::InvalidFilesystem);
        }
        read_block(metadata, block_number, &mut block)?;
        let valid_bytes = remaining.min(metadata.block_size);
        let mut offset = 0;
        while offset + 8 <= valid_bytes {
            let inode = le32(&block, offset);
            let record_len = usize::from(le16(&block, offset + 4));
            if record_len < 8 || !record_len.is_multiple_of(4) || offset + record_len > valid_bytes
            {
                return Err(Ext2Error::InvalidFilesystem);
            }
            let name_len = if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
                usize::from(block[offset + 6])
            } else {
                usize::from(le16(&block, offset + 6))
            };
            if name_len > record_len - 8 {
                return Err(Ext2Error::InvalidFilesystem);
            }
            let name = &block[offset + 8..offset + 8 + name_len];
            if inode != 0 {
                if inode > metadata.inodes_count {
                    return Err(Ext2Error::InvalidFilesystem);
                }
                if name == child {
                    return Ok(Some(inode));
                }
            }
            offset += record_len;
        }
        remaining = remaining.saturating_sub(metadata.block_size);
    }
    Ok(None)
}

fn lookup(path: &str) -> Result<Option<Inode>, Ext2Error> {
    let metadata = metadata()?;
    let mut inode = read_inode(metadata, ROOT_INODE)?;
    let mut components = path.as_bytes().split(|byte| *byte == b'/').peekable();
    while let Some(component) = components.next() {
        if component.is_empty() {
            continue;
        }
        if !safe_component(component) || inode.mode & 0xf000 != 0x4000 {
            return Ok(None);
        }
        let Some(child_number) = find_child(metadata, inode, component)? else {
            return Ok(None);
        };
        inode = read_inode(metadata, child_number)?;
    }
    Ok(Some(inode))
}

fn write_inode_fields(
    metadata: Metadata,
    inode_number: u32,
    mode: u16,
    size: u32,
    link_count: u16,
    data_blocks: &[u32; DIRECT_BLOCKS],
    single_indirect: u32,
    double_indirect: u32,
    allocated_blocks_512: u32,
    is_new: bool,
    transaction: Option<&mut BlockTransaction>,
) -> Result<(), Ext2Error> {
    let inode_index = inode_number
        .checked_sub(1)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let group = inode_index / metadata.inodes_per_group;
    let local_index = inode_index % metadata.inodes_per_group;
    let mut block = zeroed_block();
    let inode_table = read_group_descriptor(metadata, group, &mut block)?;
    let inode_offset = usize::try_from(local_index)
        .ok()
        .and_then(|index| index.checked_mul(metadata.inode_size))
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let block_number = inode_table
        .checked_add(
            u32::try_from(inode_offset / metadata.block_size)
                .map_err(|_| Ext2Error::InvalidFilesystem)?,
        )
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let within_block = inode_offset % metadata.block_size;
    if within_block + metadata.inode_size > metadata.block_size {
        return Err(Ext2Error::InvalidFilesystem);
    }
    read_block(metadata, block_number, &mut block)?;
    let inode = &mut block[within_block..within_block + metadata.inode_size];
    if is_new {
        inode.fill(0);
    }
    inode[0..2].copy_from_slice(&mode.to_le_bytes());
    inode[4..8].copy_from_slice(&size.to_le_bytes());
    inode[26..28].copy_from_slice(&link_count.to_le_bytes());
    if metadata.inode_size >= 112 {
        inode[108..112].fill(0);
    }
    inode[28..32].copy_from_slice(&allocated_blocks_512.to_le_bytes());
    for (index, block) in data_blocks.iter().enumerate() {
        let offset = INODE_BLOCK_POINTERS_OFFSET + index * 4;
        inode[offset..offset + 4].copy_from_slice(&block.to_le_bytes());
    }
    let single_offset = INODE_BLOCK_POINTERS_OFFSET + DIRECT_BLOCKS * 4;
    inode[single_offset..single_offset + 4].copy_from_slice(&single_indirect.to_le_bytes());
    inode[single_offset + 4..single_offset + 8].copy_from_slice(&double_indirect.to_le_bytes());
    match transaction {
        Some(transaction) => transaction.write(metadata, block_number, &block),
        None => write_block(metadata, block_number, &block),
    }
}

fn find_free_bit(bitmap: &[u8], count: u32) -> Option<u32> {
    (0..count).find(|index| bitmap[*index as usize / 8] & (1 << (*index % 8)) == 0)
}

fn allocate_data_block(metadata: Metadata, bitmap: &mut [u8]) -> Result<u32, Ext2Error> {
    let block_index = find_free_bit(bitmap, metadata.blocks_count - metadata.first_data_block)
        .ok_or(Ext2Error::NoSpace)?;
    let block = block_index
        .checked_add(metadata.first_data_block)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    update_block_bitmap(metadata, bitmap, block, true)?;
    Ok(block)
}

fn single_indirect_capacity(metadata: Metadata) -> Result<usize, Ext2Error> {
    DIRECT_BLOCKS
        .checked_add(metadata.block_size / 4)
        .ok_or(Ext2Error::UnsupportedFeature)
}

fn double_indirect_capacity(metadata: Metadata) -> Result<usize, Ext2Error> {
    let pointers_per_block = metadata.block_size / 4;
    single_indirect_capacity(metadata)?
        .checked_add(
            pointers_per_block
                .checked_mul(pointers_per_block)
                .ok_or(Ext2Error::UnsupportedFeature)?,
        )
        .ok_or(Ext2Error::UnsupportedFeature)
}

fn add_directory_entry(
    metadata: Metadata,
    directory: Inode,
    name: &[u8],
    inode_number: u32,
    file_type: u8,
    transaction: Option<&mut BlockTransaction>,
) -> Result<(), Ext2Error> {
    if directory.mode & 0xf000 != 0x4000 || inode_block_count(metadata, directory)? != 1 {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let block_number = directory.blocks[0];
    if block_number == 0 {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let mut block = zeroed_block();
    read_block(metadata, block_number, &mut block)?;
    let required = (8 + name.len() + 3) & !3;
    let mut offset = 0;
    while offset + 8 <= metadata.block_size {
        let entry_inode = le32(&block, offset);
        let record_len = usize::from(le16(&block, offset + 4));
        if record_len < 8
            || !record_len.is_multiple_of(4)
            || offset + record_len > metadata.block_size
        {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let name_len = if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
            usize::from(block[offset + 6])
        } else {
            usize::from(le16(&block, offset + 6))
        };
        if name_len > record_len - 8 {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let used_len = (8 + name_len + 3) & !3;
        if (entry_inode == 0 && record_len >= required)
            || (entry_inode != 0 && record_len - used_len >= required)
        {
            let entry_offset = if entry_inode == 0 {
                offset
            } else {
                let previous_len =
                    u16::try_from(used_len).map_err(|_| Ext2Error::InvalidFilesystem)?;
                block[offset + 4..offset + 6].copy_from_slice(&previous_len.to_le_bytes());
                offset + used_len
            };
            let entry_len = if entry_inode == 0 {
                record_len
            } else {
                record_len - used_len
            };
            block[entry_offset..entry_offset + entry_len].fill(0);
            block[entry_offset..entry_offset + 4].copy_from_slice(&inode_number.to_le_bytes());
            block[entry_offset + 4..entry_offset + 6]
                .copy_from_slice(&(entry_len as u16).to_le_bytes());
            if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
                block[entry_offset + 6] = name.len() as u8;
                block[entry_offset + 7] = file_type;
            } else {
                block[entry_offset + 6..entry_offset + 8]
                    .copy_from_slice(&(name.len() as u16).to_le_bytes());
            }
            block[entry_offset + 8..entry_offset + 8 + name.len()].copy_from_slice(name);
            return match transaction {
                Some(transaction) => transaction.write(metadata, block_number, &block),
                None => write_block(metadata, block_number, &block),
            };
        }
        offset += record_len;
    }
    Err(Ext2Error::NoSpace)
}

fn ensure_directory_entry_space(
    metadata: Metadata,
    directory: Inode,
    name: &[u8],
) -> Result<(), Ext2Error> {
    if directory.mode & 0xf000 != 0x4000
        || inode_block_count(metadata, directory)? != 1
        || directory.blocks[0] == 0
    {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let mut block = zeroed_block();
    read_block(metadata, directory.blocks[0], &mut block)?;
    let required = (8 + name.len() + 3) & !3;
    let mut offset = 0;
    while offset + 8 <= metadata.block_size {
        let record_len = usize::from(le16(&block, offset + 4));
        if record_len < 8
            || !record_len.is_multiple_of(4)
            || offset + record_len > metadata.block_size
        {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let name_len = if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
            usize::from(block[offset + 6])
        } else {
            usize::from(le16(&block, offset + 6))
        };
        if name_len > record_len - 8 {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let used_len = (8 + name_len + 3) & !3;
        if (le32(&block, offset) == 0 && record_len >= required)
            || (le32(&block, offset) != 0 && record_len - used_len >= required)
        {
            return Ok(());
        }
        offset += record_len;
    }
    Err(Ext2Error::NoSpace)
}

fn writable_metadata() -> Result<Metadata, Ext2Error> {
    let metadata = metadata()?;
    if metadata.block_size != 1024
        || metadata.group_count != 1
        || metadata.inode_size != 128
        || metadata.blocks_per_group < metadata.blocks_count - metadata.first_data_block
        || metadata.inodes_per_group < metadata.inodes_count
    {
        return Err(Ext2Error::UnsupportedFeature);
    }
    Ok(metadata)
}

fn resolve_parent<'a>(
    metadata: Metadata,
    path: &'a str,
) -> Result<(u32, Inode, &'a [u8]), Ext2Error> {
    let bytes = path.as_bytes();
    let separator = bytes.iter().rposition(|byte| *byte == b'/');
    let (parent_path, name) = match separator {
        Some(index) => (&path[..index], &bytes[index + 1..]),
        None => ("", bytes),
    };
    if !safe_component(name) {
        return Err(Ext2Error::InvalidPath);
    }
    if parent_path
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|component| !component.is_empty() && !safe_component(component))
    {
        return Err(Ext2Error::InvalidPath);
    }
    let parent_number = if parent_path.is_empty() || parent_path == "/" {
        ROOT_INODE
    } else {
        let parent = lookup(parent_path)?.ok_or(Ext2Error::NotFound)?;
        if parent.mode & 0xf000 != 0x4000 {
            return Err(Ext2Error::InvalidPath);
        }
        find_inode_number(metadata, parent_path)?.ok_or(Ext2Error::InvalidFilesystem)?
    };
    let parent = read_inode(metadata, parent_number)?;
    if parent.mode & 0xf000 != 0x4000 {
        return Err(Ext2Error::InvalidPath);
    }
    Ok((parent_number, parent, name))
}

fn find_inode_number(metadata: Metadata, path: &str) -> Result<Option<u32>, Ext2Error> {
    let mut inode_number = ROOT_INODE;
    let mut inode = read_inode(metadata, inode_number)?;
    for component in path.as_bytes().split(|byte| *byte == b'/') {
        if component.is_empty() {
            continue;
        }
        if !safe_component(component) || inode.mode & 0xf000 != 0x4000 {
            return Ok(None);
        }
        let Some(child_number) = find_child(metadata, inode, component)? else {
            return Ok(None);
        };
        inode_number = child_number;
        inode = read_inode(metadata, inode_number)?;
    }
    Ok(Some(inode_number))
}

fn remove_directory_entry(
    metadata: Metadata,
    directory: Inode,
    name: &[u8],
) -> Result<(), Ext2Error> {
    if directory.mode & 0xf000 != 0x4000
        || inode_block_count(metadata, directory)? != 1
        || directory.blocks[0] == 0
    {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let mut block = zeroed_block();
    read_block(metadata, directory.blocks[0], &mut block)?;
    let mut offset = 0;
    let mut previous_offset = None;
    while offset + 8 <= metadata.block_size {
        let inode = le32(&block, offset);
        let record_len = usize::from(le16(&block, offset + 4));
        if record_len < 8
            || !record_len.is_multiple_of(4)
            || offset + record_len > metadata.block_size
        {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let name_len = if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
            usize::from(block[offset + 6])
        } else {
            usize::from(le16(&block, offset + 6))
        };
        if name_len > record_len - 8 {
            return Err(Ext2Error::InvalidFilesystem);
        }
        if inode != 0 && &block[offset + 8..offset + 8 + name_len] == name {
            if let Some(previous_offset) = previous_offset {
                let previous_record_len = usize::from(le16(&block, previous_offset + 4));
                let merged_len = previous_record_len
                    .checked_add(record_len)
                    .filter(|length| *length <= metadata.block_size)
                    .and_then(|length| u16::try_from(length).ok())
                    .ok_or(Ext2Error::InvalidFilesystem)?;
                block[previous_offset + 4..previous_offset + 6]
                    .copy_from_slice(&merged_len.to_le_bytes());
            } else {
                block[offset..offset + 4].fill(0);
                block[offset + 6] = 0;
                block[offset + 7] = 0;
                block[offset + 8..offset + record_len].fill(0);
            }
            return write_block(metadata, directory.blocks[0], &block);
        }
        previous_offset = Some(offset);
        offset += record_len;
    }
    Err(Ext2Error::NotFound)
}

fn read_allocation_tables(
    metadata: Metadata,
) -> Result<(u32, u32, Box<[u8]>, Box<[u8]>, Box<[u8]>), Ext2Error> {
    let mut descriptor = zeroed_block();
    read_block(metadata, metadata.first_data_block + 1, &mut descriptor)?;
    let block_bitmap_number = le32(&descriptor, 0);
    let inode_bitmap_number = le32(&descriptor, 4);
    if block_bitmap_number >= metadata.blocks_count || inode_bitmap_number >= metadata.blocks_count
    {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let mut block_bitmap = zeroed_block();
    let mut inode_bitmap = zeroed_block();
    read_block(metadata, block_bitmap_number, &mut block_bitmap)?;
    read_block(metadata, inode_bitmap_number, &mut inode_bitmap)?;
    Ok((
        block_bitmap_number,
        inode_bitmap_number,
        descriptor,
        block_bitmap,
        inode_bitmap,
    ))
}

fn update_free_counters(
    metadata: Metadata,
    descriptor: &mut [u8],
    blocks_delta: i32,
    inodes_delta: i32,
    transaction: Option<&mut BlockTransaction>,
) -> Result<(), Ext2Error> {
    if blocks_delta == 0 && inodes_delta == 0 {
        return Ok(());
    }
    let descriptor_blocks = i32::from(le16(descriptor, 12))
        .checked_add(blocks_delta)
        .filter(|count| (0..=u16::MAX as i32).contains(count))
        .ok_or(Ext2Error::InvalidFilesystem)? as u16;
    let descriptor_inodes = i32::from(le16(descriptor, 14))
        .checked_add(inodes_delta)
        .filter(|count| (0..=u16::MAX as i32).contains(count))
        .ok_or(Ext2Error::InvalidFilesystem)? as u16;
    let mut superblock = zeroed_block();
    read_block(metadata, metadata.first_data_block, &mut superblock)?;
    let super_blocks = i64::from(le32(&superblock, 12))
        .checked_add(i64::from(blocks_delta))
        .filter(|count| (0..=u32::MAX as i64).contains(count))
        .ok_or(Ext2Error::InvalidFilesystem)? as u32;
    let super_inodes = i64::from(le32(&superblock, 16))
        .checked_add(i64::from(inodes_delta))
        .filter(|count| (0..=u32::MAX as i64).contains(count))
        .ok_or(Ext2Error::InvalidFilesystem)? as u32;

    descriptor[12..14].copy_from_slice(&descriptor_blocks.to_le_bytes());
    descriptor[14..16].copy_from_slice(&descriptor_inodes.to_le_bytes());
    superblock[12..16].copy_from_slice(&super_blocks.to_le_bytes());
    superblock[16..20].copy_from_slice(&super_inodes.to_le_bytes());
    match transaction {
        Some(transaction) => {
            transaction.write(metadata, metadata.first_data_block + 1, descriptor)?;
            transaction.write(metadata, metadata.first_data_block, &superblock)
        }
        None => {
            write_block(metadata, metadata.first_data_block + 1, descriptor)?;
            write_block(metadata, metadata.first_data_block, &superblock)
        }
    }
}

fn free_data_blocks(
    metadata: Metadata,
    blocks: &[u32],
    transaction: Option<&mut BlockTransaction>,
) -> Result<(), Ext2Error> {
    if blocks.is_empty() {
        return Ok(());
    }
    let (block_bitmap_number, _, mut descriptor, mut block_bitmap, _) =
        read_allocation_tables(metadata)?;
    for &block in blocks {
        update_block_bitmap(metadata, &mut block_bitmap, block, false)?;
    }
    match transaction {
        Some(transaction) => {
            transaction.write(metadata, block_bitmap_number, &block_bitmap)?;
            update_free_counters(
                metadata,
                &mut descriptor,
                i32::try_from(blocks.len()).map_err(|_| Ext2Error::InvalidFilesystem)?,
                0,
                Some(transaction),
            )
        }
        None => {
            write_block(metadata, block_bitmap_number, &block_bitmap)?;
            update_free_counters(
                metadata,
                &mut descriptor,
                i32::try_from(blocks.len()).map_err(|_| Ext2Error::InvalidFilesystem)?,
                0,
                None,
            )
        }
    }
}

fn push_unique_block(blocks: &mut Vec<u32>, block: u32) -> Result<(), Ext2Error> {
    if blocks.contains(&block) {
        return Err(Ext2Error::InvalidFilesystem);
    }
    blocks.push(block);
    Ok(())
}

fn collect_file_allocations(metadata: Metadata, inode: Inode) -> Result<Vec<u32>, Ext2Error> {
    if inode.size > (double_indirect_capacity(metadata)? * metadata.block_size) as u64 {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let pointer_capacity = metadata.block_size / 4;
    let mut allocation_capacity = DIRECT_BLOCKS;
    if inode.single_indirect != 0 {
        allocation_capacity = allocation_capacity
            .checked_add(pointer_capacity + 1)
            .ok_or(Ext2Error::AllocationFailed)?;
    }
    if inode.double_indirect != 0 {
        allocation_capacity = allocation_capacity
            .checked_add(
                pointer_capacity
                    .checked_mul(pointer_capacity)
                    .and_then(|blocks| blocks.checked_add(pointer_capacity + 1))
                    .ok_or(Ext2Error::AllocationFailed)?,
            )
            .ok_or(Ext2Error::AllocationFailed)?;
    }
    let mut blocks = Vec::new();
    blocks
        .try_reserve_exact(allocation_capacity)
        .map_err(|_| Ext2Error::AllocationFailed)?;
    for block in inode.blocks.iter().copied().filter(|block| *block != 0) {
        if block >= metadata.blocks_count {
            return Err(Ext2Error::InvalidFilesystem);
        }
        push_unique_block(&mut blocks, block)?;
    }
    if inode.single_indirect != 0 {
        if inode.single_indirect >= metadata.blocks_count {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let mut pointers = zeroed_block();
        read_block(metadata, inode.single_indirect, &mut pointers)?;
        for index in 0..pointer_capacity {
            let block = le32(&pointers, index * 4);
            if block != 0 {
                if block >= metadata.blocks_count {
                    return Err(Ext2Error::InvalidFilesystem);
                }
                push_unique_block(&mut blocks, block)?;
            }
        }
        push_unique_block(&mut blocks, inode.single_indirect)?;
    }
    if inode.double_indirect != 0 {
        if inode.double_indirect >= metadata.blocks_count {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let mut root = zeroed_block();
        read_block(metadata, inode.double_indirect, &mut root)?;
        for index in 0..pointer_capacity {
            let indirect = le32(&root, index * 4);
            if indirect == 0 {
                continue;
            }
            if indirect >= metadata.blocks_count {
                return Err(Ext2Error::InvalidFilesystem);
            }
            let mut pointers = zeroed_block();
            read_block(metadata, indirect, &mut pointers)?;
            for pointer_index in 0..pointer_capacity {
                let block = le32(&pointers, pointer_index * 4);
                if block != 0 {
                    if block >= metadata.blocks_count {
                        return Err(Ext2Error::InvalidFilesystem);
                    }
                    push_unique_block(&mut blocks, block)?;
                }
            }
            push_unique_block(&mut blocks, indirect)?;
        }
        push_unique_block(&mut blocks, inode.double_indirect)?;
    }
    Ok(blocks)
}

fn update_directory_link_count(
    metadata: Metadata,
    inode_number: u32,
    increase: bool,
) -> Result<(), Ext2Error> {
    let root = read_inode(metadata, inode_number)?;
    let links = if increase {
        root.links.checked_add(1)
    } else {
        root.links.checked_sub(1)
    }
    .ok_or(Ext2Error::InvalidFilesystem)?;
    write_inode_fields(
        metadata,
        inode_number,
        root.mode,
        u32::try_from(root.size).map_err(|_| Ext2Error::UnsupportedFeature)?,
        links,
        &root.blocks,
        root.single_indirect,
        root.double_indirect,
        root.allocated_blocks_512,
        false,
        None,
    )
}

fn update_inode_bitmap(
    bitmap: &mut [u8],
    inode_number: u32,
    allocated: bool,
) -> Result<(), Ext2Error> {
    let bit = inode_number
        .checked_sub(1)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    let mask = 1 << (bit % 8);
    let value = &mut bitmap[bit as usize / 8];
    if (*value & mask != 0) == allocated {
        return Err(Ext2Error::InvalidFilesystem);
    }
    if allocated {
        *value |= mask;
    } else {
        *value &= !mask;
    }
    Ok(())
}

fn update_block_bitmap(
    metadata: Metadata,
    bitmap: &mut [u8],
    block_number: u32,
    allocated: bool,
) -> Result<(), Ext2Error> {
    if block_number < metadata.first_data_block || block_number >= metadata.blocks_count {
        return Err(Ext2Error::InvalidFilesystem);
    }
    let bit = block_number - metadata.first_data_block;
    let mask = 1 << (bit % 8);
    let value = &mut bitmap[bit as usize / 8];
    if (*value & mask != 0) == allocated {
        return Err(Ext2Error::InvalidFilesystem);
    }
    if allocated {
        *value |= mask;
    } else {
        *value &= !mask;
    }
    Ok(())
}

fn create_directory_in(
    metadata: Metadata,
    parent_number: u32,
    parent: Inode,
    name: &[u8],
) -> Result<(), Ext2Error> {
    ensure_directory_entry_space(metadata, parent, name)?;
    let (
        block_bitmap_number,
        inode_bitmap_number,
        mut descriptor,
        mut block_bitmap,
        mut inode_bitmap,
    ) = read_allocation_tables(metadata)?;
    let inode_number = find_free_bit(&inode_bitmap, metadata.inodes_count)
        .and_then(|index| index.checked_add(1))
        .ok_or(Ext2Error::NoSpace)?;
    let block_index = find_free_bit(
        &block_bitmap,
        metadata.blocks_count - metadata.first_data_block,
    )
    .ok_or(Ext2Error::NoSpace)?;
    let data_block = block_index
        .checked_add(metadata.first_data_block)
        .ok_or(Ext2Error::InvalidFilesystem)?;
    update_inode_bitmap(&mut inode_bitmap, inode_number, true)?;
    update_block_bitmap(metadata, &mut block_bitmap, data_block, true)?;

    let mut directory = zeroed_block();
    directory[0..4].copy_from_slice(&inode_number.to_le_bytes());
    directory[4..6].copy_from_slice(&12u16.to_le_bytes());
    if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
        directory[6] = 1;
        directory[7] = 2;
    } else {
        directory[6..8].copy_from_slice(&1u16.to_le_bytes());
    }
    directory[8] = b'.';
    directory[12..16].copy_from_slice(&parent_number.to_le_bytes());
    directory[16..18].copy_from_slice(&((metadata.block_size - 12) as u16).to_le_bytes());
    if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
        directory[18] = 2;
        directory[19] = 2;
    } else {
        directory[18..20].copy_from_slice(&2u16.to_le_bytes());
    }
    directory[20..22].copy_from_slice(b"..");
    write_block(metadata, data_block, &directory)?;

    let mut inode_blocks = [0; DIRECT_BLOCKS];
    inode_blocks[0] = data_block;
    write_inode_fields(
        metadata,
        inode_number,
        0x41ed,
        metadata.block_size as u32,
        2,
        &inode_blocks,
        0,
        0,
        u32::try_from(metadata.block_size / 512).map_err(|_| Ext2Error::InvalidFilesystem)?,
        true,
        None,
    )?;
    write_block(metadata, block_bitmap_number, &block_bitmap)?;
    write_block(metadata, inode_bitmap_number, &inode_bitmap)?;
    update_free_counters(metadata, &mut descriptor, -1, -1, None)?;
    update_directory_link_count(metadata, parent_number, true)?;
    add_directory_entry(metadata, parent, name, inode_number, 2, None)
}

fn remove_root_entry_allocations(
    metadata: Metadata,
    parent_number: u32,
    parent: Inode,
    name: &[u8],
    inode_number: u32,
    inode: Inode,
    data_blocks: &[u32],
) -> Result<(), Ext2Error> {
    let expected_links = if inode.mode & 0xf000 == 0x4000 { 2 } else { 1 };
    if inode.links != expected_links {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let (
        block_bitmap_number,
        inode_bitmap_number,
        mut descriptor,
        mut block_bitmap,
        mut inode_bitmap,
    ) = read_allocation_tables(metadata)?;
    update_inode_bitmap(&mut inode_bitmap, inode_number, false)?;
    for &block in data_blocks {
        update_block_bitmap(metadata, &mut block_bitmap, block, false)?;
    }
    remove_directory_entry(metadata, parent, name)?;
    write_block(metadata, block_bitmap_number, &block_bitmap)?;
    write_block(metadata, inode_bitmap_number, &inode_bitmap)?;
    update_free_counters(
        metadata,
        &mut descriptor,
        i32::try_from(data_blocks.len()).map_err(|_| Ext2Error::InvalidFilesystem)?,
        1,
        None,
    )?;
    let empty_blocks = [0; DIRECT_BLOCKS];
    write_inode_fields(
        metadata,
        inode_number,
        0,
        0,
        0,
        &empty_blocks,
        0,
        0,
        0,
        true,
        None,
    )?;
    if inode.mode & 0xf000 == 0x4000 {
        update_directory_link_count(metadata, parent_number, false)?;
    }
    Ok(())
}

pub fn create_directory(path: &str) -> Result<(), Ext2Error> {
    let _guard = EXT2_WRITE_LOCK.lock();
    let metadata = writable_metadata()?;
    let (parent_number, parent, name) = resolve_parent(metadata, path)?;
    if find_child(metadata, parent, name)?.is_some() {
        return Err(Ext2Error::AlreadyExists);
    }
    create_directory_in(metadata, parent_number, parent, name)
}

pub fn remove_file(path: &str) -> Result<(), Ext2Error> {
    let _guard = EXT2_WRITE_LOCK.lock();
    let metadata = writable_metadata()?;
    let (parent_number, parent, name) = resolve_parent(metadata, path)?;
    let inode_number = find_child(metadata, parent, name)?.ok_or(Ext2Error::NotFound)?;
    let inode = read_inode(metadata, inode_number)?;
    if inode.mode & 0xf000 != 0x8000 {
        return Err(Ext2Error::InvalidPath);
    }
    if inode.size > (double_indirect_capacity(metadata)? * metadata.block_size) as u64 {
        return Err(Ext2Error::UnsupportedFeature);
    }
    let blocks = collect_file_allocations(metadata, inode)?;
    remove_root_entry_allocations(
        metadata,
        parent_number,
        parent,
        name,
        inode_number,
        inode,
        &blocks,
    )
}

pub fn remove_empty_directory(path: &str) -> Result<(), Ext2Error> {
    let _guard = EXT2_WRITE_LOCK.lock();
    let metadata = writable_metadata()?;
    let (parent_number, parent, name) = resolve_parent(metadata, path)?;
    let inode_number = find_child(metadata, parent, name)?.ok_or(Ext2Error::NotFound)?;
    let inode = read_inode(metadata, inode_number)?;
    if inode.mode & 0xf000 != 0x4000 {
        return Err(Ext2Error::InvalidPath);
    }
    let mut empty = true;
    for_each_child(inode, |_, _, _| empty = false)?;
    if !empty {
        return Err(Ext2Error::NotEmpty);
    }
    if inode.size != metadata.block_size as u64
        || inode.single_indirect != 0
        || inode.double_indirect != 0
        || inode.blocks[0] == 0
        || inode.blocks[1..].iter().any(|block| *block != 0)
    {
        return Err(Ext2Error::UnsupportedFeature);
    }
    remove_root_entry_allocations(
        metadata,
        parent_number,
        parent,
        name,
        inode_number,
        inode,
        &[inode.blocks[0]],
    )
}

pub fn write_file(path: &str, contents: &[u8]) -> Result<(), Ext2Error> {
    let _guard = EXT2_WRITE_LOCK.lock();
    let metadata = writable_metadata()?;
    let (_, parent, name) = resolve_parent(metadata, path)?;
    let pointer_capacity = metadata.block_size / 4;
    let single_capacity = single_indirect_capacity(metadata)?;
    let max_file_size = metadata
        .block_size
        .checked_mul(double_indirect_capacity(metadata)?)
        .ok_or(Ext2Error::UnsupportedFeature)?;
    if contents.len() > max_file_size || contents.len() > u32::MAX as usize {
        return Err(Ext2Error::UnsupportedFeature);
    }

    let (inode_number, existing_inode) = match find_child(metadata, parent, name)? {
        Some(inode_number) => {
            let inode = read_inode(metadata, inode_number)?;
            if inode.mode & 0xf000 != 0x8000 {
                return Err(Ext2Error::InvalidPath);
            }
            if inode.size > max_file_size as u64 {
                return Err(Ext2Error::UnsupportedFeature);
            }
            (inode_number, Some(inode))
        }
        None => (0, None),
    };
    let new_inode = existing_inode.is_none();
    if new_inode {
        ensure_directory_entry_space(metadata, parent, name)?;
    }

    let old_blocks = match existing_inode {
        Some(inode) => collect_file_allocations(metadata, inode)?,
        None => Vec::new(),
    };
    let (
        block_bitmap_number,
        inode_bitmap_number,
        mut descriptor,
        mut block_bitmap,
        mut inode_bitmap,
    ) = read_allocation_tables(metadata)?;
    let allocated_inode = if new_inode {
        find_free_bit(&inode_bitmap, metadata.inodes_count)
            .and_then(|index| index.checked_add(1))
            .ok_or(Ext2Error::NoSpace)?
    } else {
        inode_number
    };
    let required_blocks = contents.len().div_ceil(metadata.block_size);
    let mut data_blocks = Vec::new();
    data_blocks
        .try_reserve_exact(required_blocks)
        .map_err(|_| Ext2Error::AllocationFailed)?;
    data_blocks.resize(required_blocks, 0);
    let mut single_indirect = 0;
    let mut double_indirect = 0;
    let required_single_indirect = required_blocks > DIRECT_BLOCKS;
    let required_double_leaves = required_blocks
        .saturating_sub(single_capacity)
        .div_ceil(pointer_capacity);
    let mut double_leaves = Vec::new();
    double_leaves
        .try_reserve_exact(required_double_leaves)
        .map_err(|_| Ext2Error::AllocationFailed)?;
    double_leaves.resize(required_double_leaves, 0);
    if required_single_indirect {
        single_indirect = allocate_data_block(metadata, &mut block_bitmap)?;
    }
    if required_double_leaves != 0 {
        double_indirect = allocate_data_block(metadata, &mut block_bitmap)?;
    }
    for leaf in &mut double_leaves {
        *leaf = allocate_data_block(metadata, &mut block_bitmap)?;
    }
    for data_block in &mut data_blocks {
        *data_block = allocate_data_block(metadata, &mut block_bitmap)?;
    }
    let allocated_blocks = required_blocks
        + usize::from(required_single_indirect)
        + usize::from(required_double_leaves != 0)
        + required_double_leaves;
    let allocated_block_count =
        i32::try_from(allocated_blocks).map_err(|_| Ext2Error::InvalidFilesystem)?;

    for (index, contents_chunk) in contents.chunks(metadata.block_size).enumerate() {
        let mut data = zeroed_block();
        data[..contents_chunk.len()].copy_from_slice(contents_chunk);
        write_block(metadata, data_blocks[index], &data)?;
    }
    if single_indirect != 0 {
        let mut pointer_block = zeroed_block();
        for index in 0..pointer_capacity {
            let block = data_blocks.get(DIRECT_BLOCKS + index).copied().unwrap_or(0);
            pointer_block[index * 4..index * 4 + 4].copy_from_slice(&block.to_le_bytes());
        }
        write_block(metadata, single_indirect, &pointer_block)?;
    }
    if required_double_leaves != 0 {
        let mut root = zeroed_block();
        for (index, leaf) in double_leaves.iter().copied().enumerate() {
            root[index * 4..index * 4 + 4].copy_from_slice(&leaf.to_le_bytes());
        }
        for outer_index in 0..required_double_leaves {
            let mut pointers = zeroed_block();
            for inner_index in 0..pointer_capacity {
                let logical_index = single_capacity + outer_index * pointer_capacity + inner_index;
                let block = data_blocks.get(logical_index).copied().unwrap_or(0);
                pointers[inner_index * 4..inner_index * 4 + 4]
                    .copy_from_slice(&block.to_le_bytes());
            }
            write_block(metadata, double_leaves[outer_index], &pointers)?;
        }
        write_block(metadata, double_indirect, &root)?;
    }
    let mut direct_blocks = [0; DIRECT_BLOCKS];
    for (index, block) in data_blocks.iter().take(DIRECT_BLOCKS).enumerate() {
        direct_blocks[index] = *block;
    }
    let sectors_per_block =
        u32::try_from(metadata.block_size / 512).map_err(|_| Ext2Error::InvalidFilesystem)?;
    let allocated_blocks_512 = u32::try_from(allocated_blocks)
        .ok()
        .and_then(|blocks| blocks.checked_mul(sectors_per_block))
        .ok_or(Ext2Error::InvalidFilesystem)?;

    let mut transaction = BlockTransaction::new();
    let commit_result = (|| {
        if allocated_blocks != 0 {
            transaction.write(metadata, block_bitmap_number, &block_bitmap)?;
        }
        if new_inode {
            let bit = allocated_inode - 1;
            inode_bitmap[bit as usize / 8] |= 1 << (bit % 8);
            transaction.write(metadata, inode_bitmap_number, &inode_bitmap)?;
        }
        update_free_counters(
            metadata,
            &mut descriptor,
            -allocated_block_count,
            -i32::from(new_inode),
            Some(&mut transaction),
        )?;

        write_inode_fields(
            metadata,
            allocated_inode,
            0x81a4,
            u32::try_from(contents.len()).map_err(|_| Ext2Error::UnsupportedFeature)?,
            1,
            &direct_blocks,
            single_indirect,
            double_indirect,
            allocated_blocks_512,
            new_inode,
            Some(&mut transaction),
        )?;
        if new_inode {
            add_directory_entry(
                metadata,
                parent,
                name,
                allocated_inode,
                1,
                Some(&mut transaction),
            )?;
        }
        Ok(())
    })();
    if let Err(error) = commit_result {
        transaction.rollback(metadata)?;
        return Err(error);
    }
    let mut cleanup_transaction = BlockTransaction::new();
    if let Err(error) = free_data_blocks(metadata, &old_blocks, Some(&mut cleanup_transaction)) {
        let cleanup_rollback = cleanup_transaction.rollback(metadata);
        let commit_rollback = transaction.rollback(metadata);
        if cleanup_rollback.is_err() || commit_rollback.is_err() {
            return Err(Ext2Error::RecoveryFailed);
        }
        return Err(error);
    }
    Ok(())
}

/// Return the free data-block count recorded in the mounted ext2 superblock.
pub fn free_block_count() -> Result<u32, Ext2Error> {
    let metadata = metadata()?;
    let mut superblock = zeroed_block();
    read_block(metadata, metadata.first_data_block, &mut superblock)?;
    Ok(le32(&superblock, 12))
}

fn for_each_child(
    directory: Inode,
    mut visit: impl FnMut(&str, bool, u64),
) -> Result<(), Ext2Error> {
    let metadata = metadata()?;
    if directory.mode & 0xf000 != 0x4000 {
        return Ok(());
    }
    let block_count = inode_block_count(metadata, directory)?;
    let mut remaining =
        usize::try_from(directory.size).map_err(|_| Ext2Error::UnsupportedFeature)?;
    let mut block = zeroed_block();
    let mut indirect_data = zeroed_block();
    let mut double_indirect_data = zeroed_block();
    let mut cached_indirect_block = 0;
    let mut cached_double_indirect_block = 0;
    for logical_index in 0..block_count {
        let block_number = inode_data_block(
            metadata,
            directory,
            logical_index,
            &mut indirect_data,
            &mut cached_indirect_block,
            &mut double_indirect_data,
            &mut cached_double_indirect_block,
        )?;
        if block_number == 0 {
            return Err(Ext2Error::InvalidFilesystem);
        }
        read_block(metadata, block_number, &mut block)?;
        let valid_bytes = remaining.min(metadata.block_size);
        let mut offset = 0;
        while offset + 8 <= valid_bytes {
            let inode_number = le32(&block, offset);
            let record_len = usize::from(le16(&block, offset + 4));
            if record_len < 8 || !record_len.is_multiple_of(4) || offset + record_len > valid_bytes
            {
                return Err(Ext2Error::InvalidFilesystem);
            }
            let name_len = if metadata.incompat_features & FEATURE_INCOMPAT_FILETYPE != 0 {
                usize::from(block[offset + 6])
            } else {
                usize::from(le16(&block, offset + 6))
            };
            if name_len > record_len - 8 {
                return Err(Ext2Error::InvalidFilesystem);
            }
            if inode_number != 0 {
                if inode_number > metadata.inodes_count {
                    return Err(Ext2Error::InvalidFilesystem);
                }
                let name = &block[offset + 8..offset + 8 + name_len];
                if name != b"." && name != b".." {
                    if !safe_component(name) {
                        return Err(Ext2Error::UnsupportedFeature);
                    }
                    let inode = read_inode(metadata, inode_number)?;
                    let is_dir = inode.mode & 0xf000 == 0x4000;
                    if !is_dir && inode.mode & 0xf000 != 0x8000 {
                        return Err(Ext2Error::UnsupportedFeature);
                    }
                    let name =
                        core::str::from_utf8(name).map_err(|_| Ext2Error::UnsupportedFeature)?;
                    visit(name, is_dir, inode.size);
                }
            }
            offset += record_len;
        }
        remaining = remaining.saturating_sub(metadata.block_size);
    }
    Ok(())
}

pub fn directory_exists(path: &str) -> Result<bool, Ext2Error> {
    Ok(lookup(path)?.is_some_and(|inode| inode.mode & 0xf000 == 0x4000))
}

pub fn for_each_entry(path: &str, visit: impl FnMut(&str, bool, u64)) -> Result<(), Ext2Error> {
    match lookup(path)? {
        Some(directory) if directory.mode & 0xf000 == 0x4000 => for_each_child(directory, visit),
        Some(_) => Ok(()),
        None => Ok(()),
    }
}

pub fn read_file(path: &str, output: &mut [u8]) -> Result<Option<(usize, u64)>, Ext2Error> {
    let metadata = metadata()?;
    let Some(inode) = lookup(path)? else {
        return Ok(None);
    };
    if inode.mode & 0xf000 != 0x8000 {
        return Ok(None);
    }
    read_inode_contents(metadata, inode, output).map(Some)
}

pub fn read_file_vec(path: &str, max_size: usize) -> Result<Option<Vec<u8>>, Ext2Error> {
    let metadata = metadata()?;
    let Some(inode) = lookup(path)? else {
        return Ok(None);
    };
    if inode.mode & 0xf000 != 0x8000 {
        return Ok(None);
    }
    let size = usize::try_from(inode.size).map_err(|_| Ext2Error::UnsupportedFeature)?;
    if size > max_size {
        return Err(Ext2Error::FileTooLarge);
    }
    let mut contents = Vec::new();
    contents
        .try_reserve_exact(size)
        .map_err(|_| Ext2Error::AllocationFailed)?;
    contents.resize(size, 0);
    read_inode_contents(metadata, inode, &mut contents)?;
    Ok(Some(contents))
}

fn read_inode_contents(
    metadata: Metadata,
    inode: Inode,
    output: &mut [u8],
) -> Result<(usize, u64), Ext2Error> {
    let block_count = inode_block_count(metadata, inode)?;
    let copy_len = usize::try_from(inode.size)
        .unwrap_or(usize::MAX)
        .min(output.len());
    let mut copied = 0;
    let mut block = zeroed_block();
    let mut indirect_data = zeroed_block();
    let mut double_indirect_data = zeroed_block();
    let mut cached_indirect_block = 0;
    let mut cached_double_indirect_block = 0;
    while copied < copy_len {
        let block_index = copied / metadata.block_size;
        let block_offset = copied % metadata.block_size;
        if block_index >= block_count {
            return Err(Ext2Error::InvalidFilesystem);
        }
        let block_number = inode_data_block(
            metadata,
            inode,
            block_index,
            &mut indirect_data,
            &mut cached_indirect_block,
            &mut double_indirect_data,
            &mut cached_double_indirect_block,
        )?;
        let amount = (metadata.block_size - block_offset).min(copy_len - copied);
        if block_number == 0 {
            output[copied..copied + amount].fill(0);
        } else {
            read_block(metadata, block_number, &mut block)?;
            output[copied..copied + amount]
                .copy_from_slice(&block[block_offset..block_offset + amount]);
        }
        copied += amount;
    }
    Ok((copied, inode.size))
}
