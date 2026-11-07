pub mod framebuffer;
pub mod serial;
pub mod vga_buffer;

/// Write a bounded byte buffer to the active text console. Use the framebuffer
/// for interactive boots and serial as a fallback for headless/test boots.
pub fn write_bytes(bytes: &[u8]) {
    if let Some(owner) = crate::tasking::scheduler::current_task_id().map(|task| task.get())
        && crate::gui::write_terminal(owner, bytes)
    {
        return;
    }
    if framebuffer::is_initialized() {
        framebuffer::write_bytes(bytes);
    } else {
        serial::write_bytes(bytes);
    }
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    if let Some(owner) = crate::tasking::scheduler::current_task_id().map(|task| task.get())
        && crate::gui::has_terminal(owner)
    {
        let mut writer = TerminalConsoleWriter::new(owner);
        if core::fmt::write(&mut writer, args).is_ok() && writer.flush().is_ok() {
            return;
        }
        serial::_print(format_args!(
            "[WARN] Could not write formatted output to terminal window for task {}.\n",
            owner
        ));
        return;
    }

    if framebuffer::is_initialized() {
        framebuffer::_print(args);
    } else {
        serial::_print(args);
    }
}

const TERMINAL_WRITE_CHUNK: usize = 256;

struct TerminalConsoleWriter {
    owner: u64,
    bytes: [u8; TERMINAL_WRITE_CHUNK],
    length: usize,
}

impl TerminalConsoleWriter {
    const fn new(owner: u64) -> Self {
        Self {
            owner,
            bytes: [0; TERMINAL_WRITE_CHUNK],
            length: 0,
        }
    }

    fn flush(&mut self) -> core::fmt::Result {
        if self.length == 0 {
            return Ok(());
        }
        if !crate::gui::write_terminal(self.owner, &self.bytes[..self.length]) {
            return Err(core::fmt::Error);
        }
        self.length = 0;
        Ok(())
    }
}

impl core::fmt::Write for TerminalConsoleWriter {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let mut remaining = text.as_bytes();
        while !remaining.is_empty() {
            let available = TERMINAL_WRITE_CHUNK - self.length;
            let count = available.min(remaining.len());
            self.bytes[self.length..self.length + count].copy_from_slice(&remaining[..count]);
            self.length += count;
            remaining = &remaining[count..];
            if self.length == TERMINAL_WRITE_CHUNK {
                self.flush()?;
            }
        }
        Ok(())
    }
}
