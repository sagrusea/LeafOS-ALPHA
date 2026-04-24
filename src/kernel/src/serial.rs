use uart_16550::{Config, Uart16550Tty, backend::PioBackend};
use spin::Mutex;
use spin::lazy::Lazy;

pub static SERIAL1: Lazy<Mutex<Uart16550Tty<PioBackend>>> = Lazy::new(|| Mutex::new(unsafe {
    Uart16550Tty::new_port(0x3F8, Config::default())
        .expect("failed to initialize UART")
}));

#[doc(hidden)]
pub fn _print(args: ::core::fmt::Arguments) {
    use core::fmt::Write;
    SERIAL1.lock().write_fmt(args).expect("Printing to serial failed");
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