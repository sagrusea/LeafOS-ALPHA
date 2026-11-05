use crate::framebuffer;
use crate::sync::{Mutex, Once};

const MAX_WINDOWS: usize = 8;
const EVENT_CAPACITY: usize = 64;
const MAX_WINDOW_RECTS: usize = 32;

const DESKTOP: (u8, u8, u8) = (18, 30, 48);
const PANEL: (u8, u8, u8) = (28, 44, 68);
const CARD: (u8, u8, u8) = (38, 58, 86);
const ACCENT: (u8, u8, u8) = (80, 190, 160);
const TEXT: (u8, u8, u8) = (235, 245, 255);
const MUTED: (u8, u8, u8) = (160, 180, 200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiEvent {
    Key(u8),
    Mouse { x: i32, y: i32, buttons: u8 },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowRect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: (u8, u8, u8),
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
}

struct GuiState {
    active: bool,
    next_window_id: u32,
    windows: [Window; MAX_WINDOWS],
    focused: Option<u32>,
    cursor: (i32, i32),
    events: [Option<(u64, GuiEvent)>; EVENT_CAPACITY],
    event_head: usize,
    event_length: usize,
}

impl GuiState {
    const fn new() -> Self {
        Self {
            active: false,
            next_window_id: 1,
            windows: [Window::empty(); MAX_WINDOWS],
            focused: None,
            cursor: (640, 400),
            events: [None; EVENT_CAPACITY],
            event_head: 0,
            event_length: 0,
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
}

static GUI: Once<Mutex<GuiState>> = Once::new();

fn state() -> &'static Mutex<GuiState> {
    GUI.call_once(|| Mutex::new(GuiState::new()))
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
        gui.cursor
    };
    toggle_cursor(cursor);
}

pub fn push_key(byte: u8) {
    let mut gui = state().lock();
    if gui.active {
        let owner = gui
            .focused
            .and_then(|id| gui.windows.iter().find(|window| window.id == id))
            .map(|window| window.owner)
            .unwrap_or(0);
        gui.push_event(owner, GuiEvent::Key(byte));
    }
}

pub fn push_mouse(x: i32, y: i32, buttons: u8) {
    let (old_cursor, focus_changed);
    {
        let mut gui = state().lock();
        if !gui.active {
            return;
        }
        old_cursor = gui.cursor;
        let old_focus = gui.focused;
        gui.cursor = (x, y);
        if buttons & 1 != 0 {
            let hit = gui
                .windows
                .iter()
                .rev()
                .find(|window| {
                    window.visible
                        && x >= window.x as i32
                        && y >= window.y as i32
                        && x < (window.x + window.width) as i32
                        && y < (window.y + window.height) as i32
                })
                .map(|window| window.id);
            gui.focused = hit;
        }
        focus_changed = gui.focused != old_focus;
        let owner = gui
            .focused
            .and_then(|id| gui.windows.iter().find(|window| window.id == id))
            .map(|window| window.owner)
            .unwrap_or(0);
        gui.push_event(owner, GuiEvent::Mouse { x, y, buttons });
    }
    if focus_changed {
        render_desktop();
    } else {
        toggle_cursor(old_cursor);
        toggle_cursor((x, y));
    }
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
        }
        active.then_some((window.x, window.y))
    };
    if let Some((window_x, window_y)) = draw {
        framebuffer::draw_rect(window_x + x, window_y + y, width, height, color);
    }
    true
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

    let (focused, cursor) = {
        let gui = state().lock();
        (gui.focused, gui.cursor)
    };

    framebuffer::clear();
    framebuffer::draw_rect(0, 0, usize::MAX, usize::MAX, DESKTOP);
    framebuffer::draw_rect(0, 0, usize::MAX, 42, PANEL);
    framebuffer::draw_rect(0, 0, 6, 42, ACCENT);
    framebuffer::draw_text(24, 16, b"LeafOS Desktop", TEXT);
    framebuffer::draw_text(720, 16, b"GUI foundation", MUTED);

    for window_index in 0..MAX_WINDOWS {
        let Some((window, rect_count)) = ({
            let gui = state().lock();
            let source = &gui.windows[window_index];
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
            if source.visible {
                Some((window, window.rect_count))
            } else {
                None
            }
        }) else {
            continue;
        };
        let color = (focused == Some(window.id)).then_some(ACCENT).unwrap_or(CARD);
        framebuffer::draw_rect(window.x, window.y, window.width, window.height, color);
        framebuffer::draw_rect(window.x, window.y, window.width, 4, ACCENT);
        framebuffer::draw_text(
            window.x + 24,
            window.y + 28,
            &window.title[..window.title_length],
            TEXT,
        );
        framebuffer::draw_text(window.x + 24, window.y + 58, b"Window surface ready", MUTED);
        for rect_index in 0..rect_count {
            let rect = {
                let gui = state().lock();
                gui.windows[window_index].rects[rect_index]
            };
            framebuffer::draw_rect(
                window.x + rect.x,
                window.y + rect.y,
                rect.width,
                rect.height,
                rect.color,
            );
        }
    }
    let (cursor_x, cursor_y) = cursor;
    toggle_cursor((cursor_x, cursor_y));
    framebuffer::draw_text(48, 320, b"Keyboard events are queued for GUI clients.", ACCENT);
}

fn toggle_cursor((x, y): (i32, i32)) {
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
}
