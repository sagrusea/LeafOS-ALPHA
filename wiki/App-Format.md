# LeafOS application format v1

LeafOS `.app` files are fixed-address x86_64 user programs loaded from the
read-only initrd. Version 1 has a 48-byte little-endian header followed by
code bytes and then initialized data bytes. BSS is allocated and zero-filled
by the kernel; it is not stored in the file.

| Offset | Size | Field | Version 1 value or meaning |
| --- | ---: | --- | --- |
| 0 | 8 | Magic | `LEAFAPP\0` |
| 8 | 2 | Version | `1` |
| 10 | 2 | Header size | `48` |
| 12 | 4 | Flags | Reserved; must be `0` |
| 16 | 8 | Entry offset | Byte offset from the start of code |
| 24 | 8 | Code size | Number of code bytes after the header |
| 32 | 8 | Data size | Number of initialized data bytes after code |
| 40 | 8 | BSS size | Number of zero-initialized bytes after data |

All integer fields are unsigned and little-endian. The code segment must be
non-empty, and the entry offset must be less than the code size. The file must
contain exactly `48 + code_size + data_size` bytes. The combined code, data,
and BSS size is limited to 64 KiB. Unsupported versions, nonzero flags,
trailing or missing bytes, and out-of-range entry points are rejected.

The loader maps code read/execute, initialized data and BSS read/write and
non-executable, and a separate 16 KiB read/write, non-executable stack. Code
starts at a fixed user virtual address; the data segment starts on the next
page boundary after code. One unmapped page separates the image from the
stack. Version 1 has no relocations, dynamic linking, or configurable segment
permissions.

At process entry, the kernel passes the argument count in `RDI` and a pointer
to the null-terminated `argv` array in `RSI`, following the x86_64 System V
startup convention. `argv[0]` is the resolved executable path; later entries
are the command-line arguments, and `argv[argc]` is null. The shell splits
arguments on ASCII whitespace; quoting and escaping are not supported yet.
At most 16 additional arguments and 1 KiB of argument strings are accepted.

`tools\pack-leaf-app.ps1` packages prebuilt code and optional data bytes:

```powershell
.\tools\pack-leaf-app.ps1 `
  -CodePath build\demo.code `
  -DataPath build\demo.data `
  -OutputPath rootfs\apps\demo.app `
  -EntryOffset 0 `
  -BssSize 128
```

The sample app is assembled from `tests\fixtures\smoke_app.S` and packaged by
`tools\build-smoke-app.sh` during normal and test image builds. It checks its
initialized data and zero-filled BSS, writes to its data segment, calls
`get_ticks`, and exits with status 42. Raw `.bin` programs remain supported
for compatibility. Apps can be launched with arguments using
`run apps/demo.app one two` or `diskrun apps/demo.app one two`.

## Available system calls

Applications invoke system calls with `syscall`, placing the call number in
`RAX` and arguments in the registers documented below. Return values are in
`RAX`; negative values are two's-complement error codes.

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| `0` | `get_ticks` | none | Kernel timer ticks |
| `1` | `exit` | `RDI=status` | Does not return |
| `2` | `write` | `RDI=buffer`, `RSI=length` | Bytes written |
| `3` | `read` | `RDI=buffer`, `RSI=capacity` | Bytes read, nonblocking |
| `4` | `read_file` | `RDI=path`, `RSI=path length`, `RDX=buffer`, `R10=capacity` | Bytes read |
| `5` | `write_file` | `RDI=path`, `RSI=path length`, `RDX=buffer`, `R10=length` | `0` on success |
| `6` | `yield` | none | `0` |
| `7` | `sleep` | `RDI=timer ticks` | `0` |
| `8` | `spawn` | `RDI=path`, `RSI=path length` | Child task ID |
| `9` | `wait` | `RDI=child task ID or 0`, `RSI=writable exit-status pointer` | Child task ID |
| `10` | `spawn_with_arguments` | `RDI=path`, `RSI=path length`, `RDX=argument pointers`, `R10=argument count` | Child task ID |
| `11` | `fd_read` | `RDI=descriptor`, `RSI=buffer`, `RDX=capacity` | Bytes read |
| `12` | `fd_write` | `RDI=descriptor`, `RSI=buffer`, `RDX=length` | Bytes written |
| `13` | `pipe` | `RDI=read-descriptor output`, `RSI=write-descriptor output` | Packed read/write descriptors |
| `14` | `close` | `RDI=descriptor` | `0` on success |
| `15` | `dup2` | `RDI=source`, `RSI=target` | Target descriptor |

The GUI ABI also provides syscall `20`, `draw_rect`, with arguments
`RDI=window ID`, `RSI=x`, `RDX=y`, `R10=width`, `R8=height`, and `R9=RGB`.
Coordinates are window-local, rectangles must be non-empty and fully inside
the owned window, and each window retains at most 32 rectangles. Retained
rectangles are replayed after desktop redraws. Invalid geometry, unknown
windows, and windows owned by another task return `LEAFOS_EINVAL`.
Syscall `21`, `draw_text`, uses `RDI=window ID`, `RSI=x`, `RDX=y`,
`R10=text pointer`, `R8=text length`, and `R9=RGB`. Text is rendered with
the kernel's fixed 8x8 glyphs, must be non-empty, and must fit within the
window. Each window retains at most 16 text commands of at most 64 bytes;
user text is copied after pointer validation and replayed after redraws.

The console and input buffers are limited to 4096 bytes per call. File paths
are limited to 255 bytes, and file data is limited to 4096 bytes per call.
Pointers and lengths are checked against the calling process's mapped user
pages before the kernel accesses them. File calls operate on the ext2 data
disk, not the read-only initrd. `read` is nonblocking and returns zero when
no keyboard input is queued. `yield` voluntarily gives another runnable task
the CPU, while `sleep` blocks the calling task and lets the scheduler wake it
after the requested PIT ticks. Sleep requests are limited to 360,000 ticks
(one hour at the current 100 Hz timer rate). Timer preemption still applies
when an application does not yield.

`spawn` starts an application from the ext2 data disk and makes it a child of
the calling task. It starts with only `argv[0]` set to the path; command-line
arguments can be supplied with `spawn_with_arguments`, subject to the normal
16-argument and 1 KiB argument limits. `wait` accepts a specific
child task ID or zero to wait for any child, stores the child's exit status,
and reaps the child. A task cannot wait for or reap an unrelated task.

The C SDK exposes `LEAFOS_TICKS_PER_SECOND` and
`LEAFOS_MAX_SLEEP_SECONDS`, plus a checked convenience helper:

```c
if (leafos_sleep_seconds(2) < 0) {
    return 1;
}
```

`leafos_sleep_seconds` converts whole seconds to timer ticks and returns
`LEAFOS_E2BIG` instead of overflowing or issuing a request above the kernel
limit.

The C SDK also provides `leafos_spawn(path, length)` and
`leafos_spawn_with_arguments(path, length, arguments, count)` and
`leafos_wait(child_task, &status)` for parent/child application control.
Descriptors `0`, `1`, and `2` are standard input, output, and error. The
`leafos_pipe` helper creates a bounded in-memory pipe and returns its two
descriptors through writable user output pointers. Invalid output pointers
are rejected with `LEAFOS_EFAULT`. Descriptor tables are inherited by
spawned user applications, so a parent can create a pipe before spawning a
child. Descriptor reads are currently nonblocking and return zero when no
data is available. After the last write end is closed, reads return zero as
EOF. Writes after all read ends are closed return `LEAFOS_EPIPE`.
Nonblocking reads return `LEAFOS_EAGAIN` while a pipe is open but temporarily
empty; applications should yield and retry in that case.
Descriptors should be closed when no longer needed; this is especially
important for pipelines, because inherited write ends otherwise prevent EOF.
`dup2` replaces the target descriptor, closing its previous value first. This
allows an application to map a pipe endpoint onto standard input or output
before spawning a child. Kernel launchers can use the equivalent internal
descriptor-mapping API when constructing a child directly. Writes are bounded
to 4096 bytes per call.

The kernel shell supports a bounded two-stage ext2 pipeline form:

```text
diskrun apps/producer.app | diskrun apps/consumer.app
```

The left application's standard output is connected to the right application's
standard input. Pipe endpoints are removed from each child after remapping,
and the shell closes its own copies, so EOF works when the producer exits.

The repository also includes `apps/echo.app`, a freestanding user-space
utility that writes its arguments to standard output separated by spaces and
followed by a newline. It can be built and packaged with:

```powershell
.\tools\build-echo-app.ps1
```

After the ext2 image is rebuilt, it can be used as the producer in a pipeline:

```text
diskrun apps/echo.app hello LeafOS | diskrun apps/echo.app
```

The `apps/gui-demo.app` sample creates an owned window, draws retained
rectangles through the GUI ABI, and polls keyboard/mouse events. Launch it
with `diskrun apps/gui-demo.app`; click its window to focus it, then press
Escape or Ctrl+C to close it.

`apps/cat.app` copies an ext2 file to standard output when given a path, or
copies standard input to standard output when invoked without arguments. The
stdin mode polls the current nonblocking descriptor and yields between empty
reads, allowing it to consume a pipeline until EOF:

```powershell
.\tools\build-cat-app.ps1
```

```text
diskrun apps/echo.app hello LeafOS | diskrun apps/cat.app
diskrun apps/cat.app docs/ABOUT.TXT
```

Application stdout can be redirected to a writable ext2 file with `>`:

```text
diskrun apps/echo.app hello LeafOS > output.txt
diskrun apps/echo.app hello | diskrun apps/cat.app > output.txt
```

The destination is truncated when the command starts. The file-backed
descriptor is inherited by the user application and each write updates the
ext2 file through the normal filesystem write path. Redirection targets are
always on the ext2 data disk, including when the application itself is loaded
with `run`.

Application stdin can be redirected from an existing ext2 file with `<`:

```text
diskrun apps/wc.app < input.txt
diskrun apps/cat.app < input.txt | diskrun apps/wc.app
diskrun apps/cat.app < input.txt > copy.txt
```

The file-backed input descriptor maintains a read offset and returns EOF after
the complete file has been consumed. It is inherited by the application in
the same way as pipe descriptors.

Pipe reads and writes are scheduler-aware: a user task waits instead of
spinning when a pipe is empty or full, and descriptor activity wakes waiting
tasks. Console stdin remains nonblocking so interactive programs can continue
to respond to keyboard interrupts.

`Ctrl+C` interrupts all applications currently tracked by the shell. This
includes both sides of a pipeline; interrupted applications report exit status
`130` and their descriptors are released when the shell reaps them.

The shell also supports terminating a non-running task by ID:

```text
ps
kill 7
```

The command uses exit status `143` and refuses to terminate the currently
running task or the idle task.

`ps` displays each task's parent ID and retained exit status. A `None` exit
status means the task has not terminated; terminated tasks retain their status
until the parent reaps them.

`apps/wc.app` counts bytes and newline characters from standard input or one
or more ext2 files:

```powershell
.\tools\build-wc-app.ps1
```

```text
diskrun apps/echo.app hello LeafOS | diskrun apps/wc.app
diskrun apps/wc.app docs/ABOUT.TXT
```

`apps/ls.app` lists ext2 directory entries, marking directories with a
trailing slash. It accepts an optional directory path and writes the listing
to standard output, so it can be piped or redirected:

```powershell
.\tools\build-ls-app.ps1
```

```text
diskrun apps/ls.app
diskrun apps/ls.app docs | diskrun apps/wc.app
```

Listings include the byte size followed by the entry name. `apps/grep.app`
filters stdin or ext2 files by a literal substring and writes matching lines
unchanged. Pass `-i` for ASCII case-insensitive matching:

```powershell
.\tools\build-grep-app.ps1
```

```text
diskrun apps/ls.app apps | diskrun apps/grep.app ".app"
diskrun apps/cat.app README.TXT | diskrun apps/grep.app LeafOS
diskrun apps/grep.app -i leafos README.TXT
```

The shell removes matching single or double quotes around individual arguments,
so patterns and paths containing spaces can be passed as one argument. Escape
sequences are not interpreted. When a file argument is provided, the app reads
the file from ext2 instead of stdin. `grep.app` waits for input when run with
only a pattern because console stdin does not produce EOF. Press `Ctrl+C` to
terminate a running user app; it exits with status `130`. In a pipeline, the
filter exits when the preceding app closes its output.

`apps/head.app` writes the first ten lines from stdin by default. A numeric
argument changes the limit, and `-n` provides the same option explicitly:

```text
diskrun apps/ls.app apps | diskrun apps/head.app 3
diskrun apps/head.app -n 2 README.TXT
```

`apps/leafquest.app` is a small interactive terminal adventure built directly
against the LeafOS syscall ABI. Explore the dungeon with `e`, drink potions
with `p`, view stats with `s`, or quit with `q`:

```text
diskrun apps/leafquest.app
```

The current error values are `-2` for a missing file, `-5` for an I/O error,
`-7` for an oversized request, `-14` for an invalid user buffer, `-22` for
an invalid argument, and `-32` for a broken pipe. Unknown syscall numbers
return `-38`.

For C or assembly applications, `userland\leafos.h` provides the syscall
numbers, limits, error values, and function declarations, while
`userland\leafos_syscall.S` provides freestanding x86-64 wrappers. The
wrappers use the normal System V argument registers and return the kernel's
`RAX` value unchanged. They do not allocate memory or depend on a runtime
library, so they can be linked into a fixed-address `.app` after compiling the
application with `-ffreestanding -mno-red-zone`.

The wrapper layer can be tested and packaged with:

```powershell
.\tools\build-sdk-smoke-app.ps1
```

This creates `rootfs\apps\sdk-smoke.app`, which calls `leafos_write` and
`leafos_exit` through the SDK. The script requires `clang`, `ld.lld`, and
`ld` plus `llvm-objcopy` or `objcopy` on `PATH`.

For C applications, `userland\leafos_start.S` provides the `_start` entry
point. It forwards the kernel-provided `argc` and `argv` to:

```c
int main(int argc, char **argv);
```

The return value from `main` becomes the application's exit status. A complete
C example can be built with:

```powershell
.\tools\build-c-app-smoke.ps1
```

This produces `rootfs\apps\c-smoke.app` without requiring libc or a C runtime
library.

.\tools\pack-leaf-app.ps1 `
  -CodePath tests\fixtures\smoke_app.S `
  -OutputPath rootfs\apps\demo.app `
  -EntryOffset 0 `
  -BssSize 128