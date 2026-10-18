pub mod framebuffer;
pub mod serial;
pub mod vga_buffer;

/// Write a bounded byte buffer to the active text console. Use the framebuffer
/// for interactive boots and serial as a fallback for headless/test boots.
pub fn write_bytes(bytes: &[u8]) {
    if framebuffer::is_initialized() {
        framebuffer::write_bytes(bytes);
    } else {
        serial::write_bytes(bytes);
    }
}
