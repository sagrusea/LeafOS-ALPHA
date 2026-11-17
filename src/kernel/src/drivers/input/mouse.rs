use crate::{framebuffer, gui};
use x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

struct Mouse {
    packet: [u8; 4],
    index: usize,
    packet_length: usize,
    x: i32,
    y: i32,
}

impl Mouse {
    const fn new() -> Self {
        Self {
            packet: [0; 4],
            index: 0,
            packet_length: 3,
            x: 640,
            y: 400,
        }
    }

    fn byte(&mut self, value: u8) {
        if self.index == 0 && value & 0x08 == 0 {
            return;
        }
        self.packet[self.index] = value;
        self.index += 1;
        if self.index != self.packet_length {
            return;
        }
        self.index = 0;
        let flags = self.packet[0];
        let dx = i32::from(self.packet[1] as i8);
        let dy = i32::from(self.packet[2] as i8);
        let (width, height) = gui::screen_dimensions();
        self.x = (self.x + dx).clamp(0, width.saturating_sub(1) as i32);
        self.y = (self.y - dy).clamp(0, height.saturating_sub(1) as i32);
        let wheel_delta = if self.packet_length == 4 {
            decode_wheel(self.packet[3])
        } else {
            0
        };
        gui::push_mouse(self.x, self.y, flags & 0x07, wheel_delta);
    }
}

fn decode_wheel(byte: u8) -> i32 {
    i32::from((byte as i8) << 4 >> 4)
}

static MOUSE: crate::sync::Mutex<Mouse> = crate::sync::Mutex::new(Mouse::new());

fn send_mouse_byte(command: &mut Port<u8>, data: &mut Port<u8>, value: u8) -> bool {
    unsafe {
        let mut status = Port::<u8>::new(STATUS);
        for _ in 0..100_000 {
            if status.read() & 0x02 == 0 {
                break;
            }
        }
        if status.read() & 0x02 != 0 {
            return false;
        }
        command.write(0xD4);
        for _ in 0..100_000 {
            if status.read() & 0x02 == 0 {
                break;
            }
        }
        if status.read() & 0x02 != 0 {
            return false;
        }
        data.write(value);
        for _ in 0..100_000 {
            if status.read() & 0x01 != 0 {
                return data.read() == 0xFA;
            }
        }
        false
    }
}

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
        let wheel_mouse = {
            [200u8, 100, 80].into_iter().all(|rate| {
                send_mouse_byte(&mut command, &mut data, 0xF3)
                    && send_mouse_byte(&mut command, &mut data, rate)
            }) && send_mouse_byte(&mut command, &mut data, 0xF2)
                && status.read() & 0x01 != 0
                && matches!(data.read(), 0x03 | 0x04)
        };
        if wheel_mouse {
            MOUSE.lock().packet_length = 4;
        }
        let _ = send_mouse_byte(&mut command, &mut data, 0xF4);
    }
}

pub fn interrupt_byte() {
    let value = unsafe { Port::<u8>::new(DATA).read() };
    MOUSE.lock().byte(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn wheel_data_is_sign_extended_and_up_is_positive() {
        assert_eq!(decode_wheel(0x01), 1);
        assert_eq!(decode_wheel(0x0f), -1);
        assert_eq!(decode_wheel(0x00), 0);
    }
}
