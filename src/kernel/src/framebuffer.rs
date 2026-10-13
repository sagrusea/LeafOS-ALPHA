use crate::boot_info::{self, BootInfo};
use crate::misc::font::get_glyph;
use core::fmt;
use core::ptr::NonNull;
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
            || boot_info.framebuffer_pixel_format == boot_info::PIXEL_FORMAT_BLT_ONLY
        {
            return None;
        }

        let bytes_per_pixel = 4usize;
        let width = boot_info.framebuffer_width as usize;
        let height = boot_info.framebuffer_height as usize;
        let stride = boot_info.framebuffer_pixels_per_scan_line as usize * bytes_per_pixel;
        let size = stride * height;

        let buffer =
            unsafe { FramebufferBuffer::new(boot_info.framebuffer_base as *mut u8, size)? };

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

    pub fn backspace(&mut self) {
        if self.x == MARGIN {
            return;
        }
        self.x -= CHAR_WIDTH;
        for y in self.y..self.y + 8 {
            for x in self.x..self.x + CHAR_WIDTH {
                self.write_pixel(x, y, 0, 0, 0);
            }
        }
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

impl fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            match byte {
                b'\n' => self.new_line(),
                b'\r' => {}
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

pub fn clear() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().clear();
        }
    });
}

pub fn backspace() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().backspace();
        }
    });
}

pub fn set_color(r: u8, g: u8, b: u8) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().set_color(r, g, b);
        }
    });
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            use core::fmt::Write;
            writer.lock().write_fmt(args).ok();
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_WIDTH: usize = 640;
    const TEST_HEIGHT: usize = 480;
    const TEST_STRIDE: usize = TEST_WIDTH * 4;

    static mut TEST_BUFFER: [u8; TEST_STRIDE * TEST_HEIGHT] = [0; TEST_STRIDE * TEST_HEIGHT];

    fn init_test_framebuffer() {
        let buffer = unsafe {
            FramebufferBuffer::new(
                core::ptr::addr_of_mut!(TEST_BUFFER).cast::<u8>(),
                TEST_STRIDE * TEST_HEIGHT,
            )
            .expect("test framebuffer pointer must not be null")
        };

        init(FramebufferInfo {
            buffer,
            width: TEST_WIDTH,
            height: TEST_HEIGHT,
            stride: TEST_STRIDE,
            bytes_per_pixel: 4,
            bgr: false,
        });
    }

    fn reset_writer() {
        WRITER
            .get()
            .expect("test framebuffer must be initialized")
            .lock()
            .clear();
    }

    #[test_case]
    fn test_gop_println_simple() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            gop_println!("test_gop_println_simple output");

            let writer = WRITER.get().unwrap().lock();
            assert_eq!(writer.x, MARGIN);
            assert_eq!(writer.y, MARGIN + LINE_HEIGHT);
        });
    }

    #[test_case]
    fn test_gop_println_many() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            for _ in 0..200 {
                gop_println!("test_gop_println_many output");
            }

            let writer = WRITER.get().unwrap().lock();
            assert_eq!(writer.x, MARGIN);
            assert_eq!(writer.y, TEST_HEIGHT - MARGIN - LINE_HEIGHT);
        });
    }

    #[test_case]
    fn test_gop_println_output() {
        init_test_framebuffer();
        let text = "Some test string that fits on a single line";

        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            gop_println!("{}", text);

            let writer = WRITER.get().unwrap().lock();
            assert_eq!(writer.y, MARGIN + LINE_HEIGHT);

            for (char_index, byte) in text.bytes().enumerate() {
                let glyph = get_glyph(byte);
                for (row, glyph_row) in glyph.iter().enumerate() {
                    for col in 0..CHAR_WIDTH {
                        let expected_green = (glyph_row >> (7 - col)) & 1 == 1;
                        let pixel_offset = (MARGIN + row) * writer.info.stride
                            + (MARGIN + char_index * CHAR_WIDTH + col)
                                * writer.info.bytes_per_pixel;
                        let green = unsafe {
                            writer
                                .info
                                .buffer
                                .ptr
                                .as_ptr()
                                .add(pixel_offset + 1)
                                .read_volatile()
                        };

                        assert_eq!(green == DEFAULT_FG.1, expected_green);
                    }
                }
            }
        });
    }

    #[test_case]
    fn test_gop_print_handles_crlf() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            gop_print!("first\r\nsecond");

            let writer = WRITER.get().unwrap().lock();
            assert_eq!(writer.x, MARGIN + "second".len() * CHAR_WIDTH);
            assert_eq!(writer.y, MARGIN + LINE_HEIGHT);
        });
    }
}
