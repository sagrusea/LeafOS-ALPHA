# Kernel Architecture

LeafOS is structured as a 64-bit kernel project whose purpose is to establish a clean and minimal runtime before additional OS features are layered on top.

## High-level model

The kernel is built around a few core responsibilities:

1. start execution in a valid x86_64 environment
2. initialize low-level structures such as the GDT and interrupt handling
3. establish output paths for diagnostics and kernel feedback
4. keep the runtime stable enough to support the next system features

This is the kernel’s “ground floor.” Once those pieces are solid, the rest of the OS can grow.

## Main components

### Kernel startup

The kernel entry layer is responsible for early initialization. This is where the system boots into a controlled runtime and prepares the machine for the rest of the kernel logic.

### Interrupt and exception handling

A kernel is only useful if it can react predictably to faults, traps, and interrupts. This part of the system is foundational: without it, the OS cannot safely recover, debug, or reason about runtime failures.

### Output and diagnostics

LeafOS places a high value on low-level feedback. Serial output and framebuffer-based output give the kernel a way to report startup state and failures in a transparent, hardware-level way.

### Runtime stability

The project places emphasis on safe startup and stable early execution. This is a critical design choice for an OS that is still in alpha: if the base is not trustworthy, nothing above it can be trusted either.

## Architectural direction

LeafOS aims to remain minimal and readable. The architecture is intentionally not overloaded with abstraction layers or premature feature work. Instead, it follows a disciplined progression:

- initialize CPU and memory structures
- establish interrupt handling
- provide debugging output
- expand into memory management and scheduling when the base is solid

### Paging

Before handing off control, the UEFI loader clones the active four-level page
tables into loader-owned pages, creates a direct mapping of RAM using 2 MiB
pages where possible, and switches CR3 to the clone. The physical-memory offset
is passed in `BootInfo`; the kernel uses it for `OffsetPageTable` access to
physical frames without modifying firmware-owned tables. The UEFI memory map
supplies a frame allocator that returns only conventional-memory frames,
avoiding memory still owned by the loader, kernel, or firmware. The memory
manager is initialized once during regular kernel startup and serializes
allocation and mapping operations. Mapping a virtual page to a frame remains
unsafe because callers must guarantee that the address and frame are safe to
use.

### Read-only files

Before `ExitBootServices`, the UEFI loader reads `initrd.tar` from the boot
volume into loader-owned memory and passes its address and size in `BootInfo`.
The kernel validates the bounded USTAR archive once, then exposes safe nested
paths through the shell's `ls`, `cd`, `pwd`, and `cat` commands. The parser
supports regular files and directories and rejects unsafe path components and
unsupported archive entry types. This is a read-only boot snapshot, not direct
FAT access; runtime disk I/O and writes are not implemented.

### ext2 data disk

The kernel has bounded, polling ATA PIO sector reads and writes with cache
flushes for the primary IDE slave device, plus an ext2 driver for a separate
data disk. The current QEMU scripts attach an ext2 image there; `diskls` and
`diskcat` expose its contents, and `diskwrite` creates or replaces short,
root-level files up to 12 KiB.
The shell can create root-level directories with `mkdir`, remove root-level
files with `rm`, and remove empty root-level directories with `rmdir`. These
operations release inode and direct-block allocations where supported;
recursive deletion and nested creation are not implemented.
The reader validates filesystem metadata and supports regular files and
directories backed by the twelve direct inode block pointers and one
single-indirect and one double-indirect block. It is polling, single-CPU-only,
and does not implement single- or double-indirect writes, DMA, AHCI, or SMP
synchronization. The write path is limited to generated 1 KiB-block,
single-group ext2 images and twelve direct data blocks. It flushes
each sector-write command but is not crash-consistent; interrupted metadata
updates can leak allocations or leave ext2 metadata inconsistent. The initrd
remains read-only.

### Shell interaction

The shell supports editing the current command with the arrow keys, Home/End,
Backspace, and Delete, plus an eight-command Up/Down history. Its built-in
`edit` command opens or creates root-level UTF-8 text files on the ext2 disk;
the editor supports multiline insertion and cursor movement, with Ctrl+S to
save and Ctrl+Q/Esc to quit. This remains a kernel-resident shell, not a
user-mode program environment.

### Timer

The kernel programs the legacy PIT to generate 100 interrupts per second.
The timer interrupt updates an atomic tick counter, which currently backs the
shell's `uptime` command. This is elapsed time since kernel startup, not
wall-clock time; it provides the timing foundation for later sleeps and task
scheduling. The shell's bounded `sleep` command waits with interrupts enabled
and halts between ticks instead of busy-waiting. It blocks the current shell
until its interval completes; task-aware sleeping depends on a scheduler.

## Future work

The next natural steps are virtual memory, task scheduling, and eventually
user-mode execution. LeafOS already establishes a 64-bit paging base; the
periodic timer is the first piece needed for time-based scheduling.

That is the heart of LeafOS.
