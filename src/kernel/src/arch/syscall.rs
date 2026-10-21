use crate::{gdt, memory};
use core::arch::global_asm;
use core::ptr::addr_of_mut;
use x86_64::VirtAddr;

const IA32_KERNEL_GSBASE: u32 = 0xc000_0102;
const IA32_GS_BASE: u32 = 0xc000_0101;
const IA32_EFER: u32 = 0xc000_0080;
const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;
const EFER_SYSCALL_ENABLE: u64 = 1;
const RFLAGS_USER_MASK: u64 = 0x0ad5;
pub const SYS_GET_TICKS: u64 = 0;
pub const SYS_EXIT: u64 = 1;
/// Write a byte buffer to the active console: `rdi=buffer`, `rsi=length`.
pub const SYS_WRITE: u64 = 2;
/// Nonblocking console input: `rdi=destination`, `rsi=capacity`.
pub const SYS_READ: u64 = 3;
/// Read a file from ext2: `rdi=path`, `rsi=path length`, `rdx=destination`,
/// `r10=destination capacity`. The path is a byte span, not NUL-terminated.
pub const SYS_READ_FILE: u64 = 4;
/// Create or replace an ext2 file: `rdi=path`, `rsi=path length`,
/// `rdx=source`, `r10=source length`. Returns zero on success.
pub const SYS_WRITE_FILE: u64 = 5;
/// Voluntarily yield the CPU to another runnable task.
pub const SYS_YIELD: u64 = 6;
/// Sleep for a bounded number of timer ticks: `RDI=ticks`.
pub const SYS_SLEEP: u64 = 7;
/// Spawn an ext2 application: `RDI=path`, `RSI=path length`.
pub const SYS_SPAWN: u64 = 8;
/// Wait for a child: `RDI=child task ID or zero`, `RSI=status output`.
pub const SYS_WAIT: u64 = 9;
/// Spawn an ext2 application with arguments:
/// `RDI=path`, `RSI=path length`, `RDX=argv pointers`, `R10=argc`.
pub const SYS_SPAWN_WITH_ARGUMENTS: u64 = 10;
/// Read from a descriptor: `RDI=fd`, `RSI=buffer`, `RDX=capacity`.
pub const SYS_FD_READ: u64 = 11;
/// Write to a descriptor: `RDI=fd`, `RSI=buffer`, `RDX=length`.
pub const SYS_FD_WRITE: u64 = 12;
/// Create a pipe. Returns read and write descriptors packed into RAX.
pub const SYS_PIPE: u64 = 13;
/// Close a descriptor: `RDI=fd`.
pub const SYS_CLOSE: u64 = 14;
/// Duplicate a descriptor to a target slot: `RDI=source`, `RSI=target`.
pub const SYS_DUP2: u64 = 15;
/// List ext2 directory entries as newline-separated names:
/// `rdi=path`, `rsi=path length`, `rdx=destination`, `r10=capacity`.
pub const SYS_LIST_DIR: u64 = 16;

pub const MAX_SYSCALL_BUFFER: usize = 4096;
pub const MAX_SYSCALL_PATH: usize = 255;
pub const MAX_SYSCALL_SLEEP_TICKS: u64 = 360_000;

/// Errors are returned in RAX as the two's-complement representation of a
/// negative errno, matching the existing `SYSCALL_NOT_IMPLEMENTED` convention.
pub const SYS_ERR_E2BIG: u64 = (-7i64) as u64;
pub const SYS_ERR_EIO: u64 = (-5i64) as u64;
pub const SYS_ERR_EPIPE: u64 = (-32i64) as u64;
pub const SYS_ERR_EAGAIN: u64 = (-11i64) as u64;
pub const SYS_ERR_EFAULT: u64 = (-14i64) as u64;
pub const SYS_ERR_EINVAL: u64 = (-22i64) as u64;
pub const SYS_ERR_ENOENT: u64 = (-2i64) as u64;
pub const SYSCALL_NOT_IMPLEMENTED: u64 = (-38i64) as u64;

fn append_decimal(output: &mut [u8], position: &mut usize, value: u64) -> bool {
    let mut digits = [0; 20];
    let mut count = 0;
    let mut value = value;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if *position + count > output.len() {
        return false;
    }
    for digit in digits[..count].iter().rev() {
        output[*position] = *digit;
        *position += 1;
    }
    true
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
}

#[repr(C)]
struct CpuLocal {
    user_rsp: u64,
    kernel_rsp: u64,
}

#[unsafe(no_mangle)]
static mut LEAFOS_CPU_LOCAL: CpuLocal = CpuLocal {
    user_rsp: 0,
    kernel_rsp: 0,
};

pub(crate) unsafe fn user_rsp() -> u64 {
    unsafe { addr_of_mut!(LEAFOS_CPU_LOCAL.user_rsp).read() }
}

pub(crate) unsafe fn set_user_rsp(rsp: u64) {
    unsafe { addr_of_mut!(LEAFOS_CPU_LOCAL.user_rsp).write(rsp) };
}

unsafe extern "C" {
    fn leafos_syscall_entry();
}

global_asm!(
    r#"
    .global leafos_syscall_entry
    .type leafos_syscall_entry, @function
leafos_syscall_entry:
    swapgs
    mov qword ptr gs:[0], rsp
    mov rsp, qword ptr gs:[8]
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov ax, {kernel_data_selector}
    mov ds, ax
    mov es, ax
    sub rsp, 8
    lea rdi, [rsp + 8]
    call syscall_dispatch
    add rsp, 8

    mov qword ptr [rsp + 112], rax
    mov rcx, qword ptr [rsp + 96]
    mov rax, {user_space_start}
    cmp rcx, rax
    jb syscall_bad_return
    mov rax, {user_space_end}
    cmp rcx, rax
    jae syscall_bad_return
    mov rax, qword ptr gs:[0]
    mov r10, {user_space_start}
    cmp rax, r10
    jb syscall_bad_return
    mov r10, {user_space_end}
    cmp rax, r10
    jae syscall_bad_return
    and qword ptr [rsp + 32], {rflags_user_mask}
    or qword ptr [rsp + 32], 0x202

    mov ax, {user_data_selector}
    mov ds, ax
    mov es, ax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    mov rax, qword ptr [rsp]
    add rsp, 8
    mov rsp, qword ptr gs:[0]
    swapgs
    sysretq

syscall_bad_return:
    sub rsp, 8
    call leafos_invalid_syscall_return
    ud2
    "#,
    user_space_start = const memory::USER_SPACE_START,
    user_space_end = const memory::USER_SPACE_END,
    rflags_user_mask = const RFLAGS_USER_MASK,
    kernel_data_selector = const gdt::KERNEL_DATA_SELECTOR,
    user_data_selector = const gdt::USER_DATA_SELECTOR,
);

#[unsafe(no_mangle)]
extern "C" fn syscall_dispatch(frame: *mut SyscallFrame) -> u64 {
    let frame = unsafe { &*frame };
    match frame.rax {
        SYS_GET_TICKS => crate::timer::ticks(),
        SYS_EXIT => crate::tasking::scheduler::exit_current_with_code(frame.rdi),
        SYS_WRITE => sys_write(frame),
        SYS_READ => sys_read(frame),
        SYS_READ_FILE => sys_read_file(frame),
        SYS_WRITE_FILE => sys_write_file(frame),
        SYS_YIELD => sys_yield(),
        SYS_SLEEP => sys_sleep(frame.rdi),
        SYS_SPAWN => sys_spawn(frame),
        SYS_WAIT => sys_wait(frame),
        SYS_SPAWN_WITH_ARGUMENTS => sys_spawn_with_arguments(frame),
        SYS_FD_READ => sys_fd_read(frame),
        SYS_FD_WRITE => sys_fd_write(frame),
        SYS_PIPE => sys_pipe(frame),
        SYS_CLOSE => sys_close(frame),
        SYS_DUP2 => sys_dup2(frame),
        SYS_LIST_DIR => sys_list_dir(frame),
        _ => SYSCALL_NOT_IMPLEMENTED,
    }
}

fn sys_fd_read(frame: &SyscallFrame) -> u64 {
    if frame.rdi >= crate::tasking::fd::MAX_FDS as u64 || frame.rdx > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_EINVAL;
    }
    let length = frame.rdx as usize;
    if length == 0 {
        return 0;
    }
    if crate::tasking::scheduler::validate_current_user_range(frame.rsi, length, true).is_err() {
        return SYS_ERR_EFAULT;
    }
    let mut buffer = [0u8; MAX_SYSCALL_BUFFER];
    let count = loop {
        match crate::tasking::scheduler::read_current_fd(frame.rdi as usize, &mut buffer[..length])
        {
            Ok(count) => break count,
            Err(crate::tasking::scheduler::SchedulerError::InvalidFd) => return SYS_ERR_EINVAL,
            Err(crate::tasking::scheduler::SchedulerError::WouldBlock) => {
                if frame.rdi as usize == crate::tasking::fd::STDIN {
                    return SYS_ERR_EAGAIN;
                }
                if crate::tasking::scheduler::block_now().is_err() {
                    return SYS_ERR_EIO;
                }
            }
            Err(crate::tasking::scheduler::SchedulerError::BrokenPipe) => return SYS_ERR_EPIPE,
            Err(crate::tasking::scheduler::SchedulerError::Io) => return SYS_ERR_EIO,
            Err(_) => return SYS_ERR_EIO,
        }
    };
    crate::tasking::scheduler::wake_all_blocked_tasks();
    if crate::tasking::scheduler::copy_to_current_user(frame.rsi, &buffer[..count]).is_err() {
        return SYS_ERR_EFAULT;
    }
    count as u64
}

fn sys_fd_write(frame: &SyscallFrame) -> u64 {
    if frame.rdi >= crate::tasking::fd::MAX_FDS as u64 || frame.rdx > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_EINVAL;
    }
    let length = frame.rdx as usize;
    if length == 0 {
        return 0;
    }
    let mut buffer = [0u8; MAX_SYSCALL_BUFFER];
    if crate::tasking::scheduler::copy_from_current_user(frame.rsi, &mut buffer[..length]).is_err()
    {
        return SYS_ERR_EFAULT;
    }
    loop {
        match crate::tasking::scheduler::write_current_fd(frame.rdi as usize, &buffer[..length]) {
            Ok(count) => {
                crate::tasking::scheduler::wake_all_blocked_tasks();
                return count as u64;
            }
            Err(crate::tasking::scheduler::SchedulerError::InvalidFd) => return SYS_ERR_EINVAL,
            Err(crate::tasking::scheduler::SchedulerError::WouldBlock) => {
                if crate::tasking::scheduler::block_now().is_err() {
                    return SYS_ERR_EIO;
                }
            }
            Err(crate::tasking::scheduler::SchedulerError::BrokenPipe) => return SYS_ERR_EPIPE,
            Err(_) => return SYS_ERR_EIO,
        }
    }
}

fn sys_pipe(frame: &SyscallFrame) -> u64 {
    if crate::tasking::scheduler::validate_current_user_range(frame.rdi, 8, true).is_err()
        || crate::tasking::scheduler::validate_current_user_range(frame.rsi, 8, true).is_err()
    {
        return SYS_ERR_EFAULT;
    }
    match crate::tasking::scheduler::create_pipe_for_current() {
        Ok((read_fd, write_fd)) => (read_fd as u64) | ((write_fd as u64) << 32),
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_close(frame: &SyscallFrame) -> u64 {
    if frame.rdi >= crate::tasking::fd::MAX_FDS as u64 {
        return SYS_ERR_EINVAL;
    }
    match crate::tasking::scheduler::close_current_fd(frame.rdi as usize) {
        Ok(()) => {
            crate::tasking::scheduler::wake_all_blocked_tasks();
            0
        }
        Err(crate::tasking::scheduler::SchedulerError::InvalidFd) => SYS_ERR_EINVAL,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_dup2(frame: &SyscallFrame) -> u64 {
    if frame.rdi >= crate::tasking::fd::MAX_FDS as u64
        || frame.rsi >= crate::tasking::fd::MAX_FDS as u64
    {
        return SYS_ERR_EINVAL;
    }
    match crate::tasking::scheduler::duplicate_current_fd(frame.rdi as usize, frame.rsi as usize) {
        Ok(()) => frame.rsi,
        Err(crate::tasking::scheduler::SchedulerError::InvalidFd) => SYS_ERR_EINVAL,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_yield() -> u64 {
    match crate::tasking::scheduler::yield_now() {
        Ok(()) => 0,
        Err(crate::tasking::scheduler::SchedulerError::NoRunnableTasks) => 0,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_sleep(ticks: u64) -> u64 {
    if ticks > MAX_SYSCALL_SLEEP_TICKS {
        return SYS_ERR_E2BIG;
    }
    match crate::tasking::scheduler::sleep_ticks(ticks) {
        Ok(()) => 0,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_spawn(frame: &SyscallFrame) -> u64 {
    if frame.rsi == 0 {
        return SYS_ERR_EINVAL;
    }

    if frame.rsi > MAX_SYSCALL_PATH as u64 {
        return SYS_ERR_E2BIG;
    }
    let path_length = frame.rsi as usize;
    let mut path_buffer = [0u8; MAX_SYSCALL_PATH];
    if crate::tasking::scheduler::copy_from_current_user(frame.rdi, &mut path_buffer[..path_length])
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    let path_bytes = &path_buffer[..path_length];
    if path_bytes.contains(&0) {
        return SYS_ERR_EINVAL;
    }
    let path = match core::str::from_utf8(path_bytes) {
        Ok(path) if !path.is_empty() => path,
        _ => return SYS_ERR_EINVAL,
    };

    match crate::user_program::launch_from_ext2_with_arguments(path, &[]) {
        Ok(task) => task.get(),
        Err(crate::user_program::ProgramError::FileNotFound) => SYS_ERR_ENOENT,
        Err(crate::user_program::ProgramError::InvalidExtension)
        | Err(crate::user_program::ProgramError::InvalidAppFormat)
        | Err(crate::user_program::ProgramError::UnsupportedAppVersion)
        | Err(crate::user_program::ProgramError::InvalidArguments) => SYS_ERR_EINVAL,
        Err(crate::user_program::ProgramError::ImageTooLarge) => SYS_ERR_E2BIG,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_spawn_with_arguments(frame: &SyscallFrame) -> u64 {
    if frame.r10 > crate::user_program::MAX_PROGRAM_ARGUMENTS as u64 {
        return SYS_ERR_E2BIG;
    }
    if frame.r10 != 0
        && crate::tasking::scheduler::validate_current_user_range(
            frame.rdx,
            (frame.r10 as usize).saturating_mul(core::mem::size_of::<u64>()),
            false,
        )
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    if frame.rsi == 0 || frame.rsi > MAX_SYSCALL_PATH as u64 {
        return if frame.rsi == 0 {
            SYS_ERR_EINVAL
        } else {
            SYS_ERR_E2BIG
        };
    }
    let path_length = frame.rsi as usize;
    let mut path_buffer = [0u8; MAX_SYSCALL_PATH];
    if crate::tasking::scheduler::copy_from_current_user(frame.rdi, &mut path_buffer[..path_length])
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    let path_bytes = &path_buffer[..path_length];
    if path_bytes.contains(&0) {
        return SYS_ERR_EINVAL;
    }
    let path = match core::str::from_utf8(path_bytes) {
        Ok(path) if !path.is_empty() => path,
        _ => return SYS_ERR_EINVAL,
    };

    let mut argument_storage =
        [[0u8; MAX_SYSCALL_PATH]; crate::user_program::MAX_PROGRAM_ARGUMENTS];
    let mut argument_lengths = [0usize; crate::user_program::MAX_PROGRAM_ARGUMENTS];
    let mut total_length = 0usize;
    for index in 0..frame.r10 as usize {
        let pointer_address = match frame
            .rdx
            .checked_add((index as u64) * core::mem::size_of::<u64>() as u64)
        {
            Some(address) => address,
            None => return SYS_ERR_EFAULT,
        };
        let mut pointer_bytes = [0u8; core::mem::size_of::<u64>()];
        if crate::tasking::scheduler::copy_from_current_user(pointer_address, &mut pointer_bytes)
            .is_err()
        {
            return SYS_ERR_EFAULT;
        }
        let argument_address = u64::from_le_bytes(pointer_bytes);
        let mut length = 0usize;
        while length < MAX_SYSCALL_PATH {
            let byte_address = match argument_address.checked_add(length as u64) {
                Some(address) => address,
                None => return SYS_ERR_EFAULT,
            };
            if crate::tasking::scheduler::copy_from_current_user(
                byte_address,
                &mut argument_storage[index][length..length + 1],
            )
            .is_err()
            {
                return SYS_ERR_EFAULT;
            }
            if argument_storage[index][length] == 0 {
                break;
            }
            length += 1;
        }
        if length == MAX_SYSCALL_PATH {
            return SYS_ERR_E2BIG;
        }
        if core::str::from_utf8(&argument_storage[index][..length]).is_err() {
            return SYS_ERR_EINVAL;
        }
        total_length = match total_length.checked_add(length + 1) {
            Some(total) if total <= crate::user_program::MAX_PROGRAM_ARGUMENT_BYTES => total,
            _ => return SYS_ERR_E2BIG,
        };
        argument_lengths[index] = length;
    }

    let mut arguments = [""; crate::user_program::MAX_PROGRAM_ARGUMENTS];
    for index in 0..frame.r10 as usize {
        arguments[index] =
            core::str::from_utf8(&argument_storage[index][..argument_lengths[index]])
                .expect("validated UTF-8 argument");
    }
    match crate::user_program::launch_from_ext2_with_arguments(
        path,
        &arguments[..frame.r10 as usize],
    ) {
        Ok(task) => task.get(),
        Err(crate::user_program::ProgramError::FileNotFound) => SYS_ERR_ENOENT,
        Err(crate::user_program::ProgramError::InvalidExtension)
        | Err(crate::user_program::ProgramError::InvalidAppFormat)
        | Err(crate::user_program::ProgramError::UnsupportedAppVersion)
        | Err(crate::user_program::ProgramError::InvalidArguments) => SYS_ERR_EINVAL,
        Err(crate::user_program::ProgramError::ImageTooLarge) => SYS_ERR_E2BIG,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_wait(frame: &SyscallFrame) -> u64 {
    if crate::tasking::scheduler::validate_current_user_range(frame.rsi, 8, true).is_err() {
        return SYS_ERR_EFAULT;
    }
    let child = if frame.rdi == 0 {
        None
    } else {
        Some(crate::scheduler::TaskId::from_raw(frame.rdi))
    };
    match crate::tasking::scheduler::wait_for_child(child) {
        Ok((task, status)) => {
            let status = status.unwrap_or(0);
            if crate::tasking::scheduler::copy_to_current_user(frame.rsi, &status.to_le_bytes())
                .is_err()
            {
                return SYS_ERR_EFAULT;
            }
            task.get()
        }
        Err(crate::tasking::scheduler::SchedulerError::NoChild)
        | Err(crate::tasking::scheduler::SchedulerError::UnknownTask) => SYS_ERR_ENOENT,
        Err(crate::tasking::scheduler::SchedulerError::NotChild) => SYS_ERR_EINVAL,
        Err(crate::tasking::scheduler::SchedulerError::ChildRunning) => SYS_ERR_EIO,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_write(frame: &SyscallFrame) -> u64 {
    if frame.rsi > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_E2BIG;
    }
    let length = frame.rsi as usize;
    if length == 0 {
        return 0;
    }

    // Never format or dereference the user pointer directly. First validate
    // every covered page, then make a bounded copy to kernel-owned storage.
    let mut buffer = [0u8; MAX_SYSCALL_BUFFER];
    if crate::tasking::scheduler::copy_from_current_user(frame.rdi, &mut buffer[..length]).is_err()
    {
        return SYS_ERR_EFAULT;
    }
    crate::console::write_bytes(&buffer[..length]);
    length as u64
}

fn sys_read(frame: &SyscallFrame) -> u64 {
    if frame.rsi > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_E2BIG;
    }
    let capacity = frame.rsi as usize;
    if capacity == 0 {
        return 0;
    }
    if crate::tasking::scheduler::validate_current_user_range(frame.rdi, capacity, true).is_err() {
        return SYS_ERR_EFAULT;
    }

    let mut buffer = [0u8; MAX_SYSCALL_BUFFER];
    let count = crate::drivers::input::read_available(&mut buffer[..capacity]);
    if count == 0 {
        return 0;
    }
    if crate::tasking::scheduler::copy_to_current_user(frame.rdi, &buffer[..count]).is_err() {
        return SYS_ERR_EFAULT;
    }
    count as u64
}

fn sys_read_file(frame: &SyscallFrame) -> u64 {
    if frame.rsi == 0 {
        return SYS_ERR_EINVAL;
    }
    if frame.rsi > MAX_SYSCALL_PATH as u64 || frame.r10 > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_E2BIG;
    }
    let path_length = frame.rsi as usize;
    let capacity = frame.r10 as usize;

    // Check the output before performing disk I/O. This buffer must be writable,
    // not merely somewhere in the process's user virtual-address range.
    if capacity != 0
        && crate::tasking::scheduler::validate_current_user_range(frame.rdx, capacity, true)
            .is_err()
    {
        return SYS_ERR_EFAULT;
    }

    let mut path_buffer = [0u8; MAX_SYSCALL_PATH];
    if crate::tasking::scheduler::copy_from_current_user(frame.rdi, &mut path_buffer[..path_length])
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    let path_bytes = &path_buffer[..path_length];
    if path_bytes.contains(&0) {
        return SYS_ERR_EINVAL;
    }
    let path = match core::str::from_utf8(path_bytes) {
        Ok(path) => path,
        Err(_) => return SYS_ERR_EINVAL,
    };

    let mut buffer = [0u8; MAX_SYSCALL_BUFFER];
    match crate::fs::ext2::read_file(path, &mut buffer[..capacity]) {
        Ok(Some((count, _total))) => {
            if count != 0
                && crate::tasking::scheduler::copy_to_current_user(frame.rdx, &buffer[..count])
                    .is_err()
            {
                return SYS_ERR_EFAULT;
            }
            count as u64
        }
        Ok(None) => SYS_ERR_ENOENT,
        Err(crate::fs::ext2::Ext2Error::InvalidPath) => SYS_ERR_EINVAL,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_write_file(frame: &SyscallFrame) -> u64 {
    if frame.rsi == 0 {
        return SYS_ERR_EINVAL;
    }
    if frame.rsi > MAX_SYSCALL_PATH as u64 || frame.r10 > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_E2BIG;
    }

    let path_length = frame.rsi as usize;
    let contents_length = frame.r10 as usize;
    let mut path_buffer = [0u8; MAX_SYSCALL_PATH];
    if crate::tasking::scheduler::copy_from_current_user(frame.rdi, &mut path_buffer[..path_length])
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    let path_bytes = &path_buffer[..path_length];
    if path_bytes.contains(&0) {
        return SYS_ERR_EINVAL;
    }
    let path = match core::str::from_utf8(path_bytes) {
        Ok(path) if !path.is_empty() => path,
        _ => return SYS_ERR_EINVAL,
    };

    // Copy user data into kernel-owned memory before touching the filesystem.
    // Empty files are valid and do not require the source pointer to be mapped.
    let mut contents = [0u8; MAX_SYSCALL_BUFFER];
    if contents_length != 0
        && crate::tasking::scheduler::copy_from_current_user(
            frame.rdx,
            &mut contents[..contents_length],
        )
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }

    match crate::fs::ext2::write_file(path, &contents[..contents_length]) {
        Ok(()) => 0,
        Err(crate::fs::ext2::Ext2Error::InvalidPath) => SYS_ERR_EINVAL,
        Err(crate::fs::ext2::Ext2Error::NotFound) => SYS_ERR_ENOENT,
        Err(_) => SYS_ERR_EIO,
    }
}

fn sys_list_dir(frame: &SyscallFrame) -> u64 {
    if frame.rsi > MAX_SYSCALL_PATH as u64 || frame.r10 > MAX_SYSCALL_BUFFER as u64 {
        return SYS_ERR_E2BIG;
    }
    let path_length = frame.rsi as usize;
    let capacity = frame.r10 as usize;
    if capacity != 0
        && crate::tasking::scheduler::validate_current_user_range(frame.rdx, capacity, true)
            .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    let mut path_buffer = [0u8; MAX_SYSCALL_PATH];
    if crate::tasking::scheduler::copy_from_current_user(frame.rdi, &mut path_buffer[..path_length])
        .is_err()
    {
        return SYS_ERR_EFAULT;
    }
    let path_bytes = &path_buffer[..path_length];
    if path_bytes.contains(&0) {
        return SYS_ERR_EINVAL;
    }
    let path = match core::str::from_utf8(path_bytes) {
        Ok(path) => path,
        Err(_) => return SYS_ERR_EINVAL,
    };
    let mut output = [0u8; MAX_SYSCALL_BUFFER];
    let mut length = 0;
    let result = crate::fs::ext2::for_each_entry(path, |name, is_dir, size| {
        if length >= capacity {
            return;
        }
        let name_bytes = name.as_bytes();
        let start = length;
        if !append_decimal(&mut output, &mut length, size) || length + 1 > capacity {
            length = start;
            return;
        }
        output[length] = b' ';
        length += 1;
        let required = name_bytes.len() + usize::from(is_dir) + 1;
        if length + required <= capacity {
            output[length..length + name_bytes.len()].copy_from_slice(name_bytes);
            length += name_bytes.len();
            if is_dir {
                output[length] = b'/';
                length += 1;
            }
            output[length] = b'\n';
            length += 1;
        } else {
            length = start;
        }
    });
    match result {
        Ok(()) => {
            if length != 0
                && crate::tasking::scheduler::copy_to_current_user(frame.rdx, &output[..length])
                    .is_err()
            {
                return SYS_ERR_EFAULT;
            }
            length as u64
        }
        Err(crate::fs::ext2::Ext2Error::InvalidPath) => SYS_ERR_EINVAL,
        Err(_) => SYS_ERR_EIO,
    }
}

#[unsafe(no_mangle)]
extern "C" fn leafos_invalid_syscall_return() -> ! {
    crate::serial_println!("[USER FAULT] Invalid syscall return context; terminating task");
    crate::tasking::scheduler::exit_current()
}

/// Install the syscall MSRs and per-CPU ring-3 entry state.
/// Boot does not call this; user-mode execution remains explicitly gated.
///
/// # Safety
///
/// Call with interrupts disabled, after `gdt::init`, and only with a valid
/// kernel stack mapped in every process address space. The scheduler calls
/// this whenever it selects a user task so its TSS and syscall stack are set.
pub unsafe fn initialize(kernel_stack_top: VirtAddr) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let kernel_gs_base = addr_of_mut!(LEAFOS_CPU_LOCAL) as u64;
        let user_code_base = u64::from(gdt::user_code_selector() & !3) - 16;
        let star = (user_code_base << 48) | (u64::from(gdt::KERNEL_CODE_SELECTOR) << 32);
        let lstar = leafos_syscall_entry as *const () as usize as u64;
        unsafe {
            x86::msr::wrmsr(IA32_GS_BASE, kernel_gs_base);
            x86::msr::wrmsr(IA32_KERNEL_GSBASE, 0);
        }
        set_kernel_stack(kernel_stack_top);
        unsafe {
            x86::msr::wrmsr(IA32_EFER, x86::msr::rdmsr(IA32_EFER) | EFER_SYSCALL_ENABLE);
            x86::msr::wrmsr(IA32_STAR, star);
            x86::msr::wrmsr(IA32_LSTAR, lstar);
            x86::msr::wrmsr(IA32_FMASK, 0x74700);
        }
    });
}

pub fn set_kernel_stack(stack_top: VirtAddr) {
    let stack_top = VirtAddr::new(stack_top.as_u64() & !0xf);
    x86_64::instructions::interrupts::without_interrupts(|| {
        unsafe {
            addr_of_mut!(LEAFOS_CPU_LOCAL.kernel_rsp).write(stack_top.as_u64());
        }
        gdt::set_kernel_stack(stack_top);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn syscall_dispatch_returns_ticks_and_rejects_unknown_numbers() {
        let get_ticks = SyscallFrame {
            rax: SYS_GET_TICKS,
            ..SyscallFrame::default()
        };
        assert_eq!(
            syscall_dispatch(&get_ticks as *const SyscallFrame as *mut SyscallFrame),
            crate::timer::ticks()
        );
        let unknown = SyscallFrame {
            rax: u64::MAX,
            ..SyscallFrame::default()
        };
        assert_eq!(
            syscall_dispatch(&unknown as *const SyscallFrame as *mut SyscallFrame),
            SYSCALL_NOT_IMPLEMENTED
        );
    }

    #[test_case]
    fn bounded_write_handles_empty_and_oversized_requests_before_pointer_access() {
        let empty = SyscallFrame {
            rax: SYS_WRITE,
            rdi: 0,
            rsi: 0,
            ..SyscallFrame::default()
        };
        assert_eq!(
            syscall_dispatch(&empty as *const SyscallFrame as *mut SyscallFrame),
            0
        );

        let oversized = SyscallFrame {
            rax: SYS_WRITE,
            rdi: 0,
            rsi: MAX_SYSCALL_BUFFER as u64 + 1,
            ..SyscallFrame::default()
        };
        assert_eq!(
            syscall_dispatch(&oversized as *const SyscallFrame as *mut SyscallFrame),
            SYS_ERR_E2BIG
        );
    }
}
