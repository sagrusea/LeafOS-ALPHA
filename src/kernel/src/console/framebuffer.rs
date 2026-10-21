use crate::boot_info::{self, BootInfo};
use crate::misc::font::get_glyph;
use crate::sync::{Mutex, Once};
use alloc::collections::VecDeque;
use core::fmt;
use core::ptr::NonNull;

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
const INPUT_LINE_CAPACITY: usize = 256;
const SCROLLBACK_LINE_CAPACITY: usize = 256;
const SCROLLBACK_COLUMN_CAPACITY: usize = 256;
const EDITOR_MAX_COLUMNS: usize = 256;
const EDITOR_MAX_ROWS: usize = 128;
const DEFAULT_FG: (u8, u8, u8) = (0, 255, 0); // terminal green

#[derive(Clone, Copy)]
struct TerminalLine {
    id: u64,
    bytes: [u8; SCROLLBACK_COLUMN_CAPACITY],
    colors: [u32; SCROLLBACK_COLUMN_CAPACITY],
}

impl TerminalLine {
    const fn new(id: u64) -> Self {
        Self {
            id,
            bytes: [b' '; SCROLLBACK_COLUMN_CAPACITY],
            colors: [0; SCROLLBACK_COLUMN_CAPACITY],
        }
    }
}

pub struct FramebufferWriter {
    info: FramebufferInfo,
    x: usize,
    y: usize,
    fg: (u8, u8, u8),
    input_origin: Option<(usize, usize)>,
    input_cells: [(usize, usize); INPUT_LINE_CAPACITY],
    input_bytes: [u8; INPUT_LINE_CAPACITY],
    input_cell_count: usize,
    cursor_overlay: Option<(usize, usize, u8)>,
    editor_active: bool,
    editor_cells: [[u8; EDITOR_MAX_COLUMNS]; EDITOR_MAX_ROWS],
    history: VecDeque<TerminalLine>,
    current_line_id: u64,
    scrollback_offset: usize,
    input_origin_history: Option<(u64, usize)>,
    input_history_cells: [(u64, usize); INPUT_LINE_CAPACITY],
}

impl FramebufferWriter {
    pub fn new(info: FramebufferInfo) -> Self {
        let mut writer = Self {
            info,
            x: MARGIN,
            y: MARGIN,
            fg: DEFAULT_FG,
            input_origin: None,
            input_cells: [(0, 0); INPUT_LINE_CAPACITY],
            input_bytes: [b' '; INPUT_LINE_CAPACITY],
            input_cell_count: 0,
            cursor_overlay: None,
            editor_active: false,
            editor_cells: [[b' '; EDITOR_MAX_COLUMNS]; EDITOR_MAX_ROWS],
            history: VecDeque::new(),
            current_line_id: 0,
            scrollback_offset: 0,
            input_origin_history: None,
            input_history_cells: [(0, 0); INPUT_LINE_CAPACITY],
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
        self.history.clear();
        self.history.push_back(TerminalLine::new(0));
        self.current_line_id = 0;
        self.scrollback_offset = 0;
        self.x = MARGIN;
        self.y = MARGIN;
        self.input_origin = None;
        self.input_origin_history = None;
        self.input_cell_count = 0;
        self.cursor_overlay = None;
        self.editor_active = false;
        for row in &mut self.editor_cells {
            row.fill(b' ');
        }
    }

    pub fn backspace(&mut self) {
        if self.x == MARGIN {
            if self.y <= MARGIN {
                return;
            }
            self.y -= LINE_HEIGHT;
            let available = self.info.width.saturating_sub(2 * MARGIN + CHAR_WIDTH + 1);
            self.x = MARGIN + available / CHAR_WIDTH * CHAR_WIDTH + CHAR_WIDTH;
        }
        self.x -= CHAR_WIDTH;
        for y in self.y..self.y + 8 {
            for x in self.x..self.x + CHAR_WIDTH {
                self.write_pixel(x, y, 0, 0, 0);
            }
        }
    }

    fn begin_input_line(&mut self) {
        self.input_origin = Some((self.x, self.y));
        self.input_origin_history = Some((
            self.current_line_id,
            self.x.saturating_sub(MARGIN) / CHAR_WIDTH,
        ));
        self.input_cell_count = 0;
    }

    fn finish_input_line(&mut self) {
        self.remove_cursor_overlay();
        self.input_origin = None;
        self.input_origin_history = None;
        self.input_cell_count = 0;
    }

    fn redraw_input_line(&mut self, bytes: &[u8], cursor: usize) {
        let Some((origin_x, origin_y)) = self.input_origin else {
            return;
        };
        self.remove_cursor_overlay();
        let old_cell_count = self.input_cell_count;
        for index in 0..old_cell_count {
            let (line_id, column) = self.input_history_cells[index];
            self.set_history_cell(line_id, column, b' ', 0);
        }
        let Some((origin_line, _)) = self.input_origin_history else {
            return;
        };
        self.current_line_id = origin_line;
        self.x = origin_x;
        self.y = origin_y;
        let history_color =
            (u32::from(self.fg.0) << 16) | (u32::from(self.fg.1) << 8) | u32::from(self.fg.2);
        for (index, byte) in bytes.iter().copied().enumerate() {
            self.wrap_if_needed();
            let new_cell = (self.x, self.y);
            let history_cell = (
                self.current_line_id,
                self.x.saturating_sub(MARGIN) / CHAR_WIDTH,
            );
            self.input_history_cells[index] = history_cell;
            self.set_history_cell(history_cell.0, history_cell.1, byte, history_color);
            let old_cell = self.input_cells.get(index).copied();
            let changed = index >= old_cell_count
                || self.input_bytes[index] != byte
                || old_cell != Some(new_cell);
            if let Some((old_x, old_y)) = old_cell.filter(|_| index < old_cell_count) {
                if (old_x, old_y) != new_cell {
                    self.clear_character(old_x, old_y);
                }
            }
            if changed {
                self.clear_character(new_cell.0, new_cell.1);
                self.draw_ascii_char(byte);
            } else {
                self.x += CHAR_WIDTH;
            }
            self.input_cells[index] = new_cell;
            self.input_bytes[index] = byte;
            self.input_cell_count = old_cell_count.max(index + 1);
        }
        for index in bytes.len()..old_cell_count {
            let (x, y) = self.input_cells[index];
            self.clear_character(x, y);
        }
        self.trim_history_after(self.current_line_id);
        self.input_cell_count = bytes.len();
        if cursor < bytes.len() {
            (self.x, self.y) = self.input_cells[cursor];
        } else {
            self.wrap_if_needed();
        }
        let cursor_byte = bytes.get(cursor).copied().unwrap_or(b' ');
        self.draw_cursor_overlay(self.x, self.y, cursor_byte);
    }

    fn clear_character(&mut self, x: usize, y: usize) {
        for row in y..y + 8 {
            for col in x..x + CHAR_WIDTH {
                self.write_pixel(col, row, 0, 0, 0);
            }
        }
    }

    fn draw_character_at(&mut self, x: usize, y: usize, byte: u8) {
        self.draw_character_with_color(x, y, byte, self.fg);
    }

    fn draw_character_with_color(&mut self, x: usize, y: usize, byte: u8, color: (u8, u8, u8)) {
        let glyph = get_glyph(byte);
        let (r, g, b) = color;
        for (row, row_data) in glyph.iter().copied().enumerate() {
            for col in 0..CHAR_WIDTH {
                if (row_data >> (7 - col)) & 1 == 1 {
                    self.write_pixel(x + col, y + row, r, g, b);
                }
            }
        }
    }

    fn draw_cursor_overlay(&mut self, x: usize, y: usize, byte: u8) {
        let (r, g, b) = self.fg;
        let glyph = get_glyph(byte);
        for (row, row_data) in glyph.iter().copied().enumerate() {
            for col in 0..CHAR_WIDTH {
                let foreground = (row_data >> (7 - col)) & 1 == 1;
                if foreground {
                    self.write_pixel(x + col, y + row, 0, 0, 0);
                } else {
                    self.write_pixel(x + col, y + row, r, g, b);
                }
            }
        }
        self.cursor_overlay = Some((x, y, byte));
    }

    fn remove_cursor_overlay(&mut self) {
        if let Some((x, y, byte)) = self.cursor_overlay.take() {
            self.clear_character(x, y);
            if byte != b' ' {
                self.draw_character_at(x, y, byte);
            }
        }
    }

    fn write_editor_row(&mut self, row: usize, contents: &[u8], columns: usize) {
        if row >= EDITOR_MAX_ROWS || columns > EDITOR_MAX_COLUMNS {
            return;
        }
        let y = MARGIN + row * LINE_HEIGHT;
        for column in 0..columns {
            let byte = contents.get(column).copied().unwrap_or(b' ');
            if self.editor_cells[row][column] == byte {
                continue;
            }
            let x = MARGIN + column * CHAR_WIDTH;
            self.clear_character(x, y);
            if byte != b' ' {
                self.draw_character_at(x, y, byte);
            }
            self.editor_cells[row][column] = byte;
        }
    }

    fn render_editor(&mut self, name: &str, text: &str, cursor: usize, dirty: bool) {
        if !self.editor_active {
            self.clear();
            self.editor_active = true;
        }
        self.remove_cursor_overlay();

        let columns = self
            .info
            .width
            .saturating_sub(2 * MARGIN)
            .checked_div(CHAR_WIDTH)
            .unwrap_or(0)
            .min(EDITOR_MAX_COLUMNS);
        let rows = self
            .info
            .height
            .saturating_sub(2 * MARGIN)
            .checked_div(LINE_HEIGHT)
            .unwrap_or(0)
            .min(EDITOR_MAX_ROWS);
        if columns == 0 || rows < 6 {
            return;
        }

        let cursor = cursor.min(text.len());
        let mut cursor_line: usize = 0;
        let mut cursor_column: usize = 0;
        for character in text[..cursor].chars() {
            if character == '\n' {
                cursor_line += 1;
                cursor_column = 0;
            } else {
                cursor_column += 1;
            }
        }
        let total_lines = text.chars().filter(|character| *character == '\n').count() + 1;
        let content_start = 3;
        let footer_start = rows - 2;
        let content_rows = footer_start - content_start;
        let first_line = cursor_line.saturating_sub(content_rows - 1);
        let horizontal_start = cursor_column.saturating_sub(columns - 1);

        let mut line = [b' '; EDITOR_MAX_COLUMNS];
        let mut length = 0;
        append_editor_text(&mut line, &mut length, "EDIT  /", columns);
        append_editor_text(&mut line, &mut length, name, columns);
        if dirty {
            append_editor_text(&mut line, &mut length, "  * modified", columns);
        }
        self.write_editor_row(0, &line, columns);

        line.fill(b' ');
        length = 0;
        append_editor_text(
            &mut line,
            &mut length,
            "Arrows move | Enter newline | Ctrl+S save | Ctrl+Q/Esc quit",
            columns,
        );
        self.write_editor_row(1, &line, columns);

        line.fill(b'-');
        self.write_editor_row(2, &line, columns);

        for (visible_row, file_line) in text
            .split('\n')
            .skip(first_line)
            .take(content_rows)
            .enumerate()
        {
            line.fill(b' ');
            for (column, character) in file_line
                .chars()
                .skip(horizontal_start)
                .take(columns)
                .enumerate()
            {
                line[column] = editor_display_byte(character);
            }
            self.write_editor_row(content_start + visible_row, &line, columns);
        }
        let rendered_content_rows = text.split('\n').skip(first_line).take(content_rows).count();
        for row in rendered_content_rows..content_rows {
            line.fill(b' ');
            self.write_editor_row(content_start + row, &line, columns);
        }

        line.fill(b' ');
        length = 0;
        append_editor_text(&mut line, &mut length, "Ln ", columns);
        append_editor_number(&mut line, &mut length, cursor_line + 1, columns);
        append_editor_text(&mut line, &mut length, "/", columns);
        append_editor_number(&mut line, &mut length, total_lines, columns);
        append_editor_text(&mut line, &mut length, "  Col ", columns);
        append_editor_number(&mut line, &mut length, cursor_column + 1, columns);
        append_editor_text(&mut line, &mut length, "  |  ", columns);
        append_editor_number(&mut line, &mut length, text.len(), columns);
        append_editor_text(&mut line, &mut length, " bytes", columns);
        self.write_editor_row(footer_start, &line, columns);

        line.fill(b' ');
        length = 0;
        append_editor_text(
            &mut line,
            &mut length,
            if dirty { "Unsaved changes" } else { "Saved" },
            columns,
        );
        self.write_editor_row(rows - 1, &line, columns);

        let cursor_row = content_start + cursor_line.saturating_sub(first_line);
        let cursor_column = cursor_column
            .saturating_sub(horizontal_start)
            .min(columns - 1);
        let x = MARGIN + cursor_column * CHAR_WIDTH;
        let y = MARGIN + cursor_row * LINE_HEIGHT;
        let byte = text[cursor..]
            .chars()
            .next()
            .filter(|character| *character != '\n')
            .map(editor_display_byte)
            .unwrap_or(b' ');
        self.x = x;
        self.y = y;
        self.draw_cursor_overlay(x, y, byte);
    }

    fn wrap_if_needed(&mut self) {
        if self.x + CHAR_WIDTH + MARGIN >= self.info.width {
            self.new_line();
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
        if let Some((_, origin_y)) = self.input_origin.as_mut() {
            *origin_y = origin_y.saturating_sub(rows);
        }
        if let Some((_, cursor_y, _)) = self.cursor_overlay.as_mut() {
            *cursor_y = cursor_y.saturating_sub(rows);
        }
        for (_, cell_y) in &mut self.input_cells[..self.input_cell_count] {
            *cell_y = cell_y.saturating_sub(rows);
        }
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
        self.current_line_id = self.current_line_id.wrapping_add(1);
        self.ensure_history_line(self.current_line_id);
        if self.y + LINE_HEIGHT + MARGIN + LINE_HEIGHT > self.info.height {
            self.scroll_up(LINE_HEIGHT);
        } else {
            self.y += LINE_HEIGHT;
        }
    }

    pub fn draw_ascii_char(&mut self, c: u8) {
        let column = self.x.saturating_sub(MARGIN) / CHAR_WIDTH;
        let color =
            (u32::from(self.fg.0) << 16) | (u32::from(self.fg.1) << 8) | u32::from(self.fg.2);
        self.set_history_cell(self.current_line_id, column, c, color);
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

    fn ensure_history_line(&mut self, id: u64) {
        while self.history.back().is_none_or(|line| line.id < id) {
            let next_id = self.history.back().map_or(id, |line| line.id + 1);
            self.history.push_back(TerminalLine::new(next_id));
            if self.history.len() > SCROLLBACK_LINE_CAPACITY {
                self.history.pop_front();
            }
        }
    }

    fn set_history_cell(&mut self, id: u64, column: usize, byte: u8, color: u32) {
        if column >= SCROLLBACK_COLUMN_CAPACITY {
            return;
        }
        if let Some(line) = self.history.iter_mut().find(|line| line.id == id) {
            line.bytes[column] = byte;
            line.colors[column] = color;
        } else if id >= self.history.back().map_or(0, |line| line.id) {
            self.ensure_history_line(id);
            if let Some(line) = self.history.back_mut().filter(|line| line.id == id) {
                line.bytes[column] = byte;
                line.colors[column] = color;
            }
        }
    }

    fn trim_history_after(&mut self, id: u64) {
        while self.history.back().is_some_and(|line| line.id > id) {
            self.history.pop_back();
        }
    }

    fn visible_rows(&self) -> usize {
        self.info
            .height
            .saturating_sub(2 * MARGIN)
            .checked_div(LINE_HEIGHT)
            .unwrap_or(0)
    }

    fn scrollback_page(&mut self, up: bool) {
        let oldest = self
            .history
            .front()
            .map_or(self.current_line_id, |line| line.id);
        let available = self.current_line_id.saturating_sub(oldest) as usize;
        let page = (self.visible_rows() / 2).max(1);
        if up {
            self.scrollback_offset = self.scrollback_offset.saturating_add(page).min(available);
        } else {
            self.scrollback_offset = self.scrollback_offset.saturating_sub(page);
        }
        self.render_history_view();
    }

    fn render_history_view(&mut self) {
        let total_size = self.info.stride * self.info.height;
        self.info.buffer.write_bytes(0, total_size, 0);
        let rows = self.visible_rows();
        if rows == 0 {
            return;
        }
        let viewing_history = self.scrollback_offset != 0;
        let content_rows = rows.saturating_sub(usize::from(viewing_history));
        let current_screen_row = self.y.saturating_sub(MARGIN) / LINE_HEIGHT;
        let bottom_row = if viewing_history {
            rows.saturating_sub(2)
        } else {
            current_screen_row.min(rows - 1)
        };
        let oldest = self
            .history
            .front()
            .map_or(self.current_line_id, |line| line.id);
        let end_id = self
            .current_line_id
            .saturating_sub(self.scrollback_offset as u64)
            .max(oldest);
        let visible_count = content_rows.min(bottom_row + 1);
        let first_id = end_id
            .saturating_sub(visible_count.saturating_sub(1) as u64)
            .max(oldest);
        for line_id in first_id..=end_id {
            let screen_row = bottom_row.saturating_sub((end_id - line_id) as usize);
            let Some(line) = self.history.iter().find(|line| line.id == line_id).copied() else {
                continue;
            };
            for column in 0..self.info.width.saturating_sub(2 * MARGIN) / CHAR_WIDTH {
                if column >= SCROLLBACK_COLUMN_CAPACITY {
                    break;
                }
                let byte = line.bytes[column];
                let color = line.colors[column];
                if byte != b' ' {
                    let color = (
                        ((color >> 16) & 0xff) as u8,
                        ((color >> 8) & 0xff) as u8,
                        (color & 0xff) as u8,
                    );
                    self.draw_character_with_color(
                        MARGIN + column * CHAR_WIDTH,
                        MARGIN + screen_row * LINE_HEIGHT,
                        byte,
                        color,
                    );
                }
            }
        }
        if viewing_history {
            let status = b"SCROLLBACK PgUp/PgDn | Esc: live";
            let y = MARGIN + (rows - 1) * LINE_HEIGHT;
            for (column, byte) in status.iter().copied().enumerate() {
                if MARGIN + column * CHAR_WIDTH + CHAR_WIDTH > self.info.width {
                    break;
                }
                self.draw_character_with_color(MARGIN + column * CHAR_WIDTH, y, byte, self.fg);
            }
        }
    }
}

impl fmt::Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            match byte {
                b'\n' => self.new_line(),
                b'\r' => {}
                byte => {
                    self.wrap_if_needed();
                    self.draw_ascii_char(byte);
                }
            }
        }
        if self.scrollback_offset != 0 {
            self.render_history_view();
        }
        Ok(())
    }
}

fn editor_display_byte(character: char) -> u8 {
    if character.is_ascii_graphic() || character == ' ' {
        character as u8
    } else {
        b'?'
    }
}

fn append_editor_text(
    line: &mut [u8; EDITOR_MAX_COLUMNS],
    length: &mut usize,
    text: &str,
    limit: usize,
) {
    for character in text.chars() {
        if *length >= limit {
            break;
        }
        line[*length] = editor_display_byte(character);
        *length += 1;
    }
}

fn append_editor_number(
    line: &mut [u8; EDITOR_MAX_COLUMNS],
    length: &mut usize,
    mut number: usize,
    limit: usize,
) {
    let mut digits = [0; 20];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (number % 10) as u8;
        count += 1;
        number /= 10;
        if number == 0 {
            break;
        }
    }
    while count > 0 && *length < limit {
        count -= 1;
        line[*length] = digits[count];
        *length += 1;
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

pub fn begin_input_line() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().begin_input_line();
        }
    });
}

pub fn finish_input_line() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().finish_input_line();
        }
    });
}

pub fn redraw_input_line(bytes: &[u8], cursor: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer
                .lock()
                .redraw_input_line(bytes, cursor.min(bytes.len()));
        }
    });
}

pub fn scrollback_page_up() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().scrollback_page(true);
        }
    });
}

pub fn scrollback_page_down() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().scrollback_page(false);
        }
    });
}

pub fn scrollback_to_bottom() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            let mut writer = writer.lock();
            if writer.scrollback_offset == 0 {
                return false;
            }
            writer.scrollback_offset = 0;
            writer.render_history_view();
            return true;
        }
        false
    })
}

pub fn is_scrollback_active() -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        WRITER
            .get()
            .is_some_and(|writer| writer.lock().scrollback_offset != 0)
    })
}

pub fn render_editor(name: &str, text: &str, cursor: usize, dirty: bool) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            writer.lock().render_editor(name, text, cursor, dirty);
        }
    });
}

pub fn finish_editor() {
    clear();
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

/// Render a byte-oriented console write without first treating user data as a
/// NUL-terminated string or constructing a UTF-8 string from it.
pub fn write_bytes(bytes: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.get() {
            let mut writer = writer.lock();
            for byte in bytes.iter().copied() {
                match byte {
                    b'\n' => writer.new_line(),
                    b'\r' => {}
                    byte => {
                        writer.wrap_if_needed();
                        writer.draw_ascii_char(byte);
                    }
                }
            }
            if writer.scrollback_offset != 0 {
                writer.render_history_view();
            }
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
    use core::fmt::Write;

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
    fn terminal_scrollback_is_bounded_and_pages_through_history() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            let mut writer = WRITER.get().unwrap().lock();
            for index in 0..300 {
                writeln!(writer, "row {}", index).unwrap();
            }

            assert_eq!(writer.history.len(), SCROLLBACK_LINE_CAPACITY);
            assert_eq!(
                writer.history.front().map(|line| line.id),
                Some(writer.current_line_id - (SCROLLBACK_LINE_CAPACITY as u64 - 1))
            );
            writer.scrollback_page(true);
            assert_eq!(writer.scrollback_offset, (writer.visible_rows() / 2).max(1));
            writer.scrollback_page(false);
            assert_eq!(writer.scrollback_offset, 0);
        });
    }

    #[test_case]
    fn input_redraw_updates_scrollback_text() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            begin_input_line();
            redraw_input_line(b"abc", 3);
            redraw_input_line(b"ab", 2);

            let writer = WRITER.get().unwrap().lock();
            let current = writer.history.back().unwrap();
            assert_eq!(&current.bytes[..3], b"ab ");
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

    #[test_case]
    fn redraw_input_line_replaces_old_text_and_tracks_cursor() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            begin_input_line();
            redraw_input_line(b"abcd", 2);
            {
                let writer = WRITER.get().unwrap().lock();
                assert_eq!(writer.x, MARGIN + 2 * CHAR_WIDTH);
            }

            redraw_input_line(b"abXcd", 3);
            redraw_input_line(b"abd", 2);
            let writer = WRITER.get().unwrap().lock();
            assert_eq!(writer.x, MARGIN + 2 * CHAR_WIDTH);
            for y in MARGIN..MARGIN + 8 {
                for x in MARGIN + 3 * CHAR_WIDTH..MARGIN + 4 * CHAR_WIDTH {
                    let offset = y * writer.info.stride + x * writer.info.bytes_per_pixel;
                    for component in 0..3 {
                        let pixel = unsafe {
                            writer
                                .info
                                .buffer
                                .ptr
                                .as_ptr()
                                .add(offset + component)
                                .read_volatile()
                        };
                        assert_eq!(pixel, 0);
                    }
                }
            }
        });
    }

    #[test_case]
    fn finishing_input_line_removes_cursor_without_erasing_text() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            begin_input_line();
            redraw_input_line(b"abcH", 3);
            finish_input_line();

            let writer = WRITER.get().unwrap().lock();
            assert_eq!(writer.cursor_overlay, None);
            assert_eq!(writer.input_cell_count, 0);
            let x = MARGIN + 3 * CHAR_WIDTH;
            let y = MARGIN;
            let offset = y * writer.info.stride + x * writer.info.bytes_per_pixel;
            let pixel = unsafe { writer.info.buffer.ptr.as_ptr().add(offset).read_volatile() };
            assert_eq!(pixel, DEFAULT_FG.0);
        });
    }

    #[test_case]
    fn editor_scrolls_the_document_and_tracks_the_visible_cursor() {
        init_test_framebuffer();
        x86_64::instructions::interrupts::without_interrupts(|| {
            reset_writer();
            let mut bytes = [0; 80];
            for line in bytes.chunks_exact_mut(2) {
                line.copy_from_slice(b"x\n");
            }
            let text = core::str::from_utf8(&bytes).unwrap();

            render_editor("LONG.TXT", text, text.len(), true);
            let writer = WRITER.get().unwrap().lock();
            let content_rows = 24;
            let visible_cursor_row = 3 + content_rows - 1;
            assert_eq!(writer.editor_cells[3][0], b'x');
            assert!(writer.editor_cells[27].starts_with(b"Ln 41/41"));
            assert_eq!(
                writer.cursor_overlay.map(|(_, y, _)| y),
                Some(MARGIN + visible_cursor_row * LINE_HEIGHT)
            );
        });
    }
}
