use crate::sync::Mutex;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub const STDIN: usize = 0;
pub const STDOUT: usize = 1;
pub const STDERR: usize = 2;
pub const MAX_FDS: usize = 16;
const PIPE_CAPACITY: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    Invalid,
    WouldBlock,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    Invalid,
    WouldBlock,
    BrokenPipe,
    Io,
}

pub struct FileWrite {
    path: [u8; 255],
    path_len: usize,
    contents: Vec<u8>,
}

pub struct FileRead {
    contents: Vec<u8>,
    offset: usize,
}

pub enum FileDescriptor {
    Stdin,
    Console,
    PipeRead(Arc<Mutex<Pipe>>),
    PipeWrite(Arc<Mutex<Pipe>>),
    FileWrite(Arc<Mutex<FileWrite>>),
    FileRead(Arc<Mutex<FileRead>>),
}

impl Clone for FileDescriptor {
    fn clone(&self) -> Self {
        match self {
            Self::Stdin => Self::Stdin,
            Self::Console => Self::Console,
            Self::PipeRead(pipe) => {
                pipe.lock().readers += 1;
                Self::PipeRead(pipe.clone())
            }
            Self::PipeWrite(pipe) => {
                pipe.lock().writers += 1;
                Self::PipeWrite(pipe.clone())
            }
            Self::FileWrite(file) => Self::FileWrite(file.clone()),
            Self::FileRead(file) => Self::FileRead(file.clone()),
        }
    }
}

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        match self {
            Self::PipeRead(pipe) => {
                let mut pipe = pipe.lock();
                pipe.readers = pipe.readers.saturating_sub(1);
            }
            Self::PipeWrite(pipe) => {
                let mut pipe = pipe.lock();
                pipe.writers = pipe.writers.saturating_sub(1);
            }
            Self::Stdin | Self::Console | Self::FileWrite(_) | Self::FileRead(_) => {}
        }
    }
}

pub struct Pipe {
    bytes: [u8; PIPE_CAPACITY],
    head: usize,
    length: usize,
    readers: usize,
    writers: usize,
}

impl Pipe {
    fn new() -> Self {
        Self {
            bytes: [0; PIPE_CAPACITY],
            head: 0,
            length: 0,
            readers: 0,
            writers: 1,
        }
    }

    fn read(&mut self, output: &mut [u8]) -> usize {
        let count = output.len().min(self.length);
        for slot in output.iter_mut().take(count) {
            *slot = self.bytes[self.head];
            self.head = (self.head + 1) % PIPE_CAPACITY;
            self.length -= 1;
        }
        count
    }

    fn write(&mut self, input: &[u8]) -> usize {
        let count = input.len().min(PIPE_CAPACITY - self.length);
        for byte in input.iter().take(count) {
            let tail = (self.head + self.length) % PIPE_CAPACITY;
            self.bytes[tail] = *byte;
            self.length += 1;
        }
        count
    }
}

pub fn initial_table() -> [Option<FileDescriptor>; MAX_FDS] {
    let mut table = [const { None }; MAX_FDS];
    table[STDIN] = Some(FileDescriptor::Stdin);
    table[STDOUT] = Some(FileDescriptor::Console);
    table[STDERR] = Some(FileDescriptor::Console);
    table
}

pub fn create_pipe() -> Option<(FileDescriptor, FileDescriptor)> {
    let pipe = Arc::new(Mutex::new(Pipe::new()));
    let read_end = pipe.clone();
    read_end.lock().readers = 1;
    Some((
        FileDescriptor::PipeRead(read_end),
        FileDescriptor::PipeWrite(pipe),
    ))
}

pub fn create_file_write(path: &str) -> Result<FileDescriptor, ()> {
    let path_bytes = path.as_bytes();
    if path_bytes.is_empty() || path_bytes.len() > 255 {
        return Err(());
    }
    crate::fs::ext2::write_file(path, &[]).map_err(|_| ())?;
    let mut stored_path = [0; 255];
    stored_path[..path_bytes.len()].copy_from_slice(path_bytes);
    Ok(FileDescriptor::FileWrite(Arc::new(Mutex::new(FileWrite {
        path: stored_path,
        path_len: path_bytes.len(),
        contents: Vec::new(),
    }))))
}

pub fn create_file_read(path: &str) -> Result<FileDescriptor, ()> {
    let contents = crate::fs::ext2::read_file_vec(path, 1024 * 1024)
        .map_err(|_| ())?
        .ok_or(())?;
    Ok(FileDescriptor::FileRead(Arc::new(Mutex::new(FileRead {
        contents,
        offset: 0,
    }))))
}

pub fn close(descriptor: FileDescriptor) {
    drop(descriptor);
}

pub fn read(descriptor: &FileDescriptor, output: &mut [u8]) -> Result<usize, ReadError> {
    match descriptor {
        FileDescriptor::Stdin => {
            let count = crate::drivers::input::read_available(output);
            if count == 0 {
                Err(ReadError::WouldBlock)
            } else {
                Ok(count)
            }
        }
        FileDescriptor::PipeRead(pipe) => {
            let mut pipe = pipe.lock();
            if pipe.length == 0 && pipe.writers != 0 {
                Err(ReadError::WouldBlock)
            } else {
                Ok(pipe.read(output))
            }
        }
        FileDescriptor::FileRead(file) => {
            let mut file = file.lock();
            let remaining = &file.contents[file.offset..];
            let count = output.len().min(remaining.len());
            output[..count].copy_from_slice(&remaining[..count]);
            file.offset += count;
            Ok(count)
        }
        FileDescriptor::Console | FileDescriptor::PipeWrite(_) | FileDescriptor::FileWrite(_) => {
            Err(ReadError::Invalid)
        }
    }
}

pub fn write(descriptor: &FileDescriptor, input: &[u8]) -> Result<usize, WriteError> {
    match descriptor {
        FileDescriptor::Console => {
            crate::console::write_bytes(input);
            Ok(input.len())
        }
        FileDescriptor::PipeWrite(pipe) => {
            let mut pipe = pipe.lock();
            if pipe.readers == 0 {
                Err(WriteError::BrokenPipe)
            } else {
                let count = pipe.write(input);
                if count == 0 && !input.is_empty() {
                    Err(WriteError::WouldBlock)
                } else {
                    Ok(count)
                }
            }
        }
        FileDescriptor::FileWrite(file) => {
            let mut file = file.lock();
            file.contents.extend_from_slice(input);
            let path =
                core::str::from_utf8(&file.path[..file.path_len]).map_err(|_| WriteError::Io)?;
            crate::fs::ext2::write_file(path, &file.contents).map_err(|_| WriteError::Io)?;
            Ok(input.len())
        }
        FileDescriptor::Stdin | FileDescriptor::PipeRead(_) | FileDescriptor::FileRead(_) => {
            Err(WriteError::Invalid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn pipe_transfers_bytes_without_blocking() {
        let (reader, writer) = create_pipe().expect("create pipe");
        assert_eq!(read(&reader, &mut [0u8; 3]), Err(ReadError::WouldBlock));
        assert_eq!(write(&writer, b"abc"), Ok(3));
        let mut output = [0u8; 4];
        assert_eq!(read(&reader, &mut output), Ok(3));
        assert_eq!(&output[..3], b"abc");
    }

    #[test_case]
    fn full_pipe_reports_would_block() {
        let (reader, writer) = create_pipe().expect("create pipe");
        let input = [b'x'; PIPE_CAPACITY];
        assert_eq!(write(&writer, &input), Ok(PIPE_CAPACITY));
        assert_eq!(write(&writer, b"x"), Err(WriteError::WouldBlock));
        drop(reader);
    }

    #[test_case]
    fn standard_descriptor_table_has_console_endpoints() {
        let table = initial_table();
        assert!(matches!(table[STDIN], Some(FileDescriptor::Stdin)));
        assert!(matches!(table[STDOUT], Some(FileDescriptor::Console)));
        assert!(matches!(table[STDERR], Some(FileDescriptor::Console)));
    }

    #[test_case]
    fn closing_pipe_endpoints_exposes_eof_and_broken_pipe() {
        let (reader, writer) = create_pipe().expect("create pipe");
        close(reader);
        assert_eq!(write(&writer, b"x"), Err(WriteError::BrokenPipe));

        let (reader, writer) = create_pipe().expect("create pipe");
        close(writer);
        assert_eq!(read(&reader, &mut [0u8; 1]), Ok(0));
    }

    #[test_case]
    fn dropping_cloned_endpoints_updates_pipe_lifetime() {
        let (reader, writer) = create_pipe().expect("create pipe");
        let inherited_writer = writer.clone();
        close(writer);
        assert_eq!(read(&reader, &mut [0u8; 1]), Err(ReadError::WouldBlock));
        drop(inherited_writer);
        assert_eq!(read(&reader, &mut [0u8; 1]), Ok(0));
    }

    #[test_case]
    fn file_read_descriptor_returns_contents_then_eof() {
        let descriptor = FileDescriptor::FileRead(Arc::new(Mutex::new(FileRead {
            contents: b"abc".to_vec(),
            offset: 0,
        })));
        let mut output = [0; 4];
        assert_eq!(read(&descriptor, &mut output), Ok(3));
        assert_eq!(&output[..3], b"abc");
        assert_eq!(read(&descriptor, &mut output), Ok(0));
    }
}
