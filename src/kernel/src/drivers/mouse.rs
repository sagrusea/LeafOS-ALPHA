use crate::{framebuffer, gui};
use x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

struct Mouse {
    packet: [u8; 3],
    index: usize,
    x: i32,
    y: i32,
}

impl Mouse {
    const fn new() -> Self {
        Self { packet: [0; 3], index: 0, x: 640, y: 400 }
    }

    fn byte(&mut self, value: u8) {
        if self.index == 0 && value & 0x08 == 0 {
            return;
        }
        self.packet[self.index] = value;
        self.index += 1;
        if self.index != 3 {
            return;
        }
        self.index = 0;
        let flags = self.packet[0];
        let dx = i32::from(self.packet[1] as i8);
        let dy = i32::from(self.packet[2] as i8);
        let (width, height) = gui::screen_dimensions();
        self.x = (self.x + dx).clamp(0, width.saturating_sub(1) as i32);
        self.y = (self.y - dy).clamp(0, height.saturating_sub(1) as i32);
        gui::push_mouse(self.x, self.y, flags & 0x07);
    }
}

static MOUSE: crate::sync::Mutex<Mouse> = crate::sync::Mutex::new(Mouse::new());

pub fn initialize() {
    if let Some((width, height)) = framebuffer::dimensions() {
        let mut mouse = MOUSE.lock();
        mouse.x = (width / 2) as i32;
        mouse.y = (height / 2) as i32;
    }
    unsafe {
        let mut status = Port::<u8>::new(STATUS);
        let mut command = Port::<u8>::new(COMMAND);
        let mut data = Port::<u8>::new(DATA);
        for _ in 0..100_000 {
            if status.read() & 1 == 0 {
                break;
            }
            let _ = data.read();
        }
        command.write(0xA8);
        command.write(0x20);
        for _ in 0..100_000 {
            if status.read() & 1 != 0 {
                break;
            }
        }
        let config = data.read() | 0x02;
        command.write(0x60);
        data.write(config);
        command.write(0xD4);
        data.write(0xF4);
        for _ in 0..100_000 {
            if status.read() & 1 != 0 {
                let _ = data.read();
                break;
            }
        }
    }
}

pub fn interrupt_byte() {
    let value = unsafe { Port::<u8>::new(DATA).read() };
    MOUSE.lock().byte(value);
}
