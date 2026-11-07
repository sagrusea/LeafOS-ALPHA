use crate::memory;
use crate::sync::{Mutex, Once};
use super::acpi;
use x86::msr::{IA32_APIC_BASE, rdmsr, wrmsr};

const APIC_ENABLE: u64 = 1 << 11;
const LOCAL_APIC_EOI: usize = 0x0b0 / 4;
const LOCAL_APIC_ID: usize = 0x020 / 4;
const LOCAL_APIC_SVR: usize = 0x0f0 / 4;
const LOCAL_APIC_ICR_LOW: usize = 0x300 / 4;
const LOCAL_APIC_ICR_HIGH: usize = 0x310 / 4;
const IO_APIC_REG: usize = 0;
const IO_APIC_DATA: usize = 0x10 / 4;
const IO_APIC_VERSION: u8 = 1;
const IO_APIC_REDIRECTION_BASE: u8 = 0x10;
const IRQ_VECTOR_BASE: u8 = 32;
const ICR_DELIVERY_STATUS: u32 = 1 << 12;
const ICR_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_LEVEL: u32 = 1 << 15;
const ICR_INIT: u32 = 5 << 8;
const ICR_STARTUP: u32 = 6 << 8;
const ICR_LEVEL_DEASSERT: u32 = 0;
const ICR_FIXED: u32 = 0;
const IPI_TIMEOUT: usize = 1_000_000;
pub const RESCHEDULE_VECTOR: u8 = 0xf0;

struct Apic {
    local: *mut u32,
    io: *mut u32,
    io_apic_gsi_base: u32,
    pit_gsi: u32,
    keyboard_gsi: u32,
    mouse_gsi: u32,
}

unsafe impl Send for Apic {}

impl Apic {
    fn new(info: acpi::ApicInfo) -> Option<Self> {
        let local_base = info.local_apic_address;
        let local = memory::map_mmio_page(local_base).ok()?.as_mut_ptr::<u32>();
        let io = memory::map_mmio_page(info.io_apic_address)
            .ok()?
            .as_mut_ptr::<u32>();
        Some(Self {
            local,
            io,
            io_apic_gsi_base: info.io_apic_gsi_base,
            pit_gsi: info.pit_gsi,
            keyboard_gsi: info.keyboard_gsi,
            mouse_gsi: info.mouse_gsi,
        })
    }

    unsafe fn local_write(&mut self, register: usize, value: u32) {
        unsafe { self.local.add(register).write_volatile(value) };
    }

    unsafe fn io_read(&mut self, register: u8) -> u32 {
        unsafe {
            self.io.add(IO_APIC_REG).write_volatile(register as u32);
            self.io.add(IO_APIC_DATA).read_volatile()
        }
    }

    unsafe fn io_write(&mut self, register: u8, value: u32) {
        unsafe {
            self.io.add(IO_APIC_REG).write_volatile(register as u32);
            self.io.add(IO_APIC_DATA).write_volatile(value);
        }
    }

    unsafe fn route_gsi(&mut self, gsi: u32, vector: u8, apic_id: u8) {
        let index = gsi.checked_sub(self.io_apic_gsi_base).unwrap_or(u32::MAX);
        if index > u32::from(u8::MAX / 2) {
            return;
        }
        let low = IO_APIC_REDIRECTION_BASE + index as u8 * 2;
        unsafe {
            self.io_write(low, u32::from(vector));
            self.io_write(low + 1, u32::from(apic_id) << 24);
        }
    }

    unsafe fn initialize(&mut self) {
        let mut base = unsafe { rdmsr(IA32_APIC_BASE) };
        base |= APIC_ENABLE;
        unsafe { wrmsr(IA32_APIC_BASE, base) };
        let apic_id = unsafe { self.local.add(LOCAL_APIC_ID).read_volatile() >> 24 } as u8;
        unsafe {
            self.local_write(LOCAL_APIC_SVR, 0x100 | 0xff);
            let route_count = (self.io_read(IO_APIC_VERSION) >> 16 & 0xff) as u8 + 1;
            for irq in 0..route_count {
                let low = IO_APIC_REDIRECTION_BASE + irq * 2;
                self.io_write(low, 1 << 16);
                self.io_write(low + 1, 0);
            }
            self.route_gsi(self.pit_gsi, IRQ_VECTOR_BASE, apic_id);
            self.route_gsi(self.keyboard_gsi, IRQ_VECTOR_BASE + 1, apic_id);
            self.route_gsi(self.mouse_gsi, IRQ_VECTOR_BASE + 2, apic_id);
        }
    }

    unsafe fn eoi(&mut self) {
        unsafe { self.local_write(LOCAL_APIC_EOI, 0) };
    }

    unsafe fn send_ipi(&mut self, apic_id: u32, command: u32) -> bool {
        for _ in 0..IPI_TIMEOUT {
            if unsafe { self.local.add(LOCAL_APIC_ICR_LOW).read_volatile() }
                & ICR_DELIVERY_STATUS
                == 0
            {
                break;
            }
            core::hint::spin_loop();
        }
        if unsafe { self.local.add(LOCAL_APIC_ICR_LOW).read_volatile() }
            & ICR_DELIVERY_STATUS
            != 0
        {
            return false;
        }
        unsafe {
            self.local_write(LOCAL_APIC_ICR_HIGH, apic_id << 24);
            self.local_write(LOCAL_APIC_ICR_LOW, command);
        }
        for _ in 0..IPI_TIMEOUT {
            if unsafe { self.local.add(LOCAL_APIC_ICR_LOW).read_volatile() }
                & ICR_DELIVERY_STATUS
                == 0
            {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

static APIC: Once<Option<Mutex<Apic>>> = Once::new();

pub fn initialize() -> bool {
    if !x86::cpuid::CpuId::new()
        .get_feature_info()
        .is_some_and(|features| features.has_apic())
    {
        return false;
    }
    APIC.call_once(|| {
        acpi::info().and_then(|info| Apic::new(info).map(|mut apic| {
            unsafe { apic.initialize() };
            Mutex::new(apic)
        }))
    });
    APIC.get().is_some_and(Option::is_some)
}

pub fn is_initialized() -> bool {
    APIC.get().is_some_and(Option::is_some)
}

pub fn initialize_secondary_local() -> bool {
    let Some(Some(apic)) = APIC.get() else {
        return false;
    };
    unsafe {
        let mut base = rdmsr(IA32_APIC_BASE);
        base |= APIC_ENABLE;
        wrmsr(IA32_APIC_BASE, base);
        apic.lock().local_write(LOCAL_APIC_SVR, 0x100 | 0xff);
    }
    true
}

pub fn eoi() {
    if let Some(Some(apic)) = APIC.get() {
        unsafe { apic.lock().eoi() };
    }
}

pub fn send_reschedule_ipi(apic_id: u32) -> bool {
    send_ipi(apic_id, ICR_FIXED | u32::from(RESCHEDULE_VECTOR))
}

pub fn current_apic_id() -> Option<u32> {
    let apic = APIC.get()?.as_ref()?;
    Some(unsafe { apic.lock().local.add(LOCAL_APIC_ID).read_volatile() >> 24 })
}

pub fn send_init_ipi(apic_id: u32) -> bool {
    if !send_ipi(
        apic_id,
        ICR_INIT | ICR_ASSERT | ICR_TRIGGER_LEVEL,
    ) {
        return false;
    }
    crate::timer::sleep_ticks(1);
    send_ipi(
        apic_id,
        ICR_INIT | ICR_LEVEL_DEASSERT | ICR_TRIGGER_LEVEL,
    )
}

pub fn send_startup_ipi(apic_id: u32, vector: u8) -> bool {
    send_ipi(apic_id, ICR_STARTUP | u32::from(vector))
}

fn send_ipi(apic_id: u32, command: u32) -> bool {
    let Some(Some(apic)) = APIC.get() else {
        return false;
    };
    unsafe { apic.lock().send_ipi(apic_id, command) }
}
