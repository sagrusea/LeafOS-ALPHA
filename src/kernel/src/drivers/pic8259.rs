use x86_64::instructions::port::Port;

const MASTER_COMMAND_PORT: u16 = 0x20;
const MASTER_DATA_PORT: u16 = 0x21;
const SLAVE_COMMAND_PORT: u16 = 0xA0;
const SLAVE_DATA_PORT: u16 = 0xA1;
const IO_WAIT_PORT: u16 = 0x80;

const ICW1_INITIALIZE: u8 = 0x10;
const ICW1_EXPECT_ICW4: u8 = 0x01;
const ICW4_8086_MODE: u8 = 0x01;
const EOI: u8 = 0x20;
const SLAVE_IRQ_BASE: u8 = 8;
const IRQ_COUNT: u8 = 16;

pub struct Pic8259 {
    master_command: Port<u8>,
    master_data: Port<u8>,
    slave_command: Port<u8>,
    slave_data: Port<u8>,
    master_offset: u8,
    slave_offset: u8,
}

impl Pic8259 {
    pub const unsafe fn new(master_offset: u8, slave_offset: u8) -> Self {
        Self {
            master_command: Port::new(MASTER_COMMAND_PORT),
            master_data: Port::new(MASTER_DATA_PORT),
            slave_command: Port::new(SLAVE_COMMAND_PORT),
            slave_data: Port::new(SLAVE_DATA_PORT),
            master_offset,
            slave_offset,
        }
    }

    pub unsafe fn initialize(&mut self) {
        unsafe {
            self.master_command.write(ICW1_INITIALIZE | ICW1_EXPECT_ICW4);
            io_wait();
            self.slave_command.write(ICW1_INITIALIZE | ICW1_EXPECT_ICW4);
            io_wait();

            self.master_data.write(self.master_offset);
            io_wait();
            self.slave_data.write(self.slave_offset);
            io_wait();

            self.master_data.write(1 << 2);
            io_wait();
            self.slave_data.write(2);
            io_wait();

            self.master_data.write(ICW4_8086_MODE);
            io_wait();
            self.slave_data.write(ICW4_8086_MODE);
            io_wait();

            self.master_data.write(0xFE);
            io_wait();
            self.slave_data.write(0xFF);
            io_wait();
        }
    }

    pub unsafe fn signal_eoi(&mut self, interrupt_id: u8) {
        assert!(
            interrupt_id < IRQ_COUNT,
            "PIC interrupt ID must be an IRQ line from 0 through 15"
        );

        unsafe {
            if interrupt_id >= SLAVE_IRQ_BASE {
                self.slave_command.write(EOI);
                io_wait();
            }

            self.master_command.write(EOI);
            io_wait();
        }
    }
}

unsafe fn io_wait() {
    unsafe {
        Port::<u8>::new(IO_WAIT_PORT).write(0);
    }
}
