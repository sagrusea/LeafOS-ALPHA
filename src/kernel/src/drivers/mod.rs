pub mod acpi;
pub mod apic;
pub mod ata;
pub mod gpu;
pub mod input;
pub mod pic8259;

pub use input::keyboard::{KeyCode, KeyState, Keyboard};
pub use input::{keyboard, mouse};
pub use pic8259::Pic8259;
