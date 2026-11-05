use crate::sync::Once;

const MAX_PROCESSORS: usize = 64;

#[derive(Clone, Copy)]
pub struct ApicInfo {
    pub local_apic_address: u64,
    pub io_apic_address: u64,
    pub io_apic_gsi_base: u32,
    pub pit_gsi: u32,
    pub keyboard_gsi: u32,
    pub mouse_gsi: u32,
    pub processor_count: usize,
    pub processor_ids: [u32; MAX_PROCESSORS],
}

static APIC_INFO: Once<Option<ApicInfo>> = Once::new();

fn checksum(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

unsafe fn read_u32(address: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(address as *const u32) }
}

unsafe fn read_u64(address: usize) -> u64 {
    unsafe { core::ptr::read_unaligned(address as *const u64) }
}

unsafe fn table(address: usize, physical_offset: u64) -> Option<(*const u8, usize)> {
    let virtual_address = address.checked_add(physical_offset as usize)?;
    let header = virtual_address as *const u8;
    let length = unsafe { read_u32(virtual_address + 4) } as usize;
    if !(36..=1024 * 1024).contains(&length) {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(header, length) };
    checksum(bytes).then_some((header, length))
}

pub fn initialize(rsdp: u64, physical_offset: u64) {
    APIC_INFO.call_once(|| unsafe { parse(rsdp, physical_offset) });
}

pub fn info() -> Option<ApicInfo> {
    APIC_INFO.get().copied().flatten()
}

unsafe fn parse(rsdp: u64, physical_offset: u64) -> Option<ApicInfo> {
    if rsdp == 0 {
        return None;
    }
    let rsdp_address = (rsdp as usize).checked_add(physical_offset as usize)?;
    let signature = unsafe { core::slice::from_raw_parts(rsdp_address as *const u8, 8) };
    if signature != b"RSD PTR " {
        return None;
    }
    let revision = unsafe { (rsdp_address as *const u8).add(15).read() };
    if revision == 0 {
        let bytes = unsafe { core::slice::from_raw_parts(rsdp_address as *const u8, 20) };
        if !checksum(bytes) {
            return None;
        }
    } else {
        let length = unsafe { read_u32(rsdp_address + 20) } as usize;
        if !(36..=4096).contains(&length) {
            return None;
        }
        let bytes = unsafe { core::slice::from_raw_parts(rsdp_address as *const u8, length) };
        if !checksum(bytes) {
            return None;
        }
    }

    let xsdt = if revision >= 2 {
        unsafe { read_u64(rsdp_address + 24) }
    } else {
        0
    };
    let rsdt = unsafe { read_u32(rsdp_address + 16) } as u64;
    let (root_address, entry_size) = if xsdt != 0 {
        (xsdt, 8)
    } else {
        (rsdt, 4)
    };
    let (root, root_length) = unsafe { table(root_address as usize, physical_offset)? };
    let entries = (root_length - 36) / entry_size;

    for index in 0..entries {
        let entry_address = root as usize + 36 + index * entry_size;
        let table_address = if entry_size == 8 {
            unsafe { read_u64(entry_address) }
        } else {
            unsafe { read_u32(entry_address) as u64 }
        };
        let (header, length) = unsafe { table(table_address as usize, physical_offset)? };
        let signature = unsafe { core::slice::from_raw_parts(header, 4) };
        if signature == b"APIC" {
            return unsafe { parse_madt(header, length) };
        }
    }
    None
}

unsafe fn parse_madt(header: *const u8, length: usize) -> Option<ApicInfo> {
    let base = header as usize;
    let local_apic_address = unsafe { read_u32(base + 36) } as u64;
    let mut info = ApicInfo {
        local_apic_address,
        io_apic_address: 0,
        io_apic_gsi_base: 0,
        pit_gsi: 0,
        keyboard_gsi: 1,
        mouse_gsi: 12,
        processor_count: 0,
        processor_ids: [0; MAX_PROCESSORS],
    };
    let mut offset = 44;
    while offset + 2 <= length {
        let entry = unsafe { header.add(offset) };
        let kind = unsafe { entry.read() };
        let entry_length = unsafe { entry.add(1).read() } as usize;
        if entry_length < 2 || offset + entry_length > length {
            return None;
        }
        match kind {
            0 if entry_length >= 8 => {
                let flags = unsafe { read_u32(entry as usize + 4) };
                if flags & 1 != 0 && info.processor_count < MAX_PROCESSORS {
                    info.processor_ids[info.processor_count] =
                        unsafe { entry.add(3).read() } as u32;
                    info.processor_count += 1;
                }
            }
            1 if entry_length >= 12 => {
                info.io_apic_address = unsafe { read_u32(entry as usize + 4) } as u64;
                info.io_apic_gsi_base = unsafe { read_u32(entry as usize + 8) };
            }
            2 if entry_length >= 10 => {
                let source = unsafe { entry.add(3).read() };
                let gsi = unsafe { read_u32(entry as usize + 4) };
                if source == 0 {
                    info.pit_gsi = gsi;
                } else if source == 1 {
                    info.keyboard_gsi = gsi;
                } else if source == 12 {
                    info.mouse_gsi = gsi;
                }
            }
            5 if entry_length >= 12 => {
                info.local_apic_address = unsafe { read_u64(entry as usize + 4) };
            }
            _ => {}
        }
        offset += entry_length;
    }
    (info.io_apic_address != 0).then_some(info)
}
