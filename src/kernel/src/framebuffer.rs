use core::fmt;
use core::ptr::NonNull;
use crate::boot_info::{self, BootInfo};
use spin::{Mutex, Once};


#[derive(Debug, Clone, Copy)]
pub struct FramebufferBuffer {
    ptr: NonNull<u8>,
    size: usize,
}

unsafe impl Send for FramebufferBuffer {}
unsafe impl Sync for FramebufferBuffer {}

impl FramebufferBuffer {
    pub unsafe fn new(ptr: *mut u8, size: usize) -> Option<Self> {
        NonNull::new(ptr).map(|ptr| Self { ptr, size })
    }

    #[inline]
    pub fn write_volatile(&mut self, offset: usize, value: u8) {
        if offset < self.size {
            unsafe {
                self.ptr.as_ptr().add(offset).write_volatile(value);
            }
        }
    }

    pub fn write_bytes(&mut self, offset: usize, count: usize, value: u8) {
        if offset + count <= self.size {
            unsafe {
                core::ptr::write_bytes(self.ptr.as_ptr().add(offset), value, count);
            }
        }
    }


    pub fn copy_within(&mut self, src_offset: usize, dst_offset: usize, count: usize) {
        if src_offset + count <= self.size && dst_offset + count <= self.size {
            unsafe {
                core::ptr::copy(
                    self.ptr.as_ptr().add(src_offset),
                    self.ptr.as_ptr().add(dst_offset),
                    count,
                );
            }
        }
    }
}

pub struct FramebufferInfo {
    pub buffer: FramebufferBuffer,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub bgr: bool,
}

impl FramebufferInfo {
    pub unsafe fn from_boot_info(boot_info: &BootInfo) -> Option<Self> {
        if boot_info.framebuffer_base == 0 
            || boot_info.framebuffer_pixel_format == boot_info::PIXEL_FORMAT_BLT_ONLY {
            return None;
        }

        let bytes_per_pixel = 4usize;
        let width = boot_info.framebuffer_width as usize;
        let height = boot_info.framebuffer_height as usize;
        let stride = boot_info.framebuffer_pixels_per_scan_line as usize * bytes_per_pixel;
        let size = stride * height;

        let buffer = unsafe { FramebufferBuffer::new(boot_info.framebuffer_base as *mut u8, size)?};

        Some(Self {
            buffer,
            width,
            height,
            stride,
            bytes_per_pixel,
            bgr: boot_info.framebuffer_pixel_format != boot_info::PIXEL_FORMAT_RGB,
        })
    }
}

impl FramebufferBuffer {
    pub fn addr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }
}

const MARGIN: usize = 8;
const CHAR_WIDTH: usize = 8;
const LINE_HEIGHT: usize = 16;
const DEFAULT_FG: (u8, u8, u8) = (0, 255, 0); // terminal green

pub struct FramebufferWriter {
    info: FramebufferInfo,
    x: usize,
    y: usize,
    fg: (u8, u8, u8),
}

impl FramebufferWriter {
    pub fn new(info: FramebufferInfo) -> Self {
        let mut writer = Self {
            info,
            x: MARGIN,
            y: MARGIN,
            fg: DEFAULT_FG,
        };
        writer.clear();
        writer
    }

    pub fn set_color(&mut self, r: u8, g: u8, b: u8) {
        self.fg = (r, g, b);
    }

    pub fn clear(&mut self) {
        let total_size = self.info.stride * self.info.height;
        self.info.buffer.write_bytes(0, total_size, 0);
        self.x = MARGIN;
        self.y = MARGIN;
    }

    fn clear_rows(&mut self, start_row: usize, count: usize) {
        let stride = self.info.stride;
        let offset = start_row * stride;
        self.info.buffer.write_bytes(offset, count * stride, 0);
    }

    fn scroll_up(&mut self, rows: usize) {
        let stride = self.info.stride;
        let height = self.info.height;
        if rows == 0 {
            return;
        }
        if rows >= height {
            self.clear();
            return;
        }
        let src_offset = rows * stride;
        let count = (height - rows) * stride;

        self.info.buffer.copy_within(src_offset, 0, count);
        self.clear_rows(height - rows, rows);
    }

    pub fn write_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let offset = y * self.info.stride + x * self.info.bytes_per_pixel;

        if self.info.bgr {
            self.info.buffer.write_volatile(offset, b);
            self.info.buffer.write_volatile(offset + 1, g);
            self.info.buffer.write_volatile(offset + 2, r);
        } else {
            self.info.buffer.write_volatile(offset, r);
            self.info.buffer.write_volatile(offset + 1, g);
            self.info.buffer.write_volatile(offset + 2, b);
        }
    }

    pub fn new_line(&mut self) {
        self.x = MARGIN;
        if self.y + LINE_HEIGHT + MARGIN + LINE_HEIGHT > self.info.height {
            self.scroll_up(LINE_HEIGHT);
        } else {
            self.y += LINE_HEIGHT;
        }
    }

    pub fn draw_ascii_char(&mut self, c: u8) {
        let glyph = get_glyph(c);
        let (r, g, b) = self.fg;
        for row in 0..8 {
            let row_data = glyph[row];
            for col in 0..8 {
                if (row_data >> (7 - col)) & 1 == 1 {
                    self.write_pixel(self.x + col, self.y + row, r, g, b);
                }
            }
        }
        self.x += CHAR_WIDTH;
    }
}

// Basic 8x8 bitmap lookup table
fn get_glyph(c: u8) -> [u8; 8] {
    let c = if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    };
    match c {
        b'A' => [0x18, 0x3C, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        b'C' => [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00],
        b'D' => [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'G' => [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3C, 0x00],
        b'H' => [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'J' => [0x0E, 0x06, 0x06, 0x06, 0x66, 0x66, 0x3C, 0x00],
        b'K' => [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00],
        b'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        b'M' => [0xC3, 0xE7, 0xFF, 0xDB, 0xC3, 0xC3, 0xC3, 0x00],
        b'N' => [0x63, 0x73, 0x7B, 0x6F, 0x67, 0x63, 0x63, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'Q' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x6C, 0x3E, 0x06],
        b'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        b'U' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'V' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'W' => [0xC3, 0xC3, 0xC3, 0xDB, 0xFF, 0xE7, 0xC3, 0x00],
        b'X' => [0x66, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x66, 0x00],
        b'Y' => [0x66, 0x66, 0x3C, 0x18, 0x18, 0x18, 0x18, 0x00],
        b'Z' => [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x7E, 0x00],

        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'2' => [0x3C, 0x66, 0x06, 0x0C, 0x18, 0x30, 0x7E, 0x00],
        b'3' => [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00],
        b'4' => [0x0C, 0x1C, 0x2C, 0x4C, 0x7E, 0x0C, 0x0C, 0x00],
        b'5' => [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00],
        b'6' => [0x1C, 0x30, 0x60, 0x7C, 0x66, 0x66, 0x3C, 0x00],
        b'7' => [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x00],
        b'8' => [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00],
        b'9' => [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x0C, 0x38, 0x00],

        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00],
        b',' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x30],
        b':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        b';' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x30, 0x00],
        b'!' => [0x18, 0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00],
        b'?' => [0x3C, 0x66, 0x0C, 0x18, 0x18, 0x00, 0x18, 0x00],
        b'-' => [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00],
        b'_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF],
        b'/' => [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0xC0, 0x00],
        b'(' => [0x0C, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0C, 0x00],
        b')' => [0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x18, 0x30, 0x00],
        b'%' => [0x66, 0x66, 0x0C, 0x18, 0x30, 0x66, 0x66, 0x00],
        b'+' => [0x00, 0x18, 0x18, 0x7E, 0x18, 0x18, 0x00, 0x00],
        b'=' => [0x00, 0x00, 0x7E, 0x00, 0x7E, 0x00, 0x00, 0x00],
        b'\'' => [0x18, 0x18, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'"' => [0x66, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'#' => [0x24, 0x7E, 0x24, 0x24, 0x7E, 0x24, 0x00, 0x00],
        b'*' => [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],

        _ => [0xFF, 0x81, 0x81, 0x81, 0x81, 0x81, 0x81, 0xFF], // Default box outline
    }
}

impl fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            match byte {
                b'\n' => self.new_line(),
                byte => {
                    if self.x + CHAR_WIDTH + MARGIN >= self.info.width {
                        self.new_line();
                    }
                    self.draw_ascii_char(byte);
                }
            }
        }
        Ok(())
    }
}

// Global writer and macros

static WRITER: Once<Mutex<FramebufferWriter>> = Once::new();

pub fn init(info: FramebufferInfo) {
    WRITER.call_once(|| Mutex::new(FramebufferWriter::new(info)));
}

pub fn is_initialized() -> bool {
    WRITER.get().is_some()
}

pub fn set_color(r: u8, g: u8, b: u8) {
    if let Some(writer) = WRITER.get() {
        writer.lock().set_color(r, g, b);
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    if let Some(writer) = WRITER.get() {
        use core::fmt::Write;
        writer.lock().write_fmt(args).ok();
    }
}

#[macro_export]
macro_rules! gop_print {
    ($($arg:tt)*) => ($crate::framebuffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! gop_println {
    () => ($crate::gop_print!("\n"));
    ($($arg:tt)*) => ($crate::gop_print!("{}\n", format_args!($($arg)*)));
}