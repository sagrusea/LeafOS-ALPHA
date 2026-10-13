use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::{gop_print, gop_println};

const INPUT_CAPACITY: usize = 64;
const QUEUE_CAPACITY: usize = 256;
const QUEUE_MASK: usize = QUEUE_CAPACITY - 1;
const PATH_CAPACITY: usize = 255;

static INPUT_QUEUE: [AtomicU8; QUEUE_CAPACITY] = [const { AtomicU8::new(0) }; QUEUE_CAPACITY];
static QUEUE_HEAD: AtomicUsize = AtomicUsize::new(0);
static QUEUE_TAIL: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Help,
    Clear,
    Echo(&'a [u8]),
    Memory,
    ListFiles(&'a [u8]),
    Cat(&'a [u8]),
    ChangeDirectory(&'a [u8]),
    PrintWorkingDirectory,
    Settings,
    SetColor(&'a [u8]),
    SetResolution(&'a [u8]),
    InfoOs,
    InfoHardware,
    InfoSoftware,
    Unknown,
    Empty,
}

fn parse_command(line: &[u8]) -> Command<'_> {
    let line = trim_spaces(line);
    if line.is_empty() {
        return Command::Empty;
    }
    if line == b"help" {
        return Command::Help;
    }
    if line == b"clear" {
        return Command::Clear;
    }
    if line == b"cls" {
        return Command::Clear;
    }
    if line == b"mem" {
        return Command::Memory;
    }
    if line == b"ls" {
        return Command::ListFiles(&[]);
    }
    if line.starts_with(b"ls ") {
        return Command::ListFiles(trim_spaces(&line[3..]));
    }
    if line.starts_with(b"cat ") {
        return Command::Cat(trim_spaces(&line[4..]));
    }
    if line == b"cd" {
        return Command::ChangeDirectory(&[]);
    }
    if line.starts_with(b"cd ") {
        return Command::ChangeDirectory(trim_spaces(&line[3..]));
    }
    if line == b"pwd" {
        return Command::PrintWorkingDirectory;
    }
    if line == b"set" || line == b"settings" {
        return Command::Settings;
    }
    if line.starts_with(b"set col ") {
        return Command::SetColor(trim_spaces(&line[8..]));
    }
    if line.starts_with(b"set res ") {
        return Command::SetResolution(trim_spaces(&line[8..]));
    }
    if line == b"info -os" {
        return Command::InfoOs;
    }
    if line == b"info -hw" {
        return Command::InfoHardware;
    }
    if line == b"info -sw" {
        return Command::InfoSoftware;
    }
    if line == b"echo" {
        return Command::Echo(&[]);
    }
    if line.starts_with(b"echo ") {
        return Command::Echo(trim_spaces(&line[5..]));
    }
    Command::Unknown
}

fn trim_spaces(mut value: &[u8]) -> &[u8] {
    while value.first() == Some(&b' ') {
        value = &value[1..];
    }
    while value.last() == Some(&b' ') {
        value = &value[..value.len() - 1];
    }
    value
}

fn resolve_path(current: &[u8], input: &[u8], output: &mut [u8; PATH_CAPACITY]) -> Option<usize> {
    let mut length = 0;
    if !input.starts_with(b"/") {
        if current.len() > output.len() {
            return None;
        }
        output[..current.len()].copy_from_slice(current);
        length = current.len();
    }

    for component in input.split(|byte| *byte == b'/') {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if let Some(separator) = output[..length].iter().rposition(|byte| *byte == b'/') {
                length = separator;
            } else {
                length = 0;
            }
            continue;
        }
        if component.len() > 100
            || !component
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
        {
            return None;
        }
        let separator_len = usize::from(length != 0);
        let new_length = length
            .checked_add(separator_len)?
            .checked_add(component.len())?;
        if new_length > output.len() {
            return None;
        }
        if separator_len != 0 {
            output[length] = b'/';
            length += 1;
        }
        output[length..length + component.len()].copy_from_slice(component);
        length += component.len();
    }
    Some(length)
}

fn resolve_directory_path(
    current: &[u8],
    requested: &[u8],
    output: &mut [u8; PATH_CAPACITY],
) -> Option<usize> {
    resolve_path(
        current,
        if requested.is_empty() {
            b"."
        } else {
            requested
        },
        output,
    )
}

fn parse_color(value: &[u8]) -> Option<(u8, u8, u8)> {
    match value {
        b"default" | b"green" => Some((0, 255, 0)),
        b"white" => Some((255, 255, 255)),
        b"red" => Some((255, 64, 64)),
        b"yellow" => Some((255, 255, 0)),
        b"cyan" => Some((0, 255, 255)),
        _ if value.len() == 7 && value[0] == b'#' => {
            let mut rgb = [0; 3];
            for (index, component) in rgb.iter_mut().enumerate() {
                let high = hex_digit(value[1 + index * 2])?;
                let low = hex_digit(value[2 + index * 2])?;
                *component = high * 16 + low;
            }
            Some((rgb[0], rgb[1], rgb[2]))
        }
        _ => None,
    }
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn valid_resolution(value: &[u8]) -> bool {
    let Some(separator) = value.iter().position(|byte| *byte == b':') else {
        return false;
    };
    let (width, height_with_separator) = value.split_at(separator);
    let height = &height_with_separator[1..];
    !width.is_empty()
        && !height.is_empty()
        && width.iter().all(u8::is_ascii_digit)
        && height.iter().all(u8::is_ascii_digit)
        && width.iter().any(|digit| *digit != b'0')
        && height.iter().any(|digit| *digit != b'0')
}

pub fn enqueue_key(byte: u8) {
    let head = QUEUE_HEAD.load(Ordering::Relaxed);
    let next = (head + 1) & QUEUE_MASK;
    if next == QUEUE_TAIL.load(Ordering::Acquire) {
        return;
    }
    INPUT_QUEUE[head].store(byte, Ordering::Relaxed);
    QUEUE_HEAD.store(next, Ordering::Release);
}

fn dequeue_key() -> Option<u8> {
    let tail = QUEUE_TAIL.load(Ordering::Relaxed);
    if tail == QUEUE_HEAD.load(Ordering::Acquire) {
        return None;
    }
    let byte = INPUT_QUEUE[tail].load(Ordering::Relaxed);
    QUEUE_TAIL.store((tail + 1) & QUEUE_MASK, Ordering::Release);
    Some(byte)
}

fn execute(line: &[u8], current_dir: &mut [u8; PATH_CAPACITY], current_dir_len: &mut usize) {
    match parse_command(line) {
        Command::Help => {
            gop_println!("Commands: help, cls, echo, mem, ls, cat, cd, pwd, set, info")
        }
        Command::Clear => crate::framebuffer::clear(),
        Command::Echo(text) => {
            if let Ok(text) = core::str::from_utf8(text) {
                gop_println!("{}", text);
            }
        }
        Command::Memory => gop_println!(
            "Heap mapped: {} KiB",
            crate::memory::heap::mapped_bytes() / 1024
        ),
        Command::ListFiles(path) => {
            let mut resolved = [0; PATH_CAPACITY];
            if let Some(length) =
                resolve_directory_path(&current_dir[..*current_dir_len], path, &mut resolved)
            {
                match core::str::from_utf8(&resolved[..length]) {
                    Ok(directory) => match crate::fs::directory_exists(directory) {
                        Ok(true) => {
                            match crate::fs::for_each_entry(directory, |name, is_dir, size| {
                                if is_dir {
                                    gop_println!("       {}/", name);
                                } else {
                                    gop_println!("{:>6}  {}", size, name);
                                }
                            }) {
                                Ok(()) => {}
                                Err(error) => gop_println!("Filesystem error: {:?}", error),
                            }
                        }
                        Ok(false) => gop_println!("Directory not found: {}", directory),
                        Err(error) => gop_println!("Filesystem error: {:?}", error),
                    },
                    Err(_) => gop_println!("Invalid path."),
                }
            } else {
                gop_println!("Invalid path.");
            }
        }
        Command::Cat(name) => match core::str::from_utf8(name) {
            Ok(name) if !name.is_empty() => {
                let mut resolved = [0; PATH_CAPACITY];
                match resolve_path(
                    &current_dir[..*current_dir_len],
                    name.as_bytes(),
                    &mut resolved,
                )
                .and_then(|length| core::str::from_utf8(&resolved[..length]).ok())
                {
                    Some(name) => match crate::fs::read_file(name) {
                        Ok(Some(contents)) => {
                            let truncated = contents.len() > 2048;
                            let contents = &contents[..contents.len().min(2048)];
                            if let Ok(text) = core::str::from_utf8(contents) {
                                gop_print!("{}", text);
                                if !text.ends_with('\n') {
                                    gop_println!();
                                }
                            } else {
                                gop_println!("File is not UTF-8 text.");
                            }
                            if truncated {
                                gop_println!("[output truncated at 2048 bytes]");
                            }
                        }
                        Ok(None) => gop_println!("File not found: {}", name),
                        Err(error) => gop_println!("Filesystem error: {:?}", error),
                    },
                    None => gop_println!("Invalid path."),
                }
            }
            _ => gop_println!("Usage: cat <filename>"),
        },
        Command::ChangeDirectory(path) => {
            let mut resolved = [0; PATH_CAPACITY];
            let target = if path.is_empty() {
                Some(0)
            } else {
                resolve_path(&current_dir[..*current_dir_len], path, &mut resolved)
            };
            if let Some(length) = target {
                match core::str::from_utf8(&resolved[..length]) {
                    Ok(directory) => match crate::fs::directory_exists(directory) {
                        Ok(true) => {
                            current_dir[..length].copy_from_slice(&resolved[..length]);
                            *current_dir_len = length;
                        }
                        Ok(false) => gop_println!("Directory not found: /{}", directory),
                        Err(error) => gop_println!("Filesystem error: {:?}", error),
                    },
                    Err(_) => gop_println!("Invalid path."),
                }
            } else {
                gop_println!("Invalid path.");
            }
        }
        Command::PrintWorkingDirectory => {
            match core::str::from_utf8(&current_dir[..*current_dir_len]) {
                Ok(path) => gop_println!("/{}", path),
                Err(_) => gop_println!("Invalid current directory."),
            }
        }
        Command::Settings => gop_println!("set col <name|#RRGGBB> | set res <width>:<height>"),
        Command::SetColor(value) => match parse_color(value) {
            Some((red, green, blue)) => {
                crate::framebuffer::set_color(red, green, blue);
                gop_println!("Text color updated.");
            }
            None => gop_println!("Use green, white, red, yellow, cyan, default, or #RRGGBB."),
        },
        Command::SetResolution(value) => {
            if valid_resolution(value) {
                gop_println!("Resolution is selected by UEFI at boot and cannot be changed yet.");
            } else {
                gop_println!("Usage: set res <width>:<height>");
            }
        }
        Command::InfoOs => gop_println!("LeafOS Alpha 2A2 | x86_64 UEFI kernel"),
        Command::InfoHardware => {
            let cpuid = x86::cpuid::CpuId::new();
            let vendor_info = cpuid.get_vendor_info();
            let vendor = vendor_info
                .as_ref()
                .map(|info| info.as_str())
                .unwrap_or("unknown");
            let brand_info = cpuid.get_processor_brand_string();
            let brand = brand_info
                .as_ref()
                .map(|info| info.as_str())
                .unwrap_or("unknown CPU");
            let physical_bits = cpuid
                .get_processor_capacity_feature_info()
                .map(|info| info.physical_address_bits())
                .unwrap_or(0);
            gop_println!("CPU: {}", brand);
            gop_println!(
                "Vendor: {} | physical address bits: {}",
                vendor,
                physical_bits
            );
        }
        Command::InfoSoftware => {
            gop_println!("Kernel: LeafOS Alpha 2A2");
            gop_println!("Shell: built-in command shell");
            gop_println!("Heap: linked-list allocator with demand mapping");
        }
        Command::Unknown => gop_println!("Unknown command. Type 'help'."),
        Command::Empty => {}
    }
}

fn print_prompt(current_dir: &[u8]) {
    match core::str::from_utf8(current_dir) {
        Ok(path) if path.is_empty() => gop_print!("leafos> "),
        Ok(path) => gop_print!("leafos:/{}> ", path),
        Err(_) => gop_print!("leafos> "),
    }
}

pub fn run() -> ! {
    let mut line = [0; INPUT_CAPACITY];
    let mut length = 0;
    let mut current_dir = [0; PATH_CAPACITY];
    let mut current_dir_len = 0;
    gop_println!("LeafOS shell. Type 'help' for commands.");
    print_prompt(&current_dir[..current_dir_len]);

    loop {
        match dequeue_key() {
            Some(b'\n') => {
                gop_println!();
                execute(&line[..length], &mut current_dir, &mut current_dir_len);
                line = [0; INPUT_CAPACITY];
                length = 0;
                print_prompt(&current_dir[..current_dir_len]);
            }
            Some(0x08) if length != 0 => {
                length -= 1;
                line[length] = 0;
                crate::framebuffer::backspace();
            }
            Some(0x03) => {
                length = 0;
                line = [0; INPUT_CAPACITY];
                gop_println!("^C");
                print_prompt(&current_dir[..current_dir_len]);
            }
            Some(byte @ 0x20..=0x7e) if length < INPUT_CAPACITY => {
                line[length] = byte;
                length += 1;
                gop_print!("{}", byte as char);
            }
            Some(_) => {}
            None => x86_64::instructions::interrupts::enable_and_hlt(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn parses_basic_commands() {
        assert_eq!(parse_command(b"help"), Command::Help);
        assert_eq!(parse_command(b"  clear "), Command::Clear);
        assert_eq!(parse_command(b"cls"), Command::Clear);
        assert_eq!(
            parse_command(b"echo hello shell"),
            Command::Echo(b"hello shell")
        );
        assert_eq!(parse_command(b"mem"), Command::Memory);
        assert_eq!(parse_command(b"ls"), Command::ListFiles(b""));
        assert_eq!(parse_command(b"ls docs"), Command::ListFiles(b"docs"));
        assert_eq!(
            parse_command(b"cat README.TXT"),
            Command::Cat(b"README.TXT")
        );
        assert_eq!(parse_command(b"cd docs"), Command::ChangeDirectory(b"docs"));
        assert_eq!(parse_command(b"cd"), Command::ChangeDirectory(b""));
        assert_eq!(parse_command(b"pwd"), Command::PrintWorkingDirectory);
        assert_eq!(parse_command(b"set col cyan"), Command::SetColor(b"cyan"));
        assert_eq!(
            parse_command(b"set res 1024:768"),
            Command::SetResolution(b"1024:768")
        );
        assert_eq!(parse_command(b"info -hw"), Command::InfoHardware);
        assert_eq!(parse_command(b""), Command::Empty);
        assert_eq!(parse_command(b"reboot"), Command::Unknown);
    }

    #[test_case]
    fn keyboard_queue_preserves_input_order() {
        enqueue_key(b'x');
        enqueue_key(b'\n');
        assert_eq!(dequeue_key(), Some(b'x'));
        assert_eq!(dequeue_key(), Some(b'\n'));
        assert_eq!(dequeue_key(), None);
    }

    #[test_case]
    fn validates_colors_and_resolution_syntax() {
        assert_eq!(parse_color(b"#12aBef"), Some((0x12, 0xAB, 0xEF)));
        assert_eq!(parse_color(b"#gg0000"), None);
        assert!(valid_resolution(b"1920:1080"));
        assert!(!valid_resolution(b"0:600"));
        assert!(!valid_resolution(b"1920x1080"));
    }

    #[test_case]
    fn resolves_relative_and_rooted_paths_without_escaping_root() {
        let mut output = [0; PATH_CAPACITY];
        let length = resolve_path(b"docs", b"../README.TXT", &mut output).unwrap();
        assert_eq!(&output[..length], b"README.TXT");

        let length = resolve_path(b"docs", b"/docs/ABOUT.TXT", &mut output).unwrap();
        assert_eq!(&output[..length], b"docs/ABOUT.TXT");

        let length = resolve_path(b"", b"../../docs//./ABOUT.TXT", &mut output).unwrap();
        assert_eq!(&output[..length], b"docs/ABOUT.TXT");

        assert!(resolve_path(b"", b"docs/bad name", &mut output).is_none());
    }

    #[test_case]
    fn bare_ls_keeps_the_current_directory() {
        let mut output = [0; PATH_CAPACITY];
        let length = resolve_directory_path(b"docs", b"", &mut output).unwrap();
        assert_eq!(&output[..length], b"docs");
    }
}
