use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::{gop_print, gop_println};

const INPUT_CAPACITY: usize = 256;
const QUEUE_CAPACITY: usize = 256;
const QUEUE_MASK: usize = QUEUE_CAPACITY - 1;
const PATH_CAPACITY: usize = 255;
const HISTORY_CAPACITY: usize = 8;
const MAX_SLEEP_SECONDS: u64 = 3_600;
const KEY_LEFT: u8 = 0x80;
const KEY_RIGHT: u8 = 0x81;
const KEY_UP: u8 = 0x82;
const KEY_DOWN: u8 = 0x83;
const KEY_HOME: u8 = 0x84;
const KEY_END: u8 = 0x85;
const KEY_DELETE: u8 = 0x86;
const EDITOR_MAX_BYTES: usize = 12 * 1024;

static INPUT_QUEUE: [AtomicU8; QUEUE_CAPACITY] = [const { AtomicU8::new(0) }; QUEUE_CAPACITY];
static QUEUE_HEAD: AtomicUsize = AtomicUsize::new(0);
static QUEUE_TAIL: AtomicUsize = AtomicUsize::new(0);

struct LineEditor {
    bytes: [u8; INPUT_CAPACITY],
    length: usize,
    cursor: usize,
}

impl LineEditor {
    const fn new() -> Self {
        Self {
            bytes: [0; INPUT_CAPACITY],
            length: 0,
            cursor: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    fn replace(&mut self, bytes: &[u8]) {
        self.length = bytes.len().min(INPUT_CAPACITY);
        self.bytes[..self.length].copy_from_slice(&bytes[..self.length]);
        self.cursor = self.length;
    }

    fn insert(&mut self, byte: u8) -> bool {
        if self.length == INPUT_CAPACITY {
            return false;
        }
        self.bytes
            .copy_within(self.cursor..self.length, self.cursor + 1);
        self.bytes[self.cursor] = byte;
        self.cursor += 1;
        self.length += 1;
        true
    }

    fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.bytes
            .copy_within(self.cursor + 1..self.length, self.cursor);
        self.length -= 1;
        true
    }

    fn delete(&mut self) -> bool {
        if self.cursor == self.length {
            return false;
        }
        self.bytes
            .copy_within(self.cursor + 1..self.length, self.cursor);
        self.length -= 1;
        true
    }

    fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            false
        } else {
            self.cursor -= 1;
            true
        }
    }

    fn move_right(&mut self) -> bool {
        if self.cursor == self.length {
            false
        } else {
            self.cursor += 1;
            true
        }
    }

    fn clear(&mut self) {
        self.length = 0;
        self.cursor = 0;
    }
}

struct CommandHistory {
    entries: [[u8; INPUT_CAPACITY]; HISTORY_CAPACITY],
    lengths: [usize; HISTORY_CAPACITY],
    next: usize,
    count: usize,
    browsing: Option<usize>,
    draft: [u8; INPUT_CAPACITY],
    draft_len: usize,
}

impl CommandHistory {
    const fn new() -> Self {
        Self {
            entries: [[0; INPUT_CAPACITY]; HISTORY_CAPACITY],
            lengths: [0; HISTORY_CAPACITY],
            next: 0,
            count: 0,
            browsing: None,
            draft: [0; INPUT_CAPACITY],
            draft_len: 0,
        }
    }

    fn record(&mut self, line: &[u8]) {
        if line.is_empty() {
            self.browsing = None;
            return;
        }
        if self.count > 0 {
            let latest = (self.next + HISTORY_CAPACITY - 1) % HISTORY_CAPACITY;
            if &self.entries[latest][..self.lengths[latest]] == line {
                self.browsing = None;
                return;
            }
        }
        self.entries[self.next][..line.len()].copy_from_slice(line);
        self.lengths[self.next] = line.len();
        self.next = (self.next + 1) % HISTORY_CAPACITY;
        self.count = (self.count + 1).min(HISTORY_CAPACITY);
        self.browsing = None;
    }

    fn abandon_browsing(&mut self) {
        self.browsing = None;
    }

    fn older(&mut self, editor: &mut LineEditor) -> bool {
        if self.count == 0 {
            return false;
        }
        let age = match self.browsing {
            Some(age) => (age + 1).min(self.count - 1),
            None => {
                self.draft_len = editor.length;
                self.draft[..self.draft_len].copy_from_slice(editor.as_bytes());
                0
            }
        };
        self.browsing = Some(age);
        let index = (self.next + HISTORY_CAPACITY - 1 - age) % HISTORY_CAPACITY;
        editor.replace(&self.entries[index][..self.lengths[index]]);
        true
    }

    fn newer(&mut self, editor: &mut LineEditor) -> bool {
        let Some(age) = self.browsing else {
            return false;
        };
        if age == 0 {
            self.browsing = None;
            editor.replace(&self.draft[..self.draft_len]);
        } else {
            self.browsing = Some(age - 1);
            let index = (self.next + HISTORY_CAPACITY - age) % HISTORY_CAPACITY;
            editor.replace(&self.entries[index][..self.lengths[index]]);
        }
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> {
    Help,
    Clear,
    Echo(&'a [u8]),
    Memory,
    Uptime,
    Sleep(&'a [u8]),
    ListFiles(&'a [u8]),
    Cat(&'a [u8]),
    DiskList(&'a [u8]),
    DiskCat(&'a [u8]),
    DiskWrite(&'a [u8], &'a [u8]),
    MakeDirectory(&'a [u8]),
    RemoveFile(&'a [u8]),
    RemoveDirectory(&'a [u8]),
    Edit(&'a [u8]),
    ChangeDirectory(&'a [u8]),
    PrintWorkingDirectory,
    Settings,
    SetColor(&'a [u8]),
    SetResolution(&'a [u8]),
    InfoOs,
    InfoHardware,
    InfoSoftware,
    Shutdown,
    Unknown,
    Empty,
}

fn parse_command(line: &[u8]) -> Command<'_> {
    let line = trim_spaces(line);
    if line.is_empty() {
        return Command::Empty;
    }
    if line.eq_ignore_ascii_case(b"help") {
        return Command::Help;
    }
    if line.eq_ignore_ascii_case(b"shutdown") || line.eq_ignore_ascii_case(b"sd") {
        return Command::Shutdown;
    }
    if line.eq_ignore_ascii_case(b"clear")
        || line.eq_ignore_ascii_case(b"cls")
        || line.eq_ignore_ascii_case(b"clr")
    {
        return Command::Clear;
    }
    if line.eq_ignore_ascii_case(b"mem") {
        return Command::Memory;
    }
    if line.eq_ignore_ascii_case(b"uptime") {
        return Command::Uptime;
    }
    if line.eq_ignore_ascii_case(b"sleep") {
        return Command::Sleep(&[]);
    }
    if let Some(seconds) = strip_command_arguments(line, b"sleep") {
        return Command::Sleep(seconds);
    }
    if line.eq_ignore_ascii_case(b"ls") || line.eq_ignore_ascii_case(b"dir") {
        return Command::ListFiles(&[]);
    }
    if let Some(path) = strip_command_arguments(line, b"ls") {
        return Command::ListFiles(path);
    }
    if let Some(path) = strip_command_arguments(line, b"dir") {
        return Command::ListFiles(path);
    }
    if let Some(path) = strip_command_arguments(line, b"cat") {
        return Command::Cat(path);
    }
    if line.eq_ignore_ascii_case(b"diskls") {
        return Command::DiskList(&[]);
    }
    if let Some(path) = strip_command_arguments(line, b"diskls") {
        return Command::DiskList(path);
    }
    if let Some(path) = strip_command_arguments(line, b"diskcat") {
        return Command::DiskCat(path);
    }
    if let Some(arguments) = strip_command_arguments(line, b"diskwrite") {
        if let Some(separator) = arguments.iter().position(|byte| is_ascii_whitespace(*byte)) {
            let name = trim_spaces(&arguments[..separator]);
            let contents = trim_spaces(&arguments[separator + 1..]);
            if !name.is_empty() {
                return Command::DiskWrite(name, contents);
            }
        } else if !arguments.is_empty() {
            return Command::DiskWrite(arguments, &[]);
        }
    }
    if let Some(name) = strip_command_arguments(line, b"mkdir") {
        return Command::MakeDirectory(name);
    }
    if let Some(name) = strip_command_arguments(line, b"rm") {
        return Command::RemoveFile(name);
    }
    if let Some(name) = strip_command_arguments(line, b"rmdir") {
        return Command::RemoveDirectory(name);
    }
    if let Some(name) = strip_command_arguments(line, b"edit")
        && !name.is_empty()
    {
        return Command::Edit(name);
    }
    if line.eq_ignore_ascii_case(b"cd") {
        return Command::ChangeDirectory(&[]);
    }
    if let Some(path) = strip_command_arguments(line, b"cd") {
        return Command::ChangeDirectory(path);
    }
    if line.eq_ignore_ascii_case(b"pwd") {
        return Command::PrintWorkingDirectory;
    }
    if line.eq_ignore_ascii_case(b"set") || line.eq_ignore_ascii_case(b"settings") {
        return Command::Settings;
    }
    if let Some(value) = strip_command_arguments(line, b"set") {
        let (target, payload) = split_first_word(value);
        if target.eq_ignore_ascii_case(b"col") {
            return Command::SetColor(trim_spaces(payload));
        }
        if target.eq_ignore_ascii_case(b"res") {
            return Command::SetResolution(trim_spaces(payload));
        }
        return Command::Settings;
    }
    if let Some(value) = strip_command_arguments(line, b"info") {
        let (target, _) = split_first_word(value);
        if target.eq_ignore_ascii_case(b"-os") {
            return Command::InfoOs;
        }
        if target.eq_ignore_ascii_case(b"-hw") {
            return Command::InfoHardware;
        }
        if target.eq_ignore_ascii_case(b"-sw") {
            return Command::InfoSoftware;
        }
    }
    if line.eq_ignore_ascii_case(b"echo") {
        return Command::Echo(&[]);
    }
    if let Some(message) = strip_command_arguments(line, b"echo") {
        return Command::Echo(message);
    }
    Command::Unknown
}

fn strip_command_arguments<'a>(input: &'a [u8], command: &[u8]) -> Option<&'a [u8]> {
    let remainder = input.get(command.len()..)?;
    if !input[..command.len()].eq_ignore_ascii_case(command)
        || !remainder
            .first()
            .is_some_and(|byte| is_ascii_whitespace(*byte))
    {
        return None;
    }
    Some(trim_spaces(remainder))
}

fn parse_sleep_seconds(value: &[u8]) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    value.iter().try_fold(0u64, |seconds, byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        seconds
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))
    })
}

fn is_ascii_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

fn split_first_word(value: &[u8]) -> (&[u8], &[u8]) {
    let separator = value
        .iter()
        .position(|byte| is_ascii_whitespace(*byte))
        .unwrap_or(value.len());
    let word = &value[..separator];
    let rest = if separator < value.len() {
        trim_spaces(&value[separator..])
    } else {
        &[]
    };
    (word, rest)
}

fn trim_spaces(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(byte) if matches!(*byte, b' ' | b'\t' | b'\n' | b'\r')) {
        value = &value[1..];
    }
    while matches!(value.last(), Some(byte) if matches!(*byte, b' ' | b'\t' | b'\n' | b'\r')) {
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

fn resolve_ext2_command_path(
    current: &[u8],
    input: &[u8],
    current_dir_is_ext2: bool,
    output: &mut [u8; PATH_CAPACITY],
) -> Option<usize> {
    if current_dir_is_ext2 {
        resolve_path(current, input, output)
    } else if input.is_empty() || input.len() > output.len() {
        None
    } else {
        output[..input.len()].copy_from_slice(input);
        Some(input.len())
    }
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

fn redraw_command_line(editor: &LineEditor) {
    crate::framebuffer::redraw_input_line(editor.as_bytes(), editor.cursor);
}

struct TextEditor {
    bytes: Vec<u8>,
    cursor: usize,
    dirty: bool,
}

impl TextEditor {
    fn insert(&mut self, byte: u8) -> bool {
        if self.bytes.len() >= EDITOR_MAX_BYTES {
            return false;
        }
        self.bytes.insert(self.cursor, byte);
        self.cursor += 1;
        self.dirty = true;
        true
    }

    fn previous_boundary(&self) -> usize {
        let text = core::str::from_utf8(&self.bytes).expect("editor buffer stays valid UTF-8");
        let mut position = self.cursor.saturating_sub(1);
        while !text.is_char_boundary(position) {
            position -= 1;
        }
        position
    }

    fn next_boundary(&self) -> usize {
        let text = core::str::from_utf8(&self.bytes).expect("editor buffer stays valid UTF-8");
        let mut position = (self.cursor + 1).min(self.bytes.len());
        while !text.is_char_boundary(position) {
            position += 1;
        }
        position
    }

    fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let previous = self.previous_boundary();
        self.bytes.drain(previous..self.cursor);
        self.cursor = previous;
        self.dirty = true;
        true
    }

    fn delete(&mut self) -> bool {
        if self.cursor == self.bytes.len() {
            return false;
        }
        let next = self.next_boundary();
        self.bytes.drain(self.cursor..next);
        self.dirty = true;
        true
    }

    fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            false
        } else {
            self.cursor = self.previous_boundary();
            true
        }
    }

    fn move_right(&mut self) -> bool {
        if self.cursor == self.bytes.len() {
            false
        } else {
            self.cursor = self.next_boundary();
            true
        }
    }

    fn move_line_boundary(&mut self, to_end: bool) -> bool {
        let target = if to_end {
            self.bytes[self.cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(self.bytes.len(), |offset| self.cursor + offset)
        } else {
            self.bytes[..self.cursor]
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |offset| offset + 1)
        };
        let changed = target != self.cursor;
        self.cursor = target;
        changed
    }

    fn move_vertical(&mut self, down: bool) -> bool {
        let text = core::str::from_utf8(&self.bytes).expect("editor buffer stays valid UTF-8");
        let Some(target) = vertical_cursor_target(text, self.cursor, down) else {
            return false;
        };
        let changed = target != self.cursor;
        self.cursor = target;
        changed
    }
}

fn vertical_cursor_target(text: &str, cursor: usize, down: bool) -> Option<usize> {
    let column = text[..cursor]
        .chars()
        .rev()
        .take_while(|character| *character != '\n')
        .count();
    let current_start = text[..cursor].rfind('\n').map_or(0, |offset| offset + 1);
    let target_start = if down {
        text[cursor..].find('\n').map(|offset| cursor + offset + 1)
    } else if current_start > 0 {
        text[..current_start - 1]
            .rfind('\n')
            .map_or(Some(0), |offset| Some(offset + 1))
    } else {
        None
    }?;
    let target_end = text[target_start..]
        .find('\n')
        .map_or(text.len(), |offset| target_start + offset);
    Some(
        text[target_start..target_end]
            .char_indices()
            .nth(column)
            .map_or(target_end, |(offset, _)| target_start + offset),
    )
}

fn render_text_editor(name: &str, editor: &TextEditor) {
    let text = core::str::from_utf8(&editor.bytes).expect("editor buffer stays valid UTF-8");
    crate::framebuffer::render_editor(name, text, editor.cursor, editor.dirty);
}

fn edit_disk_file(name: &str) {
    if name.is_empty()
        || name.len() > 255
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || name == "."
        || name == ".."
    {
        gop_println!("Editor supports safe root-level filenames only.");
        return;
    }
    let mut bytes = Vec::new();
    bytes.resize(EDITOR_MAX_BYTES, 0);
    let copied = match crate::fs::ext2::read_file(name, &mut bytes) {
        Ok(Some((copied, total))) if total <= EDITOR_MAX_BYTES as u64 => copied,
        Ok(Some(_)) => {
            gop_println!("Editor supports files up to {} bytes.", EDITOR_MAX_BYTES);
            return;
        }
        Ok(None) => 0,
        Err(error) => {
            gop_println!("ext2 read error: {:?}", error);
            return;
        }
    };
    bytes.truncate(copied);
    if core::str::from_utf8(&bytes).is_err() {
        gop_println!("Editor only supports UTF-8 text files.");
        return;
    }
    let mut editor = TextEditor {
        bytes,
        cursor: copied,
        dirty: false,
    };
    render_text_editor(name, &editor);

    loop {
        let Some(key) = dequeue_key() else {
            x86_64::instructions::interrupts::enable_and_hlt();
            continue;
        };
        let changed = match key {
            0x13 => {
                match crate::fs::ext2::write_file(name, &editor.bytes) {
                    Ok(()) => {
                        crate::framebuffer::finish_editor();
                        gop_println!("\nSaved /{} ({} bytes).", name, editor.bytes.len());
                        return;
                    }
                    Err(error) => {
                        crate::framebuffer::finish_editor();
                        gop_println!("\next2 write error: {:?}", error);
                        render_text_editor(name, &editor);
                    }
                }
                continue;
            }
            0x11 | 0x1b => {
                if editor.dirty {
                    crate::framebuffer::finish_editor();
                    gop_println!(
                        "\nUnsaved changes. Press Y to discard, any other key to continue."
                    );
                    let confirmation = loop {
                        if let Some(byte) = dequeue_key() {
                            break byte;
                        }
                        x86_64::instructions::interrupts::enable_and_hlt();
                    };
                    let discard = confirmation.eq_ignore_ascii_case(&b'y');
                    if discard {
                        crate::framebuffer::finish_editor();
                        gop_println!("Changes discarded.");
                        return;
                    }
                    render_text_editor(name, &editor);
                    continue;
                }
                crate::framebuffer::finish_editor();
                gop_println!("\nEditor closed without changes.");
                return;
            }
            b'\n' => editor.insert(b'\n'),
            0x08 => editor.backspace(),
            KEY_LEFT => editor.move_left(),
            KEY_RIGHT => editor.move_right(),
            KEY_UP => editor.move_vertical(false),
            KEY_DOWN => editor.move_vertical(true),
            KEY_HOME => editor.move_line_boundary(false),
            KEY_END => editor.move_line_boundary(true),
            KEY_DELETE => editor.delete(),
            byte @ 0x20..=0x7e => editor.insert(byte),
            _ => false,
        };
        if changed {
            render_text_editor(name, &editor);
        } else if editor.bytes.len() == EDITOR_MAX_BYTES
            && (key == b'\n' || (0x20..=0x7e).contains(&key))
        {
            crate::framebuffer::finish_editor();
            gop_println!("\nFile is at the {} byte editor limit.", EDITOR_MAX_BYTES);
            render_text_editor(name, &editor);
        }
    }
}

fn execute(
    line: &[u8],
    current_dir: &mut [u8; PATH_CAPACITY],
    current_dir_len: &mut usize,
    current_dir_is_ext2: &mut bool,
) {
    match parse_command(line) {
        Command::Help => {
            gop_println!("Common commands:");
            gop_println!("  help                     Show this command list");
            gop_println!("  cls / clear / clr        Clear the screen");
            gop_println!("  echo <text>              Print text to the terminal");
            gop_println!("  mem                     Show memory usage");
            gop_println!("  uptime                  Show time since kernel startup");
            gop_println!("  sleep <seconds>          Wait up to one hour");
            gop_println!("  ls [path] / dir [path]   List files in a directory");
            gop_println!("  cat <path>               Read a text file");
            gop_println!("  cd [path]                Change directories");
            gop_println!("  pwd                     Print the current directory");
            gop_println!("  diskls [path]            List files on the ext2 data disk");
            gop_println!("  diskcat <path>           Read a file from the ext2 data disk");
            gop_println!("  diskwrite <file> [text]  Write a text file to the ext2 disk");
            gop_println!("  mkdir <path>             Create an ext2 directory");
            gop_println!("  rm <path>                Remove an ext2 file");
            gop_println!("  rmdir <path>             Remove an empty ext2 directory");
            gop_println!("  edit <file>              Edit a UTF-8 text file on the ext2 disk");
            gop_println!("  Up/Down                  Browse command history");
            gop_println!("  Left/Right, Home/End     Edit the command line");
            gop_println!("  set col <color>          Change the terminal foreground color");
            gop_println!("  set res <w>:<h>          Check the requested resolution");
            gop_println!("  info -os / -hw / -sw    Show system details");
            gop_println!(
                "  shutdown / sd            Power off the system - DO NOT USE (halts cpu)"
            );
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
        Command::Uptime => {
            let (days, hours, minutes, seconds) =
                crate::timer::split_uptime(crate::timer::uptime_seconds());
            gop_println!(
                "Uptime: {}d {:02}:{:02}:{:02}",
                days,
                hours,
                minutes,
                seconds
            );
        }
        Command::Sleep(value) => match parse_sleep_seconds(value) {
            Some(seconds) if seconds <= MAX_SLEEP_SECONDS => {
                gop_println!("Sleeping for {} second(s)...", seconds);
                crate::timer::sleep_ticks(seconds * crate::timer::TICKS_PER_SECOND);
                gop_println!("Awake.");
            }
            _ => gop_println!("Usage: sleep <seconds> (0-3600)"),
        },
        Command::ListFiles(path) => {
            let mut resolved = [0; PATH_CAPACITY];
            if let Some(length) =
                resolve_directory_path(&current_dir[..*current_dir_len], path, &mut resolved)
            {
                match core::str::from_utf8(&resolved[..length]) {
                    Ok(directory) if *current_dir_is_ext2 => {
                        match crate::fs::ext2::directory_exists(directory) {
                            Ok(true) => {
                                match crate::fs::ext2::for_each_entry(
                                    directory,
                                    |name, is_dir, size| {
                                        if is_dir {
                                            gop_println!("       {}/", name);
                                        } else {
                                            gop_println!("{:>6}  {}", size, name);
                                        }
                                    },
                                ) {
                                    Ok(()) => {}
                                    Err(error) => gop_println!("ext2 error: {:?}", error),
                                }
                            }
                            Ok(false) => gop_println!("Directory not found: {}", directory),
                            Err(error) => gop_println!("ext2 error: {:?}", error),
                        }
                    }
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
                    Some(name) if *current_dir_is_ext2 => {
                        let mut contents = [0; 2048];
                        match crate::fs::ext2::read_file(name, &mut contents) {
                            Ok(Some((copied, total))) => {
                                if let Ok(text) = core::str::from_utf8(&contents[..copied]) {
                                    gop_print!("{}", text);
                                    if !text.ends_with('\n') {
                                        gop_println!();
                                    }
                                } else {
                                    gop_println!("File is not UTF-8 text.");
                                }
                                if total > copied as u64 {
                                    gop_println!("[output truncated at {} bytes]", copied);
                                }
                            }
                            Ok(None) => gop_println!("File not found: {}", name),
                            Err(error) => gop_println!("ext2 error: {:?}", error),
                        }
                    }
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
        Command::DiskList(path) => match core::str::from_utf8(path) {
            Ok(path) => match crate::fs::ext2::directory_exists(path) {
                Ok(true) => match crate::fs::ext2::for_each_entry(path, |name, is_dir, size| {
                    if is_dir {
                        gop_println!("       {}/", name);
                    } else {
                        gop_println!("{:>6}  {}", size, name);
                    }
                }) {
                    Ok(()) => {}
                    Err(error) => gop_println!("ext2 error: {:?}", error),
                },
                Ok(false) => gop_println!("Disk directory not found: /{}", path),
                Err(error) => gop_println!("ext2 error: {:?}", error),
            },
            Err(_) => gop_println!("Usage: diskls [path]"),
        },
        Command::DiskCat(path) => match core::str::from_utf8(path) {
            Ok(path) if !path.is_empty() => {
                let mut contents = [0; 2048];
                match crate::fs::ext2::read_file(path, &mut contents) {
                    Ok(Some((copied, total))) => {
                        if let Ok(text) = core::str::from_utf8(&contents[..copied]) {
                            gop_print!("{}", text);
                            if !text.ends_with('\n') {
                                gop_println!();
                            }
                        } else {
                            gop_println!("File is not UTF-8 text.");
                        }
                        if total > copied as u64 {
                            gop_println!("[output truncated at {} bytes]", copied);
                        }
                    }
                    Ok(None) => gop_println!("Disk file not found: {}", path),
                    Err(error) => gop_println!("ext2 error: {:?}", error),
                }
            }
            _ => gop_println!("Usage: diskcat <path>"),
        },
        Command::DiskWrite(name, contents) => match core::str::from_utf8(name) {
            Ok(name) => match crate::fs::ext2::write_file(name, contents) {
                Ok(()) => gop_println!("Wrote {} bytes to /{}.", contents.len(), name),
                Err(error) => gop_println!("ext2 write error: {:?}", error),
            },
            Err(_) => gop_println!("Usage: diskwrite <name> <text>"),
        },
        Command::MakeDirectory(name) => match core::str::from_utf8(name) {
            Ok(name) => {
                let mut resolved = [0; PATH_CAPACITY];
                match resolve_ext2_command_path(
                    &current_dir[..*current_dir_len],
                    name.as_bytes(),
                    *current_dir_is_ext2,
                    &mut resolved,
                )
                .and_then(|length| core::str::from_utf8(&resolved[..length]).ok())
                {
                    Some(path) => match crate::fs::ext2::create_directory(path) {
                        Ok(()) => gop_println!("Created /{}.", path),
                        Err(error) => gop_println!("ext2 mkdir error: {:?}", error),
                    },
                    None => gop_println!("Usage: mkdir <path>"),
                }
            }
            Err(_) => gop_println!("Usage: mkdir <name>"),
        },
        Command::RemoveFile(name) => match core::str::from_utf8(name) {
            Ok(name) => {
                let mut resolved = [0; PATH_CAPACITY];
                match resolve_ext2_command_path(
                    &current_dir[..*current_dir_len],
                    name.as_bytes(),
                    *current_dir_is_ext2,
                    &mut resolved,
                )
                .and_then(|length| core::str::from_utf8(&resolved[..length]).ok())
                {
                    Some(path) => match crate::fs::ext2::remove_file(path) {
                        Ok(()) => gop_println!("Removed /{}.", path),
                        Err(error) => gop_println!("ext2 rm error: {:?}", error),
                    },
                    None => gop_println!("Usage: rm <path>"),
                }
            }
            Err(_) => gop_println!("Usage: rm <name>"),
        },
        Command::RemoveDirectory(name) => match core::str::from_utf8(name) {
            Ok(name) => {
                let mut resolved = [0; PATH_CAPACITY];
                match resolve_ext2_command_path(
                    &current_dir[..*current_dir_len],
                    name.as_bytes(),
                    *current_dir_is_ext2,
                    &mut resolved,
                )
                .and_then(|length| core::str::from_utf8(&resolved[..length]).ok())
                {
                    Some(path) => match crate::fs::ext2::remove_empty_directory(path) {
                        Ok(()) => gop_println!("Removed empty directory /{}.", path),
                        Err(error) => gop_println!("ext2 rmdir error: {:?}", error),
                    },
                    None => gop_println!("Usage: rmdir <path>"),
                }
            }
            Err(_) => gop_println!("Usage: rmdir <name>"),
        },
        Command::Edit(name) => match core::str::from_utf8(name) {
            Ok(name) => edit_disk_file(name),
            Err(_) => gop_println!("Usage: edit <filename>"),
        },
        Command::ChangeDirectory(path) => {
            let mut resolved = [0; PATH_CAPACITY];
            let target = if path.is_empty() || path == b"/" {
                Some(0)
            } else {
                resolve_path(&current_dir[..*current_dir_len], path, &mut resolved)
            };
            if let Some(length) = target {
                match core::str::from_utf8(&resolved[..length]) {
                    Ok(_) if path.is_empty() || path == b"/" => {
                        *current_dir_is_ext2 = false;
                        *current_dir_len = 0;
                    }
                    Ok(directory) if *current_dir_is_ext2 => {
                        match crate::fs::ext2::directory_exists(directory) {
                            Ok(true) => {
                                current_dir[..length].copy_from_slice(&resolved[..length]);
                                *current_dir_len = length;
                            }
                            Ok(false) => gop_println!("Directory not found: /{}", directory),
                            Err(error) => gop_println!("ext2 error: {:?}", error),
                        }
                    }
                    Ok(directory) => match crate::fs::directory_exists(directory) {
                        Ok(true) => {
                            current_dir[..length].copy_from_slice(&resolved[..length]);
                            *current_dir_len = length;
                            *current_dir_is_ext2 = false;
                        }
                        Ok(false) => match crate::fs::ext2::directory_exists(directory) {
                            Ok(true) => {
                                current_dir[..length].copy_from_slice(&resolved[..length]);
                                *current_dir_len = length;
                                *current_dir_is_ext2 = true;
                            }
                            Ok(false) => gop_println!("Directory not found: /{}", directory),
                            Err(error) => gop_println!("ext2 error: {:?}", error),
                        },
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
                Ok(path) if *current_dir_is_ext2 => gop_println!("ext2:/{}", path),
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
        Command::InfoOs => gop_println!("LeafOS Alpha 2A4 | x86_64 UEFI kernel"),
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
            gop_println!("Kernel: LeafOS Alpha 2A4");
            gop_println!("Shell: built-in command shell");
            gop_println!("Heap: linked-list allocator with demand mapping");
        }
        Command::Shutdown => {
            gop_println!("Shutting down...");
            crate::shutdown();
        }
        Command::Unknown => gop_println!("Unknown command. Type 'help'."),
        Command::Empty => {}
    }
}

fn print_prompt(current_dir: &[u8], current_dir_is_ext2: bool) {
    match core::str::from_utf8(current_dir) {
        Ok(path) if path.is_empty() && current_dir_is_ext2 => gop_print!("leafos:ext2:/> "),
        Ok(path) if path.is_empty() => gop_print!("leafos:/> "),
        Ok(path) if current_dir_is_ext2 => gop_print!("leafos:ext2:/{}> ", path),
        Ok(path) => gop_print!("leafos:/{}> ", path),
        Err(_) => gop_print!("leafos:/> "),
    }
    crate::framebuffer::begin_input_line();
}

pub fn run() -> ! {
    let mut line = LineEditor::new();
    let mut history = CommandHistory::new();
    let mut current_dir = [0; PATH_CAPACITY];
    let mut current_dir_len = 0;
    let mut current_dir_is_ext2 = false;
    gop_println!("LeafOS shell. Type 'help' for commands.");
    print_prompt(&current_dir[..current_dir_len], current_dir_is_ext2);

    loop {
        match dequeue_key() {
            Some(b'\n' | b'\r') => {
                line.cursor = line.length;
                redraw_command_line(&line);
                crate::framebuffer::finish_input_line();
                gop_println!();
                history.record(line.as_bytes());
                execute(
                    line.as_bytes(),
                    &mut current_dir,
                    &mut current_dir_len,
                    &mut current_dir_is_ext2,
                );
                line.clear();
                print_prompt(&current_dir[..current_dir_len], current_dir_is_ext2);
            }
            Some(0x08 | 0x7f) => {
                if line.backspace() {
                    history.abandon_browsing();
                    redraw_command_line(&line);
                }
            }
            Some(0x03) => {
                line.clear();
                redraw_command_line(&line);
                crate::framebuffer::finish_input_line();
                gop_println!("^C");
                print_prompt(&current_dir[..current_dir_len], current_dir_is_ext2);
            }
            Some(b'\t') => {
                if line.insert(b' ') {
                    history.abandon_browsing();
                    redraw_command_line(&line);
                }
            }
            Some(byte @ 0x20..=0x7e) => {
                if line.insert(byte) {
                    history.abandon_browsing();
                    redraw_command_line(&line);
                }
            }
            Some(KEY_LEFT) => {
                if line.move_left() {
                    redraw_command_line(&line);
                }
            }
            Some(KEY_RIGHT) => {
                if line.move_right() {
                    redraw_command_line(&line);
                }
            }
            Some(KEY_HOME) => {
                line.cursor = 0;
                redraw_command_line(&line);
            }
            Some(KEY_END) => {
                line.cursor = line.length;
                redraw_command_line(&line);
            }
            Some(KEY_DELETE) => {
                if line.delete() {
                    history.abandon_browsing();
                    redraw_command_line(&line);
                }
            }
            Some(KEY_UP) => {
                if history.older(&mut line) {
                    redraw_command_line(&line);
                }
            }
            Some(KEY_DOWN) => {
                if history.newer(&mut line) {
                    redraw_command_line(&line);
                }
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
        assert_eq!(parse_command(b"clr"), Command::Clear);
        assert_eq!(parse_command(b"cls"), Command::Clear);
        assert_eq!(
            parse_command(b"echo hello shell"),
            Command::Echo(b"hello shell")
        );
        assert_eq!(parse_command(b"mem"), Command::Memory);
        assert_eq!(parse_command(b"uptime"), Command::Uptime);
        assert_eq!(parse_command(b"sleep 15"), Command::Sleep(b"15"));
        assert_eq!(parse_sleep_seconds(b"0"), Some(0));
        assert_eq!(parse_sleep_seconds(b"3600"), Some(3600));
        assert_eq!(parse_sleep_seconds(b"3601"), Some(3601));
        assert_eq!(parse_sleep_seconds(b"-1"), None);
        assert_eq!(parse_sleep_seconds(b"1s"), None);
        assert_eq!(parse_sleep_seconds(b"18446744073709551616"), None);
        assert_eq!(parse_command(b"ls"), Command::ListFiles(b""));
        assert_eq!(parse_command(b"dir docs"), Command::ListFiles(b"docs"));
        assert_eq!(parse_command(b"ls docs"), Command::ListFiles(b"docs"));
        assert_eq!(parse_command(b"diskls"), Command::DiskList(b""));
        assert_eq!(parse_command(b"diskls docs"), Command::DiskList(b"docs"));
        assert_eq!(
            parse_command(b"diskcat docs/ABOUT.TXT"),
            Command::DiskCat(b"docs/ABOUT.TXT")
        );
        assert_eq!(
            parse_command(b"diskwrite NOTE.TXT hello world"),
            Command::DiskWrite(b"NOTE.TXT", b"hello world")
        );
        assert_eq!(
            parse_command(b"diskwrite EMPTY.TXT"),
            Command::DiskWrite(b"EMPTY.TXT", b"")
        );
        assert_eq!(
            parse_command(b"mkdir NOTES"),
            Command::MakeDirectory(b"NOTES")
        );
        assert_eq!(
            parse_command(b"rm NOTES.TXT"),
            Command::RemoveFile(b"NOTES.TXT")
        );
        assert_eq!(
            parse_command(b"rmdir NOTES"),
            Command::RemoveDirectory(b"NOTES")
        );
        assert_eq!(
            parse_command(b"edit NOTES.TXT"),
            Command::Edit(b"NOTES.TXT")
        );
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
        assert_eq!(parse_command(b"shutdown"), Command::Shutdown);
        assert_eq!(parse_command(b"sd"), Command::Shutdown);
        assert_eq!(parse_command(b"SD"), Command::Shutdown);
        assert_eq!(
            parse_command(b"\t  cd\t docs\r \n"),
            Command::ChangeDirectory(b"docs")
        );
        assert_eq!(parse_command(b""), Command::Empty);
        assert_eq!(parse_command(b"reboot"), Command::Unknown);
    }

    #[test_case]
    fn parses_long_write_command_lines() {
        let mut line = [b'x'; 240];
        let prefix = b"diskwrite LONG.TXT ";
        line[..prefix.len()].copy_from_slice(prefix);
        assert_eq!(
            parse_command(&line),
            Command::DiskWrite(b"LONG.TXT", &line[prefix.len()..])
        );
    }

    #[test_case]
    fn line_editor_inserts_and_deletes_at_the_cursor() {
        let mut line = LineEditor::new();
        line.insert(b'a');
        line.insert(b'c');
        line.move_left();
        line.insert(b'b');
        assert_eq!(line.as_bytes(), b"abc");
        line.delete();
        assert_eq!(line.as_bytes(), b"ab");
        line.backspace();
        assert_eq!(line.as_bytes(), b"a");
    }

    #[test_case]
    fn text_editor_vertical_movement_keeps_unicode_columns() {
        let text = "ééx\ny";
        let cursor = text.len();
        let above = vertical_cursor_target(text, cursor, false).unwrap();
        assert_eq!(above, "é".len());
        assert_eq!(vertical_cursor_target(text, above, true), Some(cursor));
    }

    #[test_case]
    fn command_history_restores_the_unsubmitted_draft() {
        let mut history = CommandHistory::new();
        let mut line = LineEditor::new();
        history.record(b"help");
        history.record(b"ls docs");
        line.replace(b"draft");

        assert!(history.older(&mut line));
        assert_eq!(line.as_bytes(), b"ls docs");
        assert!(history.older(&mut line));
        assert_eq!(line.as_bytes(), b"help");
        assert!(history.newer(&mut line));
        assert_eq!(line.as_bytes(), b"ls docs");
        assert!(history.newer(&mut line));
        assert_eq!(line.as_bytes(), b"draft");
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

    #[test_case]
    fn cd_enters_ext2_directories_and_bare_cd_returns_to_initrd_root() {
        crate::fs::ext2::create_directory("CDTEST").expect("create ext2 test directory");
        let mut current_dir = [0; PATH_CAPACITY];
        let mut current_dir_len = 0;
        let mut current_dir_is_ext2 = false;

        execute(
            b"cd CDTEST",
            &mut current_dir,
            &mut current_dir_len,
            &mut current_dir_is_ext2,
        );
        assert!(current_dir_is_ext2);
        assert_eq!(&current_dir[..current_dir_len], b"CDTEST");

        execute(
            b"ls",
            &mut current_dir,
            &mut current_dir_len,
            &mut current_dir_is_ext2,
        );

        execute(
            b"cd /",
            &mut current_dir,
            &mut current_dir_len,
            &mut current_dir_is_ext2,
        );
        assert!(!current_dir_is_ext2);
        assert_eq!(current_dir_len, 0);

        execute(
            b"cd CDTEST",
            &mut current_dir,
            &mut current_dir_len,
            &mut current_dir_is_ext2,
        );
        assert!(current_dir_is_ext2);
        execute(
            b"cd",
            &mut current_dir,
            &mut current_dir_len,
            &mut current_dir_is_ext2,
        );
        assert!(!current_dir_is_ext2);
        crate::fs::ext2::remove_empty_directory("CDTEST").expect("remove ext2 test directory");
    }

    #[test_case]
    fn ext2_commands_resolve_names_relative_to_current_ext2_directory() {
        let mut output = [0; PATH_CAPACITY];
        let length = resolve_ext2_command_path(b"parent", b"child", true, &mut output).unwrap();
        assert_eq!(&output[..length], b"parent/child");

        let length =
            resolve_ext2_command_path(b"parent", b"../sibling", true, &mut output).unwrap();
        assert_eq!(&output[..length], b"sibling");

        let length =
            resolve_ext2_command_path(b"docs", b"parent/child", false, &mut output).unwrap();
        assert_eq!(&output[..length], b"parent/child");
    }
}
