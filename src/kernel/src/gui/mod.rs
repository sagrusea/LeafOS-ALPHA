use crate::framebuffer;
use crate::sync::{Mutex, Once};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

const MAX_WINDOWS: usize = 8;
const EVENT_CAPACITY: usize = 64;
const MAX_WINDOW_RECTS: usize = 32;
const MAX_WINDOW_TEXT: usize = 16;
const MAX_TEXT_BYTES: usize = 64;
const TERMINAL_ROWS: usize = 256;
const TERMINAL_COLUMNS: usize = 80;
const TERMINAL_CELL_WIDTH: usize = 9;
const KEY_BUFFER_CAPACITY: usize = 256;
const DEFAULT_TERMINAL_TEXT: (u8, u8, u8) = (220, 235, 245);
const MIN_WINDOW_WIDTH: usize = 200;
const MIN_WINDOW_HEIGHT: usize = 150;
const RESIZE_GRIP_SIZE: usize = 12;
const TERMINAL_LINE_STRIDE: usize = 20;
const DOCK_WIDTH: usize = 400;
const DOCK_HEIGHT: usize = 48;
const DOCK_BOTTOM_OFFSET: usize = 64;
const DOCK_BUTTON_MARGIN: usize = 16;
const DOCK_BUTTON_GAP: usize = 8;

const DESKTOP: (u8, u8, u8) = (18, 30, 48);
const PANEL: (u8, u8, u8) = (28, 44, 68);
const CARD: (u8, u8, u8) = (38, 58, 86);
const DOCK: (u8, u8, u8) = (64, 72, 82);
const DOCK_BUTTON: (u8, u8, u8) = (82, 92, 104);
const ACCENT: (u8, u8, u8) = (80, 190, 160);
const TEXT: (u8, u8, u8) = (235, 245, 255);
const MUTED: (u8, u8, u8) = (160, 180, 200);
const STATUS_BAR_HEIGHT: usize = 32;

static LAST_TOP_BAR_SECOND: AtomicU64 = AtomicU64::new(u64::MAX);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockAction {
    Terminal,
    Edit,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiEvent {
    Key(u8),
    Mouse {
        x: i32,
        y: i32,
        buttons: u8,
        wheel_delta: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub id: u32,
    pub owner: u64,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub title: [u8; 32],
    pub title_length: usize,
    pub visible: bool,
    rects: [WindowRect; MAX_WINDOW_RECTS],
    rect_count: usize,
    texts: [WindowText; MAX_WINDOW_TEXT],
    text_count: usize,
    terminal: bool,
    terminal_buffer: TerminalBuffer,
    key_buffer: TerminalKeyBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalKeyBuffer {
    bytes: [u8; KEY_BUFFER_CAPACITY],
    head: usize,
    length: usize,
}

impl TerminalKeyBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; KEY_BUFFER_CAPACITY],
            head: 0,
            length: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        if self.length == KEY_BUFFER_CAPACITY {
            return;
        }
        let tail = (self.head + self.length) % KEY_BUFFER_CAPACITY;
        self.bytes[tail] = byte;
        self.length += 1;
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > KEY_BUFFER_CAPACITY.saturating_sub(self.length) {
            return false;
        }
        for byte in bytes.iter().copied() {
            self.push(byte);
        }
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.length == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % KEY_BUFFER_CAPACITY;
        self.length -= 1;
        Some(byte)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalLine {
    bytes: [u8; TERMINAL_COLUMNS],
    color: u32,
    length: usize,
}

impl TerminalLine {
    const EMPTY: Self = Self {
        bytes: [0; TERMINAL_COLUMNS],
        color: 0,
        length: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalBuffer {
    lines: [TerminalLine; TERMINAL_ROWS],
    first_line: usize,
    line_count: usize,
    cursor_column: usize,
    scrollback_offset: usize,
    input_anchor: Option<(usize, usize)>,
    input_cursor: Option<(usize, usize)>,
    pending_rows: Option<(usize, usize)>,
    pending_full_redraw: bool,
    foreground: (u8, u8, u8),
}

impl TerminalBuffer {
    const fn new() -> Self {
        Self {
            lines: [TerminalLine::EMPTY; TERMINAL_ROWS],
            first_line: 0,
            line_count: 1,
            cursor_column: 0,
            scrollback_offset: 0,
            input_anchor: None,
            input_cursor: None,
            pending_rows: None,
            pending_full_redraw: false,
            foreground: DEFAULT_TERMINAL_TEXT,
        }
    }

    fn set_foreground(&mut self, color: (u8, u8, u8)) {
        self.foreground = color;
    }

    fn append(&mut self, bytes: &[u8]) -> (usize, usize, bool) {
        let start_row = self.line_count - 1;
        let first_line = self.first_line;
        if !bytes.is_empty() {
            self.scrollback_offset = 0;
        }
        for byte in bytes.iter().copied() {
            match byte {
                b'\n' => self.new_line(),
                b'\r' => {
                    let line = self.current_line_mut();
                    line.bytes = [0; TERMINAL_COLUMNS];
                    line.color = 0;
                    line.length = 0;
                    self.cursor_column = 0;
                }
                b'\t' => {
                    for _ in 0..4 {
                        self.append_byte(b' ');
                    }
                }
                byte if byte >= b' ' && byte != 0x7f => self.append_byte(byte),
                _ => {}
            }
        }
        (start_row, self.line_count - 1, self.first_line != first_line)
    }

    fn append_byte(&mut self, byte: u8) {
        if self.cursor_column == TERMINAL_COLUMNS {
            self.new_line();
        }
        let column = self.cursor_column;
        let color = (u32::from(self.foreground.0) << 16)
            | (u32::from(self.foreground.1) << 8)
            | u32::from(self.foreground.2);
        let line = self.current_line_mut();
        if line.length == 0 {
            line.color = color;
        }
        line.bytes[column] = byte;
        line.length = line.length.max(column + 1);
        self.cursor_column += 1;
    }

    fn new_line(&mut self) {
        let next_line = if self.line_count < TERMINAL_ROWS {
            let next_line = (self.first_line + self.line_count) % TERMINAL_ROWS;
            self.line_count += 1;
            next_line
        } else {
            self.first_line = (self.first_line + 1) % TERMINAL_ROWS;
            (self.first_line + self.line_count - 1) % TERMINAL_ROWS
        };
        self.lines[next_line] = TerminalLine::EMPTY;
        self.cursor_column = 0;
        self.scrollback_offset = 0;
    }

    fn current_line_mut(&mut self) -> &mut TerminalLine {
        let index = (self.first_line + self.line_count - 1) % TERMINAL_ROWS;
        &mut self.lines[index]
    }

    fn begin_input(&mut self) {
        let line = (self.first_line + self.line_count - 1) % TERMINAL_ROWS;
        self.input_anchor = Some((line, self.cursor_column));
        self.input_cursor = None;
    }

    fn redraw_input(&mut self, bytes: &[u8], cursor: usize) {
        let Some((anchor_line, anchor_column)) = self.input_anchor else {
            return;
        };
        let anchor_row = (anchor_line + TERMINAL_ROWS - self.first_line) % TERMINAL_ROWS;
        if anchor_row >= self.line_count {
            return;
        }

        let anchor = &mut self.lines[anchor_line];
        anchor.bytes[anchor_column..].fill(0);
        if anchor_column == 0 {
            anchor.color = 0;
        }
        anchor.length = anchor_column;
        for row in anchor_row + 1..self.line_count {
            let line_index = (self.first_line + row) % TERMINAL_ROWS;
            self.lines[line_index] = TerminalLine::EMPTY;
        }
        self.line_count = anchor_row + 1;
        self.cursor_column = anchor_column;
        self.input_cursor = None;

        for (index, byte) in bytes.iter().copied().enumerate() {
            if index == cursor {
                self.mark_input_cursor();
            }
            self.append(&[byte]);
        }
        if cursor >= bytes.len() {
            self.mark_input_cursor();
        }
    }

    fn mark_input_cursor(&mut self) {
        if self.cursor_column == TERMINAL_COLUMNS {
            self.new_line();
        }
        let line = (self.first_line + self.line_count - 1) % TERMINAL_ROWS;
        self.input_cursor = Some((line, self.cursor_column));
    }

    fn record_pending_damage(&mut self, rows: (usize, usize), full_redraw: bool) {
        if full_redraw {
            self.pending_full_redraw = true;
            self.pending_rows = None;
        } else if !self.pending_full_redraw {
            self.pending_rows = Some(match self.pending_rows {
                Some((start, end)) => (start.min(rows.0), end.max(rows.1)),
                None => rows,
            });
        }
    }

    fn take_pending_damage(&mut self) -> Option<Option<(usize, usize)>> {
        if !self.pending_full_redraw && self.pending_rows.is_none() {
            return None;
        }
        let rows = if self.pending_full_redraw {
            None
        } else {
            self.pending_rows
        };
        self.pending_full_redraw = false;
        self.pending_rows = None;
        Some(rows)
    }

    fn scroll(&mut self, delta: i32, visible_rows: usize) -> bool {
        let max_offset = self.line_count.saturating_sub(visible_rows);
        let new_offset = (self.scrollback_offset as i64 + i64::from(delta))
            .clamp(0, max_offset as i64) as usize;
        let changed = self.scrollback_offset != new_offset;
        self.scrollback_offset = new_offset;
        changed
    }

    #[cfg(test)]
    fn line(&self, row: usize) -> &[u8] {
        let index = (self.first_line + row) % TERMINAL_ROWS;
        &self.lines[index].bytes[..self.lines[index].length]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowRect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: (u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowText {
    x: usize,
    y: usize,
    bytes: [u8; MAX_TEXT_BYTES],
    length: usize,
    color: (u8, u8, u8),
}

impl WindowText {
    const EMPTY: Self = Self {
        x: 0,
        y: 0,
        bytes: [0; MAX_TEXT_BYTES],
        length: 0,
        color: (0, 0, 0),
    };
}

#[derive(Debug, Clone, Copy)]
struct WindowSnapshot {
    id: u32,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    title: [u8; 32],
    title_length: usize,
    rect_count: usize,
}

impl WindowRect {
    const EMPTY: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        color: (0, 0, 0),
    };
}

impl Window {
    const fn empty() -> Self {
        Self {
            id: 0,
            owner: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            title: [0; 32],
            title_length: 0,
            visible: false,
            rects: [WindowRect::EMPTY; MAX_WINDOW_RECTS],
            rect_count: 0,
            texts: [WindowText::EMPTY; MAX_WINDOW_TEXT],
            text_count: 0,
            terminal: false,
            terminal_buffer: TerminalBuffer::new(),
            key_buffer: TerminalKeyBuffer::new(),
        }
    }

    fn add_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: (u8, u8, u8)) -> bool {
        if width == 0
            || height == 0
            || x.saturating_add(width) > self.width
            || y.saturating_add(height) > self.height
            || self.rect_count == MAX_WINDOW_RECTS
        {
            return false;
        }
        self.rects[self.rect_count] = WindowRect {
            x,
            y,
            width,
            height,
            color,
        };
        self.rect_count += 1;
        true
    }

    fn add_text(&mut self, x: usize, y: usize, text: &[u8], color: (u8, u8, u8)) -> bool {
        let pixel_width = text.len().saturating_mul(TERMINAL_CELL_WIDTH);
        if text.is_empty()
            || text.len() > MAX_TEXT_BYTES
            || x.saturating_add(pixel_width) > self.width
            || y.saturating_add(8) > self.height
            || self.text_count == MAX_WINDOW_TEXT
        {
            return false;
        }
        let command = &mut self.texts[self.text_count];
        command.x = x;
        command.y = y;
        command.bytes[..text.len()].copy_from_slice(text);
        command.length = text.len();
        command.color = color;
        self.text_count += 1;
        true
    }
}

struct GuiState {
    active: bool,
    next_window_id: u32,
    windows: [Window; MAX_WINDOWS],
    z_order: [usize; MAX_WINDOWS],
    focused: Option<u32>,
    cursor: (i32, i32),
    mouse_buttons: u8,
    mouse_inverse: bool,
    dragging: Option<(u32, i32, i32)>,
    resizing: Option<(u32, i32, i32, usize, usize)>,
    drag_surface: Option<DragSurface>,
    terminal_batch_owner: Option<u64>,
    terminal_batch_depth: usize,
    events: [Option<(u64, GuiEvent)>; EVENT_CAPACITY],
    event_head: usize,
    event_length: usize,
}

struct DragSurface {
    window_id: u32,
    width: usize,
    height: usize,
    pixels: Vec<u8>,
    background_width: usize,
    background_height: usize,
    background: Vec<u8>,
}

impl GuiState {
    const fn new() -> Self {
        Self {
            active: false,
            next_window_id: 1,
            windows: [Window::empty(); MAX_WINDOWS],
            z_order: [0, 1, 2, 3, 4, 5, 6, 7],
            focused: None,
            cursor: (640, 400),
            mouse_buttons: 0,
            mouse_inverse: false,
            dragging: None,
            resizing: None,
            drag_surface: None,
            terminal_batch_owner: None,
            terminal_batch_depth: 0,
            events: [None; EVENT_CAPACITY],
            event_head: 0,
            event_length: 0,
        }
    }

    fn raise_window(&mut self, slot: usize) {
        let Some(position) = self.z_order.iter().position(|&entry| entry == slot) else {
            return;
        };
        for index in position..MAX_WINDOWS - 1 {
            self.z_order[index] = self.z_order[index + 1];
        }
        self.z_order[MAX_WINDOWS - 1] = slot;
    }

    fn toggle_mouse_inverse(&mut self) -> bool {
        self.mouse_inverse = !self.mouse_inverse;
        self.mouse_inverse
    }

    fn scroll_delta(&self, delta: i32) -> i32 {
        if self.mouse_inverse {
            delta.saturating_neg()
        } else {
            delta
        }
    }

    fn push_event(&mut self, owner: u64, event: GuiEvent) {
        if self.event_length == EVENT_CAPACITY {
            return;
        }
        let index = (self.event_head + self.event_length) % EVENT_CAPACITY;
        self.events[index] = Some((owner, event));
        self.event_length += 1;
    }

    fn focused_window_slot(&self) -> Option<usize> {
        let id = self.focused?;
        self.windows
            .iter()
            .position(|window| window.visible && window.id == id)
    }

    fn push_focused_key(&mut self, byte: u8) -> bool {
        if !self.active {
            return false;
        }
        let Some(slot) = self.focused_window_slot() else {
            return true;
        };
        let owner = self.windows[slot].owner;
        if self.windows[slot].terminal {
            self.windows[slot].key_buffer.push(byte);
        }
        self.push_event(owner, GuiEvent::Key(byte));
        true
    }

    fn pop_focused_terminal_key(&mut self, owner: u64) -> Option<u8> {
        if !self.active
            || !self.focused_window_slot().is_some_and(|slot| {
                let window = &self.windows[slot];
                window.terminal && window.owner == owner
            })
        {
            return None;
        }
        self.windows
            .iter_mut()
            .find(|window| window.terminal && window.visible && window.owner == owner)?
            .key_buffer
            .pop()
    }

    fn pop_event(&mut self, owner: u64) -> Option<GuiEvent> {
        if self.event_length == 0 {
            return None;
        }
        for offset in 0..self.event_length {
            let index = (self.event_head + offset) % EVENT_CAPACITY;
            if self.events[index].is_some_and(|(event_owner, _)| event_owner == owner) {
                let event = self.events[index].take().map(|(_, event)| event);
                if offset == 0 {
                    self.event_head = (self.event_head + 1) % EVENT_CAPACITY;
                } else {
                    let mut current = index;
                    while current != self.event_head {
                        let previous = (current + EVENT_CAPACITY - 1) % EVENT_CAPACITY;
                        self.events[current] = self.events[previous].take();
                        current = previous;
                    }
                    self.event_head = (self.event_head + 1) % EVENT_CAPACITY;
                }
                self.event_length -= 1;
                return event;
            }
        }
        None
    }

    fn remove_owner_events(&mut self, owner: u64) {
        let mut retained = [None; EVENT_CAPACITY];
        let mut retained_length = 0;
        for offset in 0..self.event_length {
            let index = (self.event_head + offset) % EVENT_CAPACITY;
            if let Some(event) = self.events[index]
                && event.0 != owner
            {
                retained[retained_length] = Some(event);
                retained_length += 1;
            }
        }
        self.events = retained;
        self.event_head = 0;
        self.event_length = retained_length;
    }
}

static GUI: Once<Mutex<GuiState>> = Once::new();
static CANVAS_LOCK: Mutex<()> = Mutex::new(());

fn state() -> &'static Mutex<GuiState> {
    GUI.call_once(|| Mutex::new(GuiState::new()))
}

fn focused_title(gui: &GuiState) -> ([u8; 32], usize) {
    let mut title = [0; 32];
    let title_length = gui
        .focused_window_slot()
        .map(|slot| {
            let window = &gui.windows[slot];
            title[..window.title_length].copy_from_slice(&window.title[..window.title_length]);
            window.title_length
        })
        .unwrap_or_else(|| {
            title[..7].copy_from_slice(b"Desktop");
            7
        });
    (title, title_length)
}

pub fn initialize() {
    let _ = state();
}

pub fn enable() {
    state().lock().active = true;
}

pub fn is_active() -> bool {
    state().lock().active
}

pub fn screen_dimensions() -> (usize, usize) {
    framebuffer::dimensions().unwrap_or((1280, 800))
}

pub fn leave() {
    let cursor = {
        let mut gui = state().lock();
        if !gui.active {
            return;
        }
        gui.active = false;
        gui.event_head = 0;
        gui.event_length = 0;
        gui.mouse_buttons = 0;
        gui.dragging = None;
        gui.resizing = None;
        gui.cursor
    };
    toggle_cursor(cursor);
}

pub fn push_key(byte: u8) -> bool {
    state().lock().push_focused_key(byte)
}

fn bottom_dock_bounds(screen_width: usize, screen_height: usize) -> (usize, usize, usize, usize) {
    let width = DOCK_WIDTH.min(screen_width);
    let height = DOCK_HEIGHT.min(screen_height);
    let x = screen_width.saturating_sub(width) / 2;
    let y = screen_height.saturating_sub(DOCK_BOTTOM_OFFSET);
    (x, y, width, height)
}

fn dock_button_bounds(
    index: usize,
    dock: (usize, usize, usize, usize),
) -> (usize, usize, usize, usize) {
    let button_width = dock
        .2
        .saturating_sub(DOCK_BUTTON_MARGIN * 2 + DOCK_BUTTON_GAP * 2)
        / 3;
    let button_height = 32.min(dock.3);
    let x = dock.0 + DOCK_BUTTON_MARGIN + index * (button_width + DOCK_BUTTON_GAP);
    let y = dock.1 + dock.3.saturating_sub(button_height) / 2;
    (x, y, button_width, button_height)
}

fn point_in_rect(x: i32, y: i32, rect: (usize, usize, usize, usize)) -> bool {
    x >= 0
        && y >= 0
        && (x as usize) >= rect.0
        && (y as usize) >= rect.1
        && (x as usize) < rect.0.saturating_add(rect.2)
        && (y as usize) < rect.1.saturating_add(rect.3)
}

fn dock_action_at(x: i32, y: i32, screen_width: usize, screen_height: usize) -> Option<DockAction> {
    let dock = bottom_dock_bounds(screen_width, screen_height);
    [DockAction::Terminal, DockAction::Edit, DockAction::Info]
        .into_iter()
        .enumerate()
        .find_map(|(index, action)| {
            point_in_rect(x, y, dock_button_bounds(index, dock)).then_some(action)
        })
}

pub fn push_mouse(x: i32, y: i32, buttons: u8, wheel_delta: i32) {
    let (old_cursor, focus_changed, geometry_damage, drag_window_id, effective_wheel_delta);
    let mut scroll_owner = None;
    let mut close_terminal_owner = None;
    let mut launch_terminal = false;
    let mut dock_command_result = None;
    let mut dock_command_missing_terminal = false;
    {
        let mut gui = state().lock();
        if !gui.active {
            return;
        }
        old_cursor = gui.cursor;
        let old_focus = gui.focused;
        let old_buttons = gui.mouse_buttons;
        let screen = framebuffer::dimensions().unwrap_or((1280, 800));
        let dock = bottom_dock_bounds(screen.0, screen.1);
        let pointer_in_top_bar = (0..STATUS_BAR_HEIGHT as i32).contains(&y);
        let pointer_in_dock = point_in_rect(x, y, dock);
        let system_layer_pointer = pointer_in_top_bar || pointer_in_dock;
        let click_started = buttons & 1 != 0 && old_buttons & 1 == 0;
        let dock_action = if click_started && pointer_in_dock {
            dock_action_at(x, y, screen.0, screen.1)
        } else {
            None
        };
        gui.cursor = (x, y);
        gui.mouse_buttons = buttons;
        effective_wheel_delta = gui.scroll_delta(wheel_delta);
        let mut damage = None;
        if click_started && system_layer_pointer {
            gui.dragging = None;
            gui.resizing = None;
            gui.drag_surface = None;
            match dock_action {
                Some(DockAction::Terminal) => launch_terminal = true,
                Some(action @ (DockAction::Edit | DockAction::Info)) => {
                    let terminal_slot = gui
                        .focused_window_slot()
                        .filter(|&slot| gui.windows[slot].terminal)
                        .or_else(|| {
                            gui.z_order.iter().rev().copied().find(|&slot| {
                                let window = &gui.windows[slot];
                                window.visible && window.terminal
                            })
                        });
                    if let Some(slot) = terminal_slot {
                        let command: &[u8] = match action {
                            DockAction::Edit => b"edit ",
                            DockAction::Info => b"info\n",
                            DockAction::Terminal => unreachable!(),
                        };
                        let id = gui.windows[slot].id;
                        let owner = gui.windows[slot].owner;
                        gui.focused = Some(id);
                        gui.raise_window(slot);
                        let queued = gui.windows[slot].key_buffer.push_bytes(command);
                        dock_command_result = Some((owner, queued));
                    } else {
                        dock_command_missing_terminal = true;
                    }
                }
                None => {}
            }
        } else if buttons & 1 == 0 {
            gui.dragging = None;
            gui.resizing = None;
            gui.drag_surface = None;
        } else if let Some((id, start_x, start_y, start_width, start_height)) = gui.resizing {
            if let Some(window) = gui
                .windows
                .iter_mut()
                .find(|window| window.visible && window.id == id)
            {
                let max_width = screen.0.saturating_sub(window.x).max(1);
                let dock_top = bottom_dock_bounds(screen.0, screen.1).1;
                let max_height = dock_top.saturating_sub(window.y).max(1);
                let min_width = MIN_WINDOW_WIDTH.min(max_width);
                let min_height = MIN_WINDOW_HEIGHT.min(max_height);
                let width = (start_width as i64 + i64::from(x - start_x))
                    .clamp(min_width as i64, max_width as i64) as usize;
                let height = (start_height as i64 + i64::from(y - start_y))
                    .clamp(min_height as i64, max_height as i64)
                    as usize;
                if window.width != width || window.height != height {
                    let old_geometry = (window.x, window.y, window.width, window.height);
                    window.width = width;
                    window.height = height;
                    damage = Some((old_geometry, (window.x, window.y, width, height)));
                }
            } else {
                gui.resizing = None;
            }
        } else if let Some((id, offset_x, offset_y)) = gui.dragging {
            if let Some(window) = gui
                .windows
                .iter_mut()
                .find(|window| window.visible && window.id == id)
            {
                let max_x = screen.0.saturating_sub(window.width);
                let dock_top = bottom_dock_bounds(screen.0, screen.1).1;
                let max_y = dock_top
                    .saturating_sub(window.height)
                    .max(STATUS_BAR_HEIGHT);
                let new_x =
                    x.saturating_sub(offset_x)
                        .clamp(0, max_x.min(i32::MAX as usize) as i32) as usize;
                let new_y = y.saturating_sub(offset_y).clamp(
                    STATUS_BAR_HEIGHT as i32,
                    max_y.min(i32::MAX as usize) as i32,
                ) as usize;
                if window.x != new_x || window.y != new_y {
                    let old_geometry = (window.x, window.y, window.width, window.height);
                    window.x = new_x;
                    window.y = new_y;
                    damage = Some((old_geometry, (new_x, new_y, window.width, window.height)));
                }
            } else {
                gui.dragging = None;
            }
        }

        let hit_slot = (!system_layer_pointer)
            .then(|| {
                gui.z_order.iter().rev().copied().find(|&slot| {
                    let window = &gui.windows[slot];
                    window.visible
                        && x >= window.x as i32
                        && y >= window.y as i32
                        && x < window.x.saturating_add(window.width) as i32
                        && y < window.y.saturating_add(window.height) as i32
                })
            })
            .flatten();
        if effective_wheel_delta != 0 {
            if let Some(slot) = hit_slot {
                let window = &mut gui.windows[slot];
                if window.terminal {
                    let rows = window
                        .height
                        .saturating_sub(54)
                        .checked_div(TERMINAL_LINE_STRIDE)
                        .unwrap_or(0);
                    let visible_rows = rows.min(TERMINAL_ROWS);
                    if window
                        .terminal_buffer
                        .scroll(effective_wheel_delta, visible_rows)
                    {
                        scroll_owner = Some(window.owner);
                    }
                }
            }
        }

        if click_started && !system_layer_pointer {
            let hit = hit_slot.map(|slot| gui.windows[slot].id);
            let close_id = hit.and_then(|id| {
                gui.windows
                    .iter()
                    .find(|window| {
                        window.id == id
                            && window.width >= 24
                            && x >= (window.x + window.width - 22) as i32
                            && x < (window.x + window.width - 6) as i32
                            && y >= (window.y + 8) as i32
                            && y < (window.y + 24) as i32
                    })
                    .map(|window| window.id)
            });
            if let Some(id) = close_id {
                let owner = gui
                    .windows
                    .iter()
                    .find(|window| window.id == id)
                    .map(|window| window.owner)
                    .unwrap_or(0);
                close_terminal_owner = gui
                    .windows
                    .iter()
                    .find(|window| window.id == id && window.terminal)
                    .map(|window| window.owner);
                if let Some(window) = gui.windows.iter_mut().find(|window| window.id == id) {
                    window.visible = false;
                }
                if gui.focused == Some(id) {
                    gui.focused = None;
                }
                gui.remove_owner_events(owner);
                gui.dragging = None;
                gui.resizing = None;
            } else {
                gui.focused = hit;
                if let Some(slot) = hit_slot {
                    gui.raise_window(slot);
                    let window = gui.windows[slot];
                    if x >= window
                        .x
                        .saturating_add(window.width)
                        .saturating_sub(RESIZE_GRIP_SIZE) as i32
                        && y >= window
                            .y
                            .saturating_add(window.height)
                            .saturating_sub(RESIZE_GRIP_SIZE) as i32
                    {
                        gui.drag_surface = None;
                        gui.resizing = Some((window.id, x, y, window.width, window.height));
                        gui.dragging = None;
                    } else if y >= window.y as i32 && y < window.y.saturating_add(36) as i32 {
                        gui.drag_surface = None;
                        gui.dragging = Some((window.id, x - window.x as i32, y - window.y as i32));
                        gui.resizing = None;
                    } else {
                        gui.dragging = None;
                        gui.resizing = None;
                    }
                } else {
                    gui.dragging = None;
                    gui.resizing = None;
                }
            }
        }
        focus_changed = gui.focused != old_focus;
        geometry_damage = damage;
        let owner = gui
            .focused
            .and_then(|id| gui.windows.iter().find(|window| window.id == id))
            .map(|window| window.owner)
            .unwrap_or(0);
        if !system_layer_pointer {
            gui.push_event(
                owner,
                GuiEvent::Mouse {
                    x,
                    y,
                    buttons,
                    wheel_delta: effective_wheel_delta,
                },
            );
        }
        drag_window_id = gui.dragging.map(|(id, _, _)| id);
    }
    if focus_changed {
        let _canvas = CANVAS_LOCK.lock();
        render_desktop_locked();
    } else if let Some((old_geometry, new_geometry)) = geometry_damage {
        let _canvas = CANVAS_LOCK.lock();
        toggle_cursor_locked(old_cursor);
        let damage = (
            old_geometry.0.min(new_geometry.0),
            old_geometry.1.min(new_geometry.1),
            old_geometry
                .0
                .saturating_add(old_geometry.2)
                .max(new_geometry.0.saturating_add(new_geometry.2)),
            old_geometry
                .1
                .saturating_add(old_geometry.3)
                .max(new_geometry.1.saturating_add(new_geometry.3)),
        );
        framebuffer::begin_frame();
        if let Some(window_id) = drag_window_id {
            let needs_snapshot = state()
                .lock()
                .drag_surface
                .as_ref()
                .is_none_or(|surface| surface.window_id != window_id);
            if needs_snapshot {
                if let Some((width, height, pixels)) = framebuffer::snapshot_rect(
                    old_geometry.0,
                    old_geometry.1,
                    old_geometry.2,
                    old_geometry.3,
                ) {
                    if let Some((screen_width, screen_height)) = framebuffer::dimensions() {
                        render_desktop_contents(Some(window_id), false);
                        if let Some((background_width, background_height, background)) =
                            framebuffer::snapshot_rect(0, 0, screen_width, screen_height)
                        {
                            state().lock().drag_surface = Some(DragSurface {
                                window_id,
                                width,
                                height,
                                pixels,
                                background_width,
                                background_height,
                                background,
                            });
                        } else {
                            render_desktop_contents(None, false);
                        }
                    }
                }
            }
        }
        render_drag_damage_locked(old_geometry, new_geometry, drag_window_id);
        framebuffer::end_frame_with_rect(
            damage.0,
            damage.1,
            damage.2.saturating_sub(damage.0),
            damage.3.saturating_sub(damage.1),
        );
        toggle_cursor_locked((x, y));
    } else if let Some(owner) = scroll_owner {
        let _canvas = CANVAS_LOCK.lock();
        toggle_cursor_locked(old_cursor);
        render_terminal_rows_locked(owner, None);
        toggle_cursor_locked((x, y));
    } else {
        let _canvas = CANVAS_LOCK.lock();
        toggle_cursor_locked(old_cursor);
        toggle_cursor_locked((x, y));
    }
    if dock_command_missing_terminal {
        crate::serial_println!("[WARN] Dock shortcut requires an open terminal");
    }
    if let Some((owner, queued)) = dock_command_result
        && !queued
    {
        crate::serial_println!(
            "[WARN] Could not queue dock shortcut in terminal task {}: input buffer is full",
            owner
        );
    }
    if let Some(owner) = close_terminal_owner
        && let Err(error) = crate::scheduler::request_terminate_task(
            crate::scheduler::TaskId::from_raw(owner),
            0,
        )
    {
        crate::serial_println!(
            "[WARN] Could not stop closed terminal task {}: {:?}",
            owner,
            error
        );
    }
    if launch_terminal {
        match crate::scheduler::spawn_detached(crate::shell::run_service_task) {
            Ok(task) => {
                crate::serial_println!(
                    "[INFO] Started terminal shell task {} from the dock",
                    task.get()
                );
            }
            Err(error) => {
                crate::serial_println!(
                    "[WARN] Could not start a terminal shell from the dock: {:?}",
                    error
                );
            }
        }
        render_desktop();
    }
}

pub fn toggle_mouse_inverse() -> bool {
    state().lock().toggle_mouse_inverse()
}

pub fn poll_event(owner: u64) -> Option<GuiEvent> {
    state().lock().pop_event(owner)
}

pub fn create_window(
    owner: u64,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    title: &[u8],
) -> Option<u32> {
    let mut gui = state().lock();
    let slot = gui.windows.iter().position(|window| !window.visible)?;
    let mut window = Window::empty();
    window.id = gui.next_window_id;
    window.owner = owner;
    gui.next_window_id = gui.next_window_id.wrapping_add(1).max(1);
    window.x = x;
    window.y = y;
    window.width = width;
    window.height = height;
    window.title_length = title.len().min(window.title.len());
    window.title[..window.title_length].copy_from_slice(&title[..window.title_length]);
    window.visible = true;
    gui.windows[slot] = window;
    gui.raise_window(slot);
    Some(window.id)
}

pub fn open_window(
    owner: u64,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    title: &[u8],
) -> Option<u32> {
    enable();
    let id = create_window(owner, x, y, width, height, title)?;
    render_desktop();
    Some(id)
}

pub fn open_terminal_window(owner: u64) -> Option<u32> {
    enable();
    let (screen_width, screen_height) = screen_dimensions();
    let width = 680.min(screen_width.saturating_sub(16)).max(1);
    let dock_top = bottom_dock_bounds(screen_width, screen_height).1;
    let y = 54.min(dock_top.saturating_sub(1));
    let height = 286.min(dock_top.saturating_sub(y)).max(1);
    let x = 32.min(screen_width.saturating_sub(width));
    let id = {
        let mut gui = state().lock();
        if let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        {
            window.visible = true;
            window.key_buffer = TerminalKeyBuffer::new();
            window.id
        } else {
            let slot = gui.windows.iter().position(|window| !window.visible)?;
            let mut window = Window::empty();
            window.id = gui.next_window_id;
            window.owner = owner;
            gui.next_window_id = gui.next_window_id.wrapping_add(1).max(1);
            window.x = x;
            window.y = y;
            window.width = width;
            window.height = height;
            window.title[..8].copy_from_slice(b"Terminal");
            window.title_length = 8;
            window.visible = true;
            window.terminal = true;
            gui.windows[slot] = window;
            gui.raise_window(slot);
            window.id
        }
    };
    {
        let mut gui = state().lock();
        gui.focused = Some(id);
        if let Some(slot) = gui.windows.iter().position(|window| window.id == id) {
            gui.raise_window(slot);
        }
    }
    render_desktop();
    Some(id)
}

pub fn has_terminal(owner: u64) -> bool {
    state()
        .lock()
        .windows
        .iter()
        .any(|window| window.terminal && window.owner == owner)
}

pub fn terminal_is_visible(owner: u64) -> bool {
    state()
        .lock()
        .windows
        .iter()
        .any(|window| window.terminal && window.owner == owner && window.visible)
}

pub fn poll_terminal_key(owner: u64) -> Option<u8> {
    state().lock().pop_focused_terminal_key(owner)
}

pub fn set_terminal_color(owner: u64, color: (u8, u8, u8)) -> bool {
    let mut gui = state().lock();
    let Some(window) = gui
        .windows
        .iter_mut()
        .find(|window| window.terminal && window.owner == owner)
    else {
        return false;
    };
    window.terminal_buffer.set_foreground(color);
    true
}

pub fn write_terminal(owner: u64, bytes: &[u8]) -> bool {
    let damage = {
        let mut gui = state().lock();
        let active = gui.active;
        let batching = gui.terminal_batch_owner == Some(owner);
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        else {
            return false;
        };
        let old_line_count = window.terminal_buffer.line_count;
        let (start_row, end_row, scrolled) = window.terminal_buffer.append(bytes);
        let visible_rows =
            (window.height.saturating_sub(54) / TERMINAL_LINE_STRIDE).min(TERMINAL_ROWS);
        let full_redraw = scrolled || (old_line_count >= visible_rows && end_row != start_row);
        let buffer = &mut window.terminal_buffer;
        buffer.record_pending_damage((start_row, end_row), full_redraw);
        if active && window.visible && !batching {
            buffer.take_pending_damage().map(|rows| (owner, rows))
        } else {
            None
        }
    };
    if let Some((owner, rows)) = damage {
        render_terminal_rows(owner, rows);
    }
    true
}

pub fn begin_terminal_batch(owner: u64) -> bool {
    let mut gui = state().lock();
    if !gui
        .windows
        .iter()
        .any(|window| window.terminal && window.owner == owner)
    {
        return false;
    }
    if gui.terminal_batch_owner.is_some_and(|batch_owner| batch_owner != owner) {
        return false;
    }
    gui.terminal_batch_owner = Some(owner);
    gui.terminal_batch_depth = gui.terminal_batch_depth.saturating_add(1);
    true
}

pub fn end_terminal_batch(owner: u64) {
    let damage = {
        let mut gui = state().lock();
        let active = gui.active;
        if gui.terminal_batch_owner != Some(owner) || gui.terminal_batch_depth == 0 {
            return;
        }
        gui.terminal_batch_depth -= 1;
        if gui.terminal_batch_depth != 0 {
            return;
        }
        gui.terminal_batch_owner = None;
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        else {
            return;
        };
        window
            .terminal_buffer
            .take_pending_damage()
            .filter(|_| active && window.visible)
            .map(|rows| (owner, rows))
    };
    if let Some((owner, rows)) = damage {
        render_terminal_rows(owner, rows);
    }
}

pub fn clear_terminal(owner: u64) -> bool {
    let redraw = {
        let mut gui = state().lock();
        let active = gui.active;
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        else {
            return false;
        };
        let foreground = window.terminal_buffer.foreground;
        window.terminal_buffer = TerminalBuffer::new();
        window.terminal_buffer.set_foreground(foreground);
        active && window.visible
    };
    if redraw {
        render_desktop();
    }
    true
}

pub fn begin_terminal_input(owner: u64) -> bool {
    let mut gui = state().lock();
    let Some(window) = gui
        .windows
        .iter_mut()
        .find(|window| window.terminal && window.owner == owner)
    else {
        return false;
    };
    window.terminal_buffer.begin_input();
    true
}

pub fn redraw_terminal_input(owner: u64, bytes: &[u8], cursor: usize) -> bool {
    let redraw = {
        let mut gui = state().lock();
        let active = gui.active;
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        else {
            return false;
        };
        window.terminal_buffer.redraw_input(bytes, cursor.min(bytes.len()));
        active && window.visible
    };
    if redraw {
        render_desktop();
    }
    true
}

pub fn finish_terminal_input(owner: u64) -> bool {
    let redraw = {
        let mut gui = state().lock();
        let active = gui.active;
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        else {
            return false;
        };
        window.terminal_buffer.input_anchor = None;
        window.terminal_buffer.input_cursor = None;
        active && window.visible
    };
    if redraw {
        render_desktop();
    }
    true
}

pub fn render_terminal_editor(
    owner: u64,
    name: &[u8],
    text: &[u8],
    cursor: usize,
    dirty: bool,
) -> bool {
    let redraw = {
        let mut gui = state().lock();
        let active = gui.active;
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.terminal && window.owner == owner)
        else {
            return false;
        };
        let buffer = &mut window.terminal_buffer;
        *buffer = TerminalBuffer::new();
        buffer.append(name);
        if dirty {
            buffer.append(b" *");
        }
        buffer.append(b"\n");
        for (index, byte) in text.iter().copied().enumerate() {
            if index == cursor {
                buffer.mark_input_cursor();
            }
            buffer.append(&[byte]);
        }
        if cursor >= text.len() {
            buffer.mark_input_cursor();
        }
        buffer.append(b"\n^S save  Esc/Q quit");
        active && window.visible
    };
    if redraw {
        render_desktop();
    }
    true
}

pub fn close_window(id: u32) -> bool {
    close_window_owned(id, 0)
}

pub fn close_window_owned(id: u32, owner: u64) -> bool {
    let redraw = {
        let mut gui = state().lock();
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.visible && window.id == id && window.owner == owner)
        else {
            return false;
        };
        window.visible = false;
        let closed_id = window.id;
        drop(window);
        if gui.focused == Some(closed_id) {
            gui.focused = None;
        }
        gui.remove_owner_events(owner);
        gui.active
    };
    if redraw {
        render_desktop();
    }
    true
}

pub fn close_windows_owned(owner: u64) {
    let redraw = {
        let mut gui = state().lock();
        let mut changed = false;
        let focused = gui.focused;
        for window in gui.windows.iter_mut() {
            if window.visible && window.owner == owner {
                window.visible = false;
                changed = true;
            }
        }
        if focused.is_some_and(|id| {
            gui.windows
                .iter()
                .all(|window| !window.visible || window.id != id)
        }) {
            gui.focused = None;
        }
        if changed {
            gui.remove_owner_events(owner);
        }
        changed && gui.active
    };
    if redraw {
        render_desktop();
    }
}

pub fn draw_window_rect(
    owner: u64,
    id: u32,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: (u8, u8, u8),
) -> bool {
    let draw = {
        let mut gui = state().lock();
        let active = gui.active;
        let Some(window) = gui
            .windows
            .iter_mut()
            .find(|window| window.visible && window.id == id && window.owner == owner)
        else {
            return false;
        };
        if !window.add_rect(x, y, width, height, color) {
            return false;
        };
        active.then_some((window.x, window.y, gui.cursor))
    };
    if let Some((window_x, window_y, cursor)) = draw {
        let _canvas = CANVAS_LOCK.lock();
        toggle_cursor_locked(cursor);
        framebuffer::draw_rect(window_x + x, window_y + y, width, height, color);
        toggle_cursor_locked(cursor);
    }
    true
}

pub fn draw_window_text(
    owner: u64,
    id: u32,
    x: usize,
    y: usize,
    text: &[u8],
    color: (u8, u8, u8),
) -> bool {
    let mut gui = state().lock();
    let Some(window) = gui
        .windows
        .iter_mut()
        .find(|window| window.visible && window.id == id && window.owner == owner)
    else {
        return false;
    };
    window.add_text(x, y, text, color)
}

pub fn show_desktop() {
    enable();
    let gui = state().lock();
    if !gui.windows.iter().any(|window| window.visible) {
        drop(gui);
        let _ = create_window(0, 48, 96, 280, 180, b"Terminal");
        let _ = create_window(0, 360, 96, 280, 180, b"System");
    } else {
        drop(gui);
    }
    render_desktop();
}

fn render_desktop() {
    if !framebuffer::is_initialized() {
        return;
    }
    let _canvas = CANVAS_LOCK.lock();
    render_desktop_locked();
}

fn render_desktop_locked() {
    state().lock().drag_surface = None;
    framebuffer::begin_frame();
    render_desktop_contents(None, true);
    framebuffer::end_frame();
}

fn render_desktop_contents(excluded_window: Option<u32>, draw_cursor: bool) {
    let (focused, cursor, focused_title, focused_title_length) = {
        let gui = state().lock();
        let (title, title_length) = focused_title(&gui);
        (gui.focused, gui.cursor, title, title_length)
    };
    let (screen_width, screen_height) = framebuffer::dimensions().unwrap_or((1280, 800));

    framebuffer::draw_rect(0, 0, usize::MAX, usize::MAX, DESKTOP);

    let gui = state().lock();
    for slot in gui.z_order {
        let source = &gui.windows[slot];
        if !source.visible || Some(source.id) == excluded_window {
            continue;
        }
        let window = WindowSnapshot {
            id: source.id,
            x: source.x,
            y: source.y,
            width: source.width,
            height: source.height,
            title: source.title,
            title_length: source.title_length,
            rect_count: source.rect_count,
        };
        draw_window_surface(source, &window, focused == Some(window.id));
    }
    drop(gui);
    draw_top_bar(screen_width, &focused_title[..focused_title_length]);
    draw_bottom_dock(screen_width, screen_height);
    if draw_cursor {
        toggle_cursor_locked(cursor);
    }
}

fn draw_top_bar(screen_width: usize, active_title: &[u8]) {
    framebuffer::draw_rect(0, 0, screen_width, STATUS_BAR_HEIGHT, PANEL);
    framebuffer::draw_rect(0, 0, screen_width, 1, ACCENT);
    framebuffer::draw_text(12, 12, b"LeafOS", TEXT);
    framebuffer::draw_text(80, 12, active_title, MUTED);

    let mut clock = [0; 8];
    format_clock(&mut clock, crate::timer::ticks());
    framebuffer::draw_text(screen_width.saturating_sub(136), 12, &clock, TEXT);

    for core in 0..3 {
        let x = screen_width.saturating_sub(44) + core * 12;
        framebuffer::draw_rect(x, 6, 8, 14, CARD);
        let idle = crate::scheduler::cpu_is_idle(core).unwrap_or(true);
        let load_height = if idle { 4 } else { 12 };
        framebuffer::draw_rect(
            x + 1,
            19 - load_height,
            6,
            load_height,
            if idle { MUTED } else { ACCENT },
        );
        framebuffer::draw_text(x, 22, &[b'0' + core as u8], MUTED);
    }
}

fn format_clock(output: &mut [u8; 8], ticks: u64) {
    let total_seconds = ticks / crate::timer::TICKS_PER_SECOND;
    let hours = (total_seconds / 3_600) % 24;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;
    output[0] = b'0' + (hours / 10) as u8;
    output[1] = b'0' + (hours % 10) as u8;
    output[2] = b':';
    output[3] = b'0' + (minutes / 10) as u8;
    output[4] = b'0' + (minutes % 10) as u8;
    output[5] = b':';
    output[6] = b'0' + (seconds / 10) as u8;
    output[7] = b'0' + (seconds % 10) as u8;
}

fn draw_bottom_dock(screen_width: usize, screen_height: usize) {
    let dock = bottom_dock_bounds(screen_width, screen_height);
    framebuffer::draw_rect(dock.0, dock.1, dock.2, dock.3, DOCK);
    for (index, label) in [
        b"[ TERM ]".as_slice(),
        b"[ EDIT ]".as_slice(),
        b"[ INFO ]".as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let button = dock_button_bounds(index, dock);
        framebuffer::draw_rect(button.0, button.1, button.2, button.3, DOCK_BUTTON);
        let text_width = label.len().saturating_mul(TERMINAL_CELL_WIDTH);
        let text_x = button.0 + button.2.saturating_sub(text_width) / 2;
        let text_y = button.1 + button.3.saturating_sub(8) / 2;
        framebuffer::draw_text(text_x, text_y, label, TEXT);
    }
}

pub fn refresh_top_bar_if_due() {
    if !framebuffer::is_initialized() {
        return;
    }
    let second = crate::timer::uptime_seconds();
    let mut previous = LAST_TOP_BAR_SECOND.load(Ordering::Acquire);
    loop {
        if previous == second {
            return;
        }
        match LAST_TOP_BAR_SECOND.compare_exchange_weak(
            previous,
            second,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(actual) => previous = actual,
        }
    }

    if !is_active() {
        return;
    }

    let _canvas = CANVAS_LOCK.lock();
    let (active, cursor, title) = {
        let gui = state().lock();
        (gui.active, gui.cursor, focused_title(&gui))
    };
    if !active {
        return;
    }
    let (screen_width, _) = framebuffer::dimensions().unwrap_or((1280, 800));
    toggle_cursor_locked(cursor);
    framebuffer::begin_frame();
    draw_top_bar(screen_width, &title.0[..title.1]);
    framebuffer::end_frame();
    toggle_cursor_locked(cursor);
}

fn draw_terminal_contents(window: &Window) {
    draw_terminal_rows_contents(window, None);
}

fn draw_terminal_rows_contents(window: &Window, dirty_rows: Option<(usize, usize)>) {
    const TERMINAL_BACKGROUND: (u8, u8, u8) = (8, 14, 22);
    let body_x = window.x.saturating_add(8);
    let body_y = window.y.saturating_add(46);
    let body_width = window.width.saturating_sub(16);
    let body_height = window.height.saturating_sub(54);

    let rows = (body_height / TERMINAL_LINE_STRIDE).min(TERMINAL_ROWS);
    let columns = (body_width / TERMINAL_CELL_WIDTH).min(TERMINAL_COLUMNS);
    let first_visible = window
        .terminal_buffer
        .line_count
        .saturating_sub(rows.saturating_add(window.terminal_buffer.scrollback_offset));
    if dirty_rows.is_none() {
        framebuffer::draw_rect(body_x, body_y, body_width, body_height, TERMINAL_BACKGROUND);
    }
    let (start_row, end_row) =
        dirty_rows.unwrap_or((first_visible, first_visible + rows.saturating_sub(1)));
    for row in start_row.max(first_visible)..=end_row.min(first_visible + rows.saturating_sub(1)) {
        if rows == 0 || start_row > end_row {
            break;
        }
        let line_index = (window.terminal_buffer.first_line + row) % TERMINAL_ROWS;
        let line = &window.terminal_buffer.lines[line_index];
        let length = line.length.min(columns);
        framebuffer::draw_rect(
            body_x,
            body_y + (row - first_visible) * TERMINAL_LINE_STRIDE,
            body_width,
            TERMINAL_LINE_STRIDE,
            TERMINAL_BACKGROUND,
        );
        if length != 0 {
            let color = (
                ((line.color >> 16) & 0xff) as u8,
                ((line.color >> 8) & 0xff) as u8,
                (line.color & 0xff) as u8,
            );
            framebuffer::draw_text(
                body_x,
                body_y + (row - first_visible) * TERMINAL_LINE_STRIDE,
                &line.bytes[..length],
                color,
            );
        }
    }
    if let Some((cursor_line, cursor_column)) = window.terminal_buffer.input_cursor {
        let cursor_buffer_row =
            (cursor_line + TERMINAL_ROWS - window.terminal_buffer.first_line) % TERMINAL_ROWS;
        if cursor_buffer_row >= first_visible
            && cursor_buffer_row < first_visible + rows
            && cursor_column < columns
            && dirty_rows.is_none_or(|(start, end)| (start..=end).contains(&cursor_buffer_row))
        {
            framebuffer::draw_rect(
                body_x + cursor_column * TERMINAL_CELL_WIDTH,
                body_y + (cursor_buffer_row - first_visible) * TERMINAL_LINE_STRIDE + 6,
                6,
                2,
                window.terminal_buffer.foreground,
            );
        }
    }
}

fn render_terminal_rows(owner: u64, rows: Option<(usize, usize)>) {
    if !framebuffer::is_initialized() {
        return;
    }
    let _canvas = CANVAS_LOCK.lock();
    let (active, cursor) = {
        let gui = state().lock();
        (gui.active, gui.cursor)
    };
    if active {
        toggle_cursor_locked(cursor);
    }
    render_terminal_rows_locked(owner, rows);
    if active {
        toggle_cursor_locked(cursor);
    }
}

fn render_terminal_rows_locked(owner: u64, rows: Option<(usize, usize)>) {
    if !framebuffer::is_initialized() {
        return;
    }
    state().lock().drag_surface = None;
    framebuffer::begin_frame();
    {
        let gui = state().lock();
        if let Some(window) = gui
            .windows
            .iter()
            .find(|window| window.visible && window.terminal && window.owner == owner)
        {
            draw_terminal_rows_contents(window, rows);
        }
    }
    framebuffer::end_frame();
}

fn draw_window_surface(source: &Window, window: &WindowSnapshot, focused: bool) {
    let color = if focused { ACCENT } else { CARD };
    framebuffer::draw_rect(window.x, window.y, window.width, window.height, color);
    framebuffer::draw_rect(window.x, window.y, window.width, 4, ACCENT);
    framebuffer::draw_text(
        window.x + 24,
        window.y + 28,
        &window.title[..window.title_length],
        TEXT,
    );
    if window.width >= 24 {
        framebuffer::draw_rect(
            window.x + window.width - 22,
            window.y + 8,
            16,
            16,
            (150, 70, 90),
        );
        framebuffer::draw_text(window.x + window.width - 18, window.y + 12, b"x", TEXT);
    }
    if window.width >= RESIZE_GRIP_SIZE && window.height >= RESIZE_GRIP_SIZE {
        let grip_x = window.x + window.width - RESIZE_GRIP_SIZE;
        let grip_y = window.y + window.height - RESIZE_GRIP_SIZE;
        framebuffer::draw_rect(grip_x + 4, grip_y + 8, 4, 1, MUTED);
        framebuffer::draw_rect(grip_x + 7, grip_y + 5, 1, 4, MUTED);
        framebuffer::draw_rect(grip_x + 10, grip_y + 2, 1, 7, MUTED);
    }

    if source.terminal {
        draw_terminal_contents(source);
    } else {
        framebuffer::draw_text(window.x + 24, window.y + 58, b"Window surface ready", MUTED);
        for rect in source.rects[..window.rect_count].iter() {
            framebuffer::draw_rect(
                window.x + rect.x,
                window.y + rect.y,
                rect.width,
                rect.height,
                rect.color,
            );
        }
        for text in source.texts[..source.text_count].iter() {
            framebuffer::draw_text(
                window.x + text.x,
                window.y + text.y,
                &text.bytes[..text.length],
                text.color,
            );
        }
    }
}

fn render_drag_damage_locked(
    old_geometry: (usize, usize, usize, usize),
    new_geometry: (usize, usize, usize, usize),
    dragged_window_id: Option<u32>,
) {
    let damage = (
        old_geometry.0.min(new_geometry.0),
        old_geometry.1.min(new_geometry.1),
        old_geometry
            .0
            .saturating_add(old_geometry.2)
            .max(new_geometry.0.saturating_add(new_geometry.2)),
        old_geometry
            .1
            .saturating_add(old_geometry.3)
            .max(new_geometry.1.saturating_add(new_geometry.3)),
    );
    let gui = state().lock();
    if let Some(window_id) = dragged_window_id
        && let Some(window) = gui
            .windows
            .iter()
            .find(|window| window.visible && window.id == window_id)
        && let Some(surface) = gui
            .drag_surface
            .as_ref()
            .filter(|surface| surface.window_id == window_id)
    {
        let (screen_width, screen_height) = framebuffer::dimensions().unwrap_or((1280, 800));
        let start_x = damage.0.min(screen_width);
        let start_y = damage.1.min(screen_height);
        let end_x = damage.2.min(screen_width);
        let end_y = damage.3.min(screen_height);
        if start_x < end_x
            && start_y < end_y
            && framebuffer::blit_snapshot_rect(
                start_x,
                start_y,
                start_x,
                start_y,
                end_x - start_x,
                end_y - start_y,
                surface.background_width,
                surface.background_height,
                &surface.background,
            )
        {
            framebuffer::blit_rect(
                window.x,
                window.y,
                surface.width,
                surface.height,
                &surface.pixels,
            );
            return;
        }
    }

    framebuffer::draw_rect(
        damage.0,
        damage.1,
        damage.2.saturating_sub(damage.0),
        damage.3.saturating_sub(damage.1),
        DESKTOP,
    );
    for slot in gui.z_order {
        let source = &gui.windows[slot];
        if !source.visible || Some(source.id) == dragged_window_id {
            continue;
        }
        let window_end_x = source.x.saturating_add(source.width);
        let window_end_y = source.y.saturating_add(source.height);
        if source.x >= damage.2
            || window_end_x <= damage.0
            || source.y >= damage.3
            || window_end_y <= damage.1
        {
            continue;
        }
        let window = WindowSnapshot {
            id: source.id,
            x: source.x,
            y: source.y,
            width: source.width,
            height: source.height,
            title: source.title,
            title_length: source.title_length,
            rect_count: source.rect_count,
        };
        draw_window_surface(source, &window, gui.focused == Some(source.id));
    }
    if let Some(window_id) = dragged_window_id
        && let Some(window) = gui
            .windows
            .iter()
            .find(|window| window.visible && window.id == window_id)
    {
        let snapshot = WindowSnapshot {
            id: window.id,
            x: window.x,
            y: window.y,
            width: window.width,
            height: window.height,
            title: window.title,
            title_length: window.title_length,
            rect_count: window.rect_count,
        };
        draw_window_surface(window, &snapshot, gui.focused == Some(window.id));
    }
    let (screen_width, screen_height) = framebuffer::dimensions().unwrap_or((1280, 800));
    let (title, title_length) = focused_title(&gui);
    draw_top_bar(screen_width, &title[..title_length]);
    draw_bottom_dock(screen_width, screen_height);
}

fn toggle_cursor((x, y): (i32, i32)) {
    let _canvas = CANVAS_LOCK.lock();
    toggle_cursor_locked((x, y));
}

fn toggle_cursor_locked((x, y): (i32, i32)) {
    if x < 0 || y < 0 {
        return;
    }
    framebuffer::xor_rect(x as usize, y as usize, 10, 2, TEXT);
    framebuffer::xor_rect(x as usize, y as usize, 2, 10, TEXT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn event_queue_is_fifo_and_bounded() {
        let mut state = GuiState::new();
        state.active = true;
        for byte in 0..EVENT_CAPACITY as u8 {
            state.push_event(0, GuiEvent::Key(byte));
        }
        state.push_event(0, GuiEvent::Key(0xff));
        for byte in 0..EVENT_CAPACITY as u8 {
            assert_eq!(state.pop_event(0), Some(GuiEvent::Key(byte)));
        }
        assert_eq!(state.pop_event(0), None);
    }

    #[test_case]
    fn event_queue_does_not_cross_owner_boundaries() {
        let mut state = GuiState::new();
        state.push_event(7, GuiEvent::Key(b'a'));
        state.push_event(8, GuiEvent::Key(b'b'));
        assert_eq!(state.pop_event(8), Some(GuiEvent::Key(b'b')));
        assert_eq!(state.pop_event(7), Some(GuiEvent::Key(b'a')));
    }

    #[test_case]
    fn terminal_keys_are_isolated_and_only_read_while_focused() {
        let mut state = GuiState::new();
        state.active = true;
        for (slot, id, owner) in [(0, 10, 7), (1, 11, 8)] {
            let mut window = Window::empty();
            window.id = id;
            window.owner = owner;
            window.visible = true;
            window.terminal = true;
            state.windows[slot] = window;
        }

        state.focused = Some(10);
        assert!(state.push_focused_key(b'a'));
        state.focused = Some(11);
        assert!(state.push_focused_key(b'b'));
        assert_eq!(state.pop_focused_terminal_key(7), None);
        assert_eq!(state.pop_focused_terminal_key(8), Some(b'b'));
        state.focused = Some(10);
        assert_eq!(state.pop_focused_terminal_key(7), Some(b'a'));
        assert_eq!(state.pop_focused_terminal_key(7), None);
        state.focused = None;
        assert!(state.push_focused_key(b'c'));
        state.focused = Some(10);
        assert_eq!(state.pop_focused_terminal_key(7), None);
    }

    #[test_case]
    fn dock_shortcut_hitboxes_match_the_centered_dock() {
        let dock = bottom_dock_bounds(1920, 1080);
        assert_eq!(dock, (760, 1016, 400, 48));
        let actions = [DockAction::Terminal, DockAction::Edit, DockAction::Info];
        for (index, action) in actions.into_iter().enumerate() {
            let button = dock_button_bounds(index, dock);
            assert_eq!(
                dock_action_at(
                    (button.0 + button.2 / 2) as i32,
                    (button.1 + button.3 / 2) as i32,
                    1920,
                    1080,
                ),
                Some(action)
            );
        }
        assert_eq!(dock_action_at(0, 0, 1920, 1080), None);
    }

    #[test_case]
    fn live_clock_formats_ticks_as_twenty_four_hour_time() {
        let mut clock = [0; 8];
        format_clock(&mut clock, 12_345);
        assert_eq!(&clock, b"00:02:03");
        format_clock(&mut clock, 24 * 60 * 60 * crate::timer::TICKS_PER_SECOND);
        assert_eq!(&clock, b"00:00:00");
    }

    #[test_case]
    fn terminal_key_buffer_queues_shortcuts_atomically() {
        let mut buffer = TerminalKeyBuffer::new();
        assert!(buffer.push_bytes(b"info\n"));
        assert_eq!(buffer.length, 5);
        assert!(buffer.push_bytes(&[b'x'; KEY_BUFFER_CAPACITY - 5]));
        assert!(!buffer.push_bytes(b"edit "));
        assert_eq!(buffer.length, KEY_BUFFER_CAPACITY);
    }

    #[test_case]
    fn z_order_raises_a_window_without_duplicates() {
        let mut state = GuiState::new();
        state.raise_window(2);
        assert_eq!(state.z_order, [0, 1, 3, 4, 5, 6, 7, 2]);
        state.raise_window(2);
        assert_eq!(state.z_order, [0, 1, 3, 4, 5, 6, 7, 2]);
        state.raise_window(0);
        assert_eq!(state.z_order, [1, 3, 4, 5, 6, 7, 2, 0]);
    }

    #[test_case]
    fn inverse_mouse_setting_reverses_the_wheel_delta() {
        let mut state = GuiState::new();
        assert_eq!(state.scroll_delta(2), 2);
        assert!(state.toggle_mouse_inverse());
        assert_eq!(state.scroll_delta(2), -2);
        assert!(!state.toggle_mouse_inverse());
        assert_eq!(state.scroll_delta(i32::MIN), i32::MIN);
    }

    #[test_case]
    fn window_rects_are_bounded_and_limited() {
        let mut window = Window::empty();
        window.width = 20;
        window.height = 10;
        assert!(window.add_rect(2, 3, 5, 4, (1, 2, 3)));
        assert!(!window.add_rect(18, 3, 3, 2, (1, 2, 3)));
        assert!(!window.add_rect(2, 3, 0, 2, (1, 2, 3)));
        for _ in 1..MAX_WINDOW_RECTS {
            assert!(window.add_rect(0, 0, 1, 1, (1, 2, 3)));
        }
        assert!(!window.add_rect(0, 0, 1, 1, (1, 2, 3)));
    }

    #[test_case]
    fn window_text_is_bounded_and_limited() {
        let mut window = Window::empty();
        window.width = 64;
        window.height = 16;
        assert!(window.add_text(0, 0, b"hello", (1, 2, 3)));
        assert!(!window.add_text(25, 0, b"hello", (1, 2, 3)));
        assert!(!window.add_text(0, 9, b"hello", (1, 2, 3)));
        assert!(!window.add_text(0, 0, b"", (1, 2, 3)));
        for _ in 1..MAX_WINDOW_TEXT {
            assert!(window.add_text(0, 0, b"x", (1, 2, 3)));
        }
        assert!(!window.add_text(0, 0, b"x", (1, 2, 3)));
    }

    #[test_case]
    fn terminal_buffer_wraps_at_eighty_columns() {
        let mut buffer = TerminalBuffer::new();
        let full_line = [b'x'; TERMINAL_COLUMNS];
        buffer.append(&full_line);
        buffer.append(b"y");
        assert_eq!(buffer.line_count, 2);
        assert_eq!(buffer.line(0), &full_line);
        assert_eq!(buffer.line(1), b"y");
    }

    #[test_case]
    fn terminal_buffer_preserves_foreground_color_per_line() {
        let mut buffer = TerminalBuffer::new();
        buffer.append(b"old\n");
        buffer.set_foreground((255, 0, 0));
        buffer.append(b"new");

        let old_line = &buffer.lines[buffer.first_line];
        let new_line = &buffer.lines[(buffer.first_line + 1) % TERMINAL_ROWS];
        let default = (u32::from(DEFAULT_TERMINAL_TEXT.0) << 16)
            | (u32::from(DEFAULT_TERMINAL_TEXT.1) << 8)
            | u32::from(DEFAULT_TERMINAL_TEXT.2);
        assert_eq!(&old_line.bytes[..3], b"old");
        assert_eq!(old_line.color, default);
        assert_eq!(&new_line.bytes[..3], b"new");
        assert_eq!(new_line.color, 0x00ff_0000);
    }

    #[test_case]
    fn terminal_buffer_scrolls_off_the_oldest_line() {
        let mut buffer = TerminalBuffer::new();
        for line in 0..=TERMINAL_ROWS {
            buffer.append(&[b'0' + (line % 10) as u8]);
            buffer.append(b"\n");
        }
        assert_eq!(buffer.line_count, TERMINAL_ROWS);
        assert_eq!(buffer.line(0), b"2");
    }

    #[test_case]
    fn terminal_scrollback_is_bounded_by_visible_history() {
        let mut buffer = TerminalBuffer::new();
        for _ in 0..10 {
            buffer.append(b"line\n");
        }
        assert!(buffer.scroll(3, 4));
        assert_eq!(buffer.scrollback_offset, 3);
        assert!(buffer.scroll(i32::MAX, 4));
        assert_eq!(buffer.scrollback_offset, buffer.line_count - 4);
        assert!(buffer.scroll(i32::MIN, 4));
        assert_eq!(buffer.scrollback_offset, 0);
        assert!(!buffer.scroll(-1, 4));
    }

    #[test_case]
    fn terminal_input_redraw_preserves_its_prompt_and_cursor() {
        let mut buffer = TerminalBuffer::new();
        buffer.append(b"leafos:/> ");
        buffer.begin_input();
        buffer.redraw_input(b"echo", 2);
        assert_eq!(buffer.line(0), b"leafos:/> echo");
        assert_eq!(buffer.input_cursor, Some((0, 12)));
        buffer.redraw_input(b"help", 4);
        assert_eq!(buffer.line(0), b"leafos:/> help");
    }

    #[test_case]
    fn terminal_batch_coalesces_dirty_rows_and_full_scroll_damage() {
        let mut buffer = TerminalBuffer::new();
        buffer.record_pending_damage((2, 3), false);
        buffer.record_pending_damage((7, 9), false);
        assert_eq!(buffer.take_pending_damage(), Some(Some((2, 9))));

        buffer.record_pending_damage((1, 1), false);
        buffer.record_pending_damage((0, TERMINAL_ROWS - 1), true);
        assert_eq!(buffer.take_pending_damage(), Some(None));
        assert_eq!(buffer.take_pending_damage(), None);
    }

    #[test_case]
    fn removing_owner_events_preserves_other_clients() {
        let mut state = GuiState::new();
        state.push_event(7, GuiEvent::Key(b'a'));
        state.push_event(8, GuiEvent::Key(b'b'));
        state.push_event(7, GuiEvent::Key(b'c'));
        state.remove_owner_events(7);
        assert_eq!(state.pop_event(7), None);
        assert_eq!(state.pop_event(8), Some(GuiEvent::Key(b'b')));
    }
}
