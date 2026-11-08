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

The `gop_print!` and `gop_println!` macros use the framebuffer when one is
available and automatically fall back to serial output for headless boots.
This keeps the shell and kernel diagnostics observable even when UEFI does not
provide a usable graphical framebuffer.

### Runtime stability

The project places emphasis on safe startup and stable early execution. This is a critical design choice for an OS that is still in alpha: if the base is not trustworthy, nothing above it can be trusted either.

### Kernel source layout

Kernel source files are grouped by responsibility: `arch/` contains CPU setup
and interrupt handling, `console/` contains framebuffer and text output,
`drivers/` contains hardware drivers, `fs/` contains filesystems, `memory/`
contains memory management, `tasking/` contains the scheduler, and `time/`
contains timer support. The crate root re-exports the moved modules so existing
kernel and test code can continue using paths such as `kernel::framebuffer`
and `kernel::scheduler`.

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

Each process address space owns a fresh level-4 root and shares the kernel's
existing mappings with their user-accessible bit cleared at the root. PML4
slot 255 is reserved for a per-process 512 GiB user region; the kernel heap
allocator skips that slot. User-page allocation zeroes frames and supports
read-only, read/write, or read/execute permissions, never writable-executable
pages. User mappings are explicitly destroyed before their root is reclaimed. The
scheduler activates each user task's root and restores the kernel root when
returning to a kernel task or bootstrap. Tasks are pinned to the first CPU that
dispatches them, so their address spaces are not migrated between CPUs.

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
`diskcat` expose its contents, and `diskwrite` creates or replaces files.
The shell supports nested directory creation with `mkdir`, file removal with
`rm`, and removal of empty directories with `rmdir`; recursive deletion is
not implemented.
The reader validates filesystem metadata and supports regular files and
directories backed by the twelve direct inode block pointers and one
single-indirect and one double-indirect block. The writer supports the same
block-pointer levels on generated 1 KiB-block, single-group ext2 images, and
reclaims data and indirect metadata blocks on shrink and deletion. Replacements
stage fresh data and pointer blocks before switching the inode; a data-write
failure before that point preserves the prior file contents, at the cost of
temporarily needing space for both copies. The driver is polling and
single-CPU-only; DMA, AHCI, and SMP synchronization are not implemented. It
flushes each sector-write command. File create and replacement operations keep
in-memory metadata before-images and roll back reported write failures;
rollback failure is returned explicitly. This is not power-loss recovery, and
interruption can still leave ext2 metadata inconsistent. The initrd remains
read-only. User programs can be loaded from either filesystem:
`run` reads from the current filesystem, while `diskrun` explicitly selects
ext2. Both paths apply the executable size and format checks before mapping
user memory.

### Shell interaction

The shell supports editing the current command with the arrow keys, Home/End,
Backspace, and Delete, plus an eight-command Up/Down history. Its built-in
`edit` command opens or creates UTF-8 text files by path on the ext2 disk;
the editor supports multiline insertion and cursor movement, with Ctrl+S to
save and Ctrl+Q/Esc to quit. This remains a kernel-resident shell, not a
user-mode program environment.

### GUI milestone

The `gui` shell command renders a desktop preview directly into the initialized
framebuffer. The kernel GUI layer now owns a bounded window table with stable
window IDs, title and geometry metadata, close operations, and a bounded FIFO
event queue. Keyboard bytes are offered to that queue while preserving the
existing shell and user-program input paths. This provides a reusable drawing
and event foundation rather than a one-off screenshot.

The PS/2 mouse driver now decodes three-byte packets, maintains a bounded
screen cursor using the active GOP framebuffer dimensions, and emits mouse
events. Clicking performs window hit-testing
and focus selection; focused windows are rendered with the accent frame.
Both APIC vector 34 and legacy PIC vector 44 are wired to the mouse handler.
Mouse movement repaints the desktop immediately, so the cursor state is
visible without rerunning the `gui` command. Cursor-only movement uses an
XOR overlay to erase and redraw only the small cursor shape; it no longer
clears or repaints the full framebuffer on every mouse packet. Full redraws
remain reserved for desktop layout and focus changes.
Bulk rectangle rendering uses clipped scanline writes with packed 32-bit
pixels, avoiding per-pixel bounds calculations and framebuffer reads. This
keeps the safe software framebuffer path while reducing the cost of the
current retained-surface implementation.
Leaving GUI mode through `cls`, `clear`, or the equivalent shell command first
removes the cursor overlay and disables GUI event delivery, preventing stale
cursor artifacts from remaining on the terminal. The renderer snapshots GUI
state before drawing, so a mouse interrupt does not hold the GUI mutex while a
layout redraw is in progress.
User-space applications can create windows, poll fixed-size keyboard/mouse
events, close only windows owned by their task, and submit retained rectangles
and 8x8 text commands through syscalls 17-21. Event delivery is owner-filtered, so one GUI task
cannot consume another task's input. Rectangle commands use window-local
coordinates, are bounds-checked by the kernel, and are replayed after desktop
redraws. Text is copied from validated user memory and is similarly bounded.
Each window has a bounded command list; applications still cannot map the raw
framebuffer. This is a safe first drawing surface while input widgets and
surface management remain future work. The desktop window manager also draws a
close button in each window title bar and handles its hit-testing in the kernel.
Holding the primary mouse button on a title bar drags the window, while window
positions remain clamped below the desktop panel.
Drag motion uses a damage-region repaint instead of clearing the full
framebuffer on every mouse packet; the old and new window regions are restored
and only intersecting windows are replayed.
The desktop panel also includes a status bar with uptime, visible-window count,
and focus state, providing the first live desktop shell information without
requiring a separate user-space compositor.
SMP startup uses the validated ACPI processor list to start each available
secondary CPU up to the kernel's CPU-local limit. Each AP loads a private GDT
and TSS (including a private double-fault stack), loads the IDT, installs a
GS-relative CPU-local slot, and joins the shared scheduler. Fixed APIC
reschedule IPIs wake idle APs and prompt running tasks to yield when new work
becomes ready.
The framebuffer hot path uses one bounded 32-bit unaligned store per pixel for
the 32-bit UEFI format instead of three volatile byte stores, which is safe for
the normal framebuffer memory mapping and substantially reduces software
rendering overhead.
Framebuffer composition now targets a system-RAM back-buffer. Dirty rectangles
are copied to the hardware framebuffer in row-sized bulk operations, while
full-frame composition uses one deferred flush. Cursor movement only updates
its small overlay region, avoiding full desktop copies.
Closing a focused owned window clears focus and repaints the desktop, avoiding
stale window pixels after an application exits.
Task termination also removes every window owned by the exiting task,
including sibling applications terminated by Ctrl+C, so dead GUI clients
cannot leave stale surfaces or retain input ownership. Their queued events are
purged at the same time, while events belonging to other GUI clients remain
ordered and available.
The packaged `gui-demo.app` exercises this path end to end: it creates a
window, draws three retained colored panels and a text label, and exits on
Escape or Ctrl+C.

### Timer

The kernel programs the legacy PIT to generate 100 interrupts per second on the
bootstrap CPU. The timer interrupt updates an atomic tick counter, sends its
controller EOI, and asks the scheduler to preempt. Ticks back the shell's
`uptime` command and task-aware sleeps; they measure elapsed time since kernel
startup, not wall-clock time. The bootstrap CPU can preempt on timer ticks;
secondary CPUs are rescheduled by APIC IPIs and voluntary scheduler operations.
An expired sleeper is made runnable and secondary CPUs are notified.

### Interrupt-controller transition

The kernel contains an APIC controller implementation for the standard I/O
APIC routes. During initialization it explicitly maps the local APIC and I/O
APIC MMIO pages as uncached kernel mappings before touching the registers. If
CPU support or MMIO mapping is unavailable, the kernel safely falls back to
the legacy 8259 PIC. The UEFI loader captures the ACPI RSDP before exiting boot
services; the kernel validates the RSDP/XSDT or RSDT and MADT checksums, then
uses the discovered local APIC/I/O APIC addresses and interrupt-source
overrides. Invalid or unavailable ACPI data also selects the PIC fallback.
The MADT parser records enabled processor APIC IDs and reports the discovered
topology during boot. The startup path copies a trampoline below 1 MiB,
identity-maps it, sends INIT/SIPIs, and waits for each AP to report readiness.
Each AP gets a separate GDT/TSS and GS slot. Every CPU owns an eight-slot
runqueue and interrupt-safe lock, plus its own current-task and bootstrap-context
slots. New tasks are placed on the least-loaded online CPU; task IDs remain
globally unique, while execution stays pinned to the first CPU to avoid
migrating saved contexts and address spaces. Cross-queue task lookups lock one
runqueue at a time. APs install their own syscall entry state and can run user
tasks. APs have no periodic local timer, so rescheduling there depends on
reschedule IPIs and task yield/block/sleep operations; a CPU-bound task that
does not yield may continue running until another task or wake event sends an
IPI.

### Scheduler groundwork

The bounded scheduler has task IDs, ready/running/blocked/sleeping/terminated
states, and deterministic round-robin selection. Kernel tasks use dedicated
64 KiB stacks; cooperative switches save the callee-saved registers and flags,
while timer preemption suspends the compiler-saved general registers and
hardware interrupt frame on the interrupted task's own stack. The stack and
saved flags are restored when that task is scheduled again. Blocked tasks can
be woken; sleeping tasks become ready when their timer deadline expires.
Terminated task stacks remain allocated until explicitly reaped, so an active
stack is never freed during task exit. Kernel mutexes and one-time initializers
preserve the previous interrupt state and disable local interrupts while held
or initializing. Scheduling operations lock only the local runqueue; task
placement, snapshots, and parent/child lookups coordinate by briefly scanning
the independent queues. Each CPU's permanent idle task runs only on its own
queue.
Tasks created while another task is running retain that task's ID as an
optional parent, while bootstrap and idle tasks are parentless. The scheduler
exposes this relationship for the shell and future wait/ownership APIs; it
does not yet enforce parent-side reaping. Ext2 block-sized scratch buffers are heap-backed so
nested file operations fit within task stacks. Essential startup diagnostics
are emitted before the shell task is started, and the kernel panic handler
reports task failures to serial independently of the shell. The first panicking
CPU claims an atomic panic lock and broadcasts a local-APIC NMI to stop the
other CPUs before reporting the panic; later panicking CPUs halt without
printing over the original diagnostic.

### User-mode groundwork

The GDT now includes ring-3 code and data selectors, and the TSS can be given a
per-task ring-0 stack. `arch::user` validates initial entry/stack addresses
against mapped user pages and provides an explicit `iretq` transition.
`arch::syscall` contains a register-saving `SYSCALL` entry, safe user-flags
masking, return-address/stack bounds checks, `get_ticks`, and task exit. The
scheduler owns each scheduled user's address space, activates its CR3 and
kernel entry stack when selected, and restores the boot kernel root for kernel
tasks and bootstrap. User page faults and breakpoints emit serial diagnostics
and terminate only the current user task. User tasks can run on APs; each CPU
has independent syscall MSRs, GS-local state, GDT/TSS, and kernel entry stack.
Tasks are not migrated between CPUs, so cross-CPU TLB shootdowns are not yet
needed for per-task address-space teardown.

Normal boot does not start a user app automatically. The shell's `run` command
starts a scheduled user task on demand; the scheduler installs syscall MSRs
when it selects that task. QEMU tests exercise the real `arch::user::enter`
transition through scheduled tasks:
one task returns from `get_ticks`, then faults on a supervisor-mapped kernel
address; a second task at the same user virtual addresses returns from
`get_ticks` and exits with a checked `SYS_EXIT` status; a third supplies an
invalid syscall-return stack and is terminated without halting the kernel. A
scheduled kernel diagnostics task runs afterward, and the test verifies the
kernel CR3 is restored. The entry, syscall return, task exit/fault handling,
and scheduler address-space activation paths are tested.

The read-only initrd can contain raw `.bin` programs and versioned LeafOS
`.app` programs. See [App-Format.md](./App-Format.md) for the v1 format
specification. The bounded `.app` v1 loader validates its fixed-size
little-endian header, entry point, segment lengths, and memory limit. It maps
code read/execute, initialized data and zero-filled BSS read/write, and a
separate read/write, non-executable stack. Both formats use fixed user virtual
addresses and do not support relocation or dynamic linking. The smoke `.app`
is assembled and packaged during normal and test image builds; its QEMU test
writes to initialized data, verifies its BSS starts zeroed, calls `get_ticks`,
and exits with a checked status.
Normal boot does not start an app automatically; the shell `run` command is
opt-in.

### Foundation checkpoint before GUI work

At this historical pre-GUI checkpoint, the foundation was complete for
single-CPU use:
UEFI boot, paging and isolated user address spaces, ext2 file operations,
scheduled ring-3 applications, pipes and descriptor inheritance, shell
pipelines/redirection, process control, APIC initialization with PIC fallback,
ACPI MADT discovery, and QEMU boot validation are all in place. Syscall
CPU-local accesses are GS-relative, and the boot CPU-local block can be
replaced by per-CPU storage when AP startup is implemented.

That checkpoint provided a base for adding applications, device drivers,
crash-safe filesystem behavior, and operator-facing diagnostics. The later
SMP work starts secondary CPUs with per-CPU GDT/TSS and syscall state and adds
multicore scheduling; task migration, periodic AP timers, and broader
SMP-safe filesystem testing remain future work.

The per-CPU storage milestone is complete: CPU-local syscall state is no
longer tied to one global block, and invalid CPU-slot selection is rejected.
The scheduler supports concurrent kernel- and user-task execution on started
APs. Tasks remain pinned to their first CPU; periodic AP timers, task migration,
and broader SMP stress testing remain future work.

## Future work

Further work includes periodic AP timer delivery, task migration with
cross-CPU TLB coordination, SMP-safe filesystem operations, and broader
multicore stress testing.

That is the heart of LeafOS.
