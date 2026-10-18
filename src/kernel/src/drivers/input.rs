//! Small nonblocking byte queue used by the console input syscall.
//!
//! Keyboard interrupt handling feeds this queue after translating key events.
//! A full queue drops new bytes rather than overwriting unread input.

use crate::sync::Mutex;

pub const INPUT_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
pub struct InputBuffer {
    bytes: [u8; INPUT_CAPACITY],
    head: usize,
    length: usize,
}

impl InputBuffer {
    pub const fn new() -> Self {
        Self {
            bytes: [0; INPUT_CAPACITY],
            head: 0,
            length: 0,
        }
    }

    pub fn push(&mut self, byte: u8) {
        if self.length == INPUT_CAPACITY {
            return;
        }
        let tail = (self.head + self.length) % INPUT_CAPACITY;
        self.bytes[tail] = byte;
        self.length += 1;
    }

    pub fn read(&mut self, output: &mut [u8]) -> usize {
        let count = output.len().min(self.length);
        for slot in output.iter_mut().take(count) {
            *slot = self.bytes[self.head];
            self.head = (self.head + 1) % INPUT_CAPACITY;
            self.length -= 1;
        }
        count
    }
}

static INPUT: Mutex<InputBuffer> = Mutex::new(InputBuffer::new());

/// Enqueue one translated byte from the keyboard interrupt path.
pub fn enqueue(byte: u8) {
    INPUT.lock().push(byte);
}

/// Read bytes already available; this function never blocks waiting for input.
pub fn read_available(output: &mut [u8]) -> usize {
    INPUT.lock().read(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn queue_reads_fifo_and_does_not_overwrite_when_full() {
        let mut queue = InputBuffer::new();
        queue.push(b'a');
        queue.push(b'b');
        queue.push(b'c');
        let mut first = [0; 2];
        assert_eq!(queue.read(&mut first), 2);
        assert_eq!(&first, b"ab");
        let mut second = [0; 2];
        assert_eq!(queue.read(&mut second), 1);
        assert_eq!(&second[..1], b"c");

        for index in 0..INPUT_CAPACITY {
            queue.push(index as u8);
        }
        queue.push(0xff);
        let mut full = [0; INPUT_CAPACITY];
        assert_eq!(queue.read(&mut full), INPUT_CAPACITY);
        assert_eq!(full[0], 0);
        assert_eq!(full[INPUT_CAPACITY - 1], (INPUT_CAPACITY - 1) as u8);
    }
}
