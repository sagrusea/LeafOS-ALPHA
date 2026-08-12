use core::fmt;
use crate::boot_info::{self, BootInfo};

pub struct FramebufferInfo {
    pub address: *mut u8,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub bgr: bool,
}

impl FramebufferInfo {
    pub unsafe fn from_boot_info(boot_info: &BootInfo) -> Option<Self> {
        if boot_info.framebuffer_base == 0 || boot_info.framebuffer_pixel_format == boot_info::PIXEL_FORMAT_BLT_ONLY {
            return None;
        }
        let bytes_per_pixel = 4usize;

        Some(Self {
            address: boot_info.framebuffer_base as *mut u8,
            width: boot_info.framebuffer_width as usize,
            height: boot_info.framebuffer_height as usize,
            stride: boot_info.framebuffer_pixels_per_scan_line as usize * bytes_per_pixel,
            bytes_per_pixel,
            bgr: boot_info.framebuffer_pixel_format != boot_info::PIXEL_FORMAT_RGB,
        })
    }
}

pub struct FramebufferWriter {
    info: FramebufferInfo,
    x: usize,
    y: usize,
}

impl FramebufferWriter {
    pub fn new(info: FramebufferInfo) -> Self {
        Self { info, x: 0, y: 0 }
    }

    pub fn write_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let offset = y * self.info.stride + x * self.info.bytes_per_pixel;
        unsafe {
            let ptr = self.info.address.add(offset);
            if self.info.bgr {
                ptr.add(0).write_volatile(b);
                ptr.add(1).write_volatile(g);
                ptr.add(2).write_volatile(r);
            } else {
                ptr.add(0).write_volatile(r);
                ptr.add(1).write_volatile(g);
                ptr.add(2).write_volatile(b);
            }
        }
    }

    pub fn new_line(&mut self) {
        self.x = 0;
        self.y += 16;
    }

    pub fn draw_ascii_char(&mut self, c: u8, r: u8, g: u8, b: u8) {
        let glyph = get_glyph(c);
        for row in 0..8 {
            let row_data = glyph[row];
            for col in 0..8 {
                if (row_data >> (7 - col)) & 1 == 1 {
                    self.write_pixel(self.x + col, self.y + row, r, g, b);
                }
            }
        }
        self.x += 8;
    }
}

// Basic 8x8 bitmap lookup table
fn get_glyph(c: u8) -> [u8; 8] {
    match c {
        b'A' => [0x18, 0x3C, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'K' => [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00],
        b'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        _ => [0xFF, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0xFF], // Default box outline
    }
}

impl fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            match byte {
                b'\n' => self.new_line(),
                byte => {
                    if self.x + 8 >= self.info.width {
                        self.new_line();
                    }
                    self.draw_ascii_char(byte, 255, 255, 255);
                }
            }
        }
        Ok(())
    }
}