use spin::Mutex;
use x86_64::instructions::{interrupts, port::Port};

const DATA: u16 = 0x1f0;
const ERROR_FEATURES: u16 = 0x1f1;
const SECTOR_COUNT: u16 = 0x1f2;
const LBA_LOW: u16 = 0x1f3;
const LBA_MID: u16 = 0x1f4;
const LBA_HIGH: u16 = 0x1f5;
const DRIVE_HEAD: u16 = 0x1f6;
const STATUS_COMMAND: u16 = 0x1f7;
const ALT_STATUS_CONTROL: u16 = 0x3f6;

const STATUS_ERROR: u8 = 1;
const STATUS_DEVICE_FAULT: u8 = 1 << 5;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_BUSY: u8 = 1 << 7;
const READ_SECTORS: u8 = 0x20;
const WRITE_SECTORS: u8 = 0x30;
const CACHE_FLUSH: u8 = 0xe7;
const MAX_LBA28: u32 = 0x0fff_ffff;
const WAIT_LIMIT: usize = 1_000_000;

static CHANNEL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtaError {
    InvalidRequest,
    NoDevice,
    Timeout,
    Device,
}

fn wait_not_busy(status: &mut Port<u8>) -> Result<u8, AtaError> {
    for _ in 0..WAIT_LIMIT {
        let value = unsafe { status.read() };
        if value == 0 || value == 0xff {
            return Err(AtaError::NoDevice);
        }
        if value & STATUS_BUSY == 0 {
            if value & (STATUS_ERROR | STATUS_DEVICE_FAULT) != 0 {
                return Err(AtaError::Device);
            }
            return Ok(value);
        }
        core::hint::spin_loop();
    }
    Err(AtaError::Timeout)
}

fn delay_400ns(alt_status: &mut Port<u8>) {
    for _ in 0..4 {
        let _ = unsafe { alt_status.read() };
    }
}

fn read_sectors_locked(lba: u32, count: u8, output: &mut [u8]) -> Result<(), AtaError> {
    validate_request(lba, count, output.len())?;

    let mut data = Port::<u16>::new(DATA);
    let mut error = Port::<u8>::new(ERROR_FEATURES);
    let mut sector_count = Port::<u8>::new(SECTOR_COUNT);
    let mut lba_low = Port::<u8>::new(LBA_LOW);
    let mut lba_mid = Port::<u8>::new(LBA_MID);
    let mut lba_high = Port::<u8>::new(LBA_HIGH);
    let mut drive_head = Port::<u8>::new(DRIVE_HEAD);
    let mut status_command = Port::<u8>::new(STATUS_COMMAND);
    let mut alt_status = Port::<u8>::new(ALT_STATUS_CONTROL);

    unsafe {
        drive_head.write(0xf0 | ((lba >> 24) as u8 & 0x0f));
    }
    delay_400ns(&mut alt_status);
    wait_not_busy(&mut status_command)?;

    unsafe {
        error.write(0);
        sector_count.write(count);
        lba_low.write(lba as u8);
        lba_mid.write((lba >> 8) as u8);
        lba_high.write((lba >> 16) as u8);
        status_command.write(READ_SECTORS);
    }

    for sector in output.chunks_exact_mut(512) {
        delay_400ns(&mut alt_status);
        let status = wait_not_busy(&mut status_command)?;
        if status & STATUS_DRQ == 0 {
            return Err(AtaError::Device);
        }
        for word_bytes in sector.chunks_exact_mut(2) {
            let word = unsafe { data.read() }.to_le_bytes();
            word_bytes.copy_from_slice(&word);
        }
    }

    delay_400ns(&mut alt_status);
    wait_not_busy(&mut status_command)?;
    Ok(())
}

fn validate_request(lba: u32, count: u8, buffer_len: usize) -> Result<(), AtaError> {
    let expected_len = usize::from(count)
        .checked_mul(512)
        .ok_or(AtaError::InvalidRequest)?;
    if count == 0
        || buffer_len != expected_len
        || lba > MAX_LBA28
        || lba
            .checked_add(u32::from(count) - 1)
            .is_none_or(|last| last > MAX_LBA28)
    {
        return Err(AtaError::InvalidRequest);
    }
    Ok(())
}

fn write_sectors_locked(lba: u32, count: u8, input: &[u8]) -> Result<(), AtaError> {
    validate_request(lba, count, input.len())?;

    let mut data = Port::<u16>::new(DATA);
    let mut error = Port::<u8>::new(ERROR_FEATURES);
    let mut sector_count = Port::<u8>::new(SECTOR_COUNT);
    let mut lba_low = Port::<u8>::new(LBA_LOW);
    let mut lba_mid = Port::<u8>::new(LBA_MID);
    let mut lba_high = Port::<u8>::new(LBA_HIGH);
    let mut drive_head = Port::<u8>::new(DRIVE_HEAD);
    let mut status_command = Port::<u8>::new(STATUS_COMMAND);
    let mut alt_status = Port::<u8>::new(ALT_STATUS_CONTROL);

    unsafe {
        drive_head.write(0xf0 | ((lba >> 24) as u8 & 0x0f));
    }
    delay_400ns(&mut alt_status);
    wait_not_busy(&mut status_command)?;

    unsafe {
        error.write(0);
        sector_count.write(count);
        lba_low.write(lba as u8);
        lba_mid.write((lba >> 8) as u8);
        lba_high.write((lba >> 16) as u8);
        status_command.write(WRITE_SECTORS);
    }

    for sector in input.chunks_exact(512) {
        delay_400ns(&mut alt_status);
        let status = wait_not_busy(&mut status_command)?;
        if status & STATUS_DRQ == 0 {
            return Err(AtaError::Device);
        }
        for word_bytes in sector.chunks_exact(2) {
            let word = u16::from_le_bytes([word_bytes[0], word_bytes[1]]);
            unsafe { data.write(word) };
        }
    }

    delay_400ns(&mut alt_status);
    wait_not_busy(&mut status_command)?;
    unsafe { status_command.write(CACHE_FLUSH) };
    delay_400ns(&mut alt_status);
    wait_not_busy(&mut status_command)?;
    Ok(())
}

pub fn read_sectors(lba: u32, count: u8, output: &mut [u8]) -> Result<(), AtaError> {
    interrupts::without_interrupts(|| {
        let _guard = CHANNEL_LOCK.lock();
        read_sectors_locked(lba, count, output)
    })
}

pub fn write_sectors(lba: u32, count: u8, input: &[u8]) -> Result<(), AtaError> {
    interrupts::without_interrupts(|| {
        let _guard = CHANNEL_LOCK.lock();
        write_sectors_locked(lba, count, input)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn rejects_invalid_sector_requests_before_port_access() {
        assert_eq!(read_sectors(0, 0, &mut []), Err(AtaError::InvalidRequest));
        assert_eq!(
            read_sectors(0, 1, &mut [0; 511]),
            Err(AtaError::InvalidRequest)
        );
        assert_eq!(
            read_sectors(MAX_LBA28 + 1, 1, &mut [0; 512]),
            Err(AtaError::InvalidRequest)
        );
        assert_eq!(
            read_sectors(MAX_LBA28, 2, &mut [0; 1024]),
            Err(AtaError::InvalidRequest)
        );
    }

    #[test_case]
    fn validates_writes_without_touching_ports() {
        assert_eq!(validate_request(0, 1, 511), Err(AtaError::InvalidRequest));
        assert_eq!(
            validate_request(MAX_LBA28, 2, 1024),
            Err(AtaError::InvalidRequest)
        );
        assert_eq!(validate_request(1, 2, 1024), Ok(()));
    }
}
