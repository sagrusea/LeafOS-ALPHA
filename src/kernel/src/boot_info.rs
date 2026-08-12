use core::num::FpCategory::Infinite;

const BOOT_INFO_MAGIC: u64 = 0x5255_5354_424F_4F54;

pub const PIXEL_FORMAT_RGB: u32 = 0;
pub const PIXEL_FORMAT_BGR: u32 = 1;

pub const PIXEL_FORMAT_BITMASK: u32 = 2;
pub const PIXEL_FORMAT_BLT_ONLY: u32 = 3;

#[repr(C)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    pub _padding0: u32,
 
    pub system_table: *mut core::ffi::c_void,
 
    pub memory_map: *mut core::ffi::c_void,
    pub memory_map_size: usize,
    pub descriptor_size: usize,
    pub descriptor_version: u32,
    pub _padding1: u32,
 
    pub kernel_base: u64,
    pub kernel_file_size: u64,
    pub kernel_reserved_size: u64,
 
    pub framebuffer_base: u64,
    pub framebuffer_size: u64,
    pub framebuffer_width: u32,
    pub framebuffer_height: u32,
    pub framebuffer_pixels_per_scan_line: u32,
    pub framebuffer_pixel_format: u32,
}

impl BootInfo {
    pub unsafe fn from_ptr<'a>(ptr: *const BootInfo) -> Option<&'a BootInfo> {
        if ptr.is_null() {
            return None;
        }
        let info = unsafe { &*ptr };
        if info.magic != BOOT_INFO_MAGIC {
            return None;
        }
        Some(info)
    }
}