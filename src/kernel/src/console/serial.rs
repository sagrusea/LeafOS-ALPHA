use crate::sync::Lazy;
use crate::sync::Mutex;
use uart_16550::{Config, Uart16550Tty, backend::PioBackend};

pub static SERIAL1: Lazy<Mutex<Uart16550Tty<PioBackend>>> = Lazy::new(|| {
    Mutex::new(unsafe {
        Uart16550Tty::new_port(0x3F8, Config::default()).expect("failed to initialize UART")
    })
});

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL1
            .lock()
            .write_fmt(args)
            .expect("Printing to serial failed");
    });
}

/// Write a byte buffer to the serial console. The console is text-oriented;
/// non-ASCII bytes are shown as `?` rather than being expanded as UTF-8.
pub fn write_bytes(bytes: &[u8]) {
    use core::fmt::Write;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut serial = SERIAL1.lock();
        for byte in bytes.iter().copied() {
            let character = if byte.is_ascii() {
                char::from(byte)
            } else {
                '?'
            };
            serial
                .write_char(character)
                .expect("Printing to serial failed");
        }
    });
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}
