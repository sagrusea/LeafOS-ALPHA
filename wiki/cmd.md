
set - settings
usage:
    set col <name|#RRGGBB>
    set res <width>:<height>

Color names: default, green, white, red, yellow, cyan.
Resolution is reported but cannot be changed after UEFI boot yet.

cls - clear screen (`clear`, `clr`, and `dir` aliases are also accepted)

info - prints a system summary; options show a specific category
usage:
    info -os
    info -hw
    info -sw

Other commands:
    help
    shutdown / sd
    echo <text>
    mem
    uptime
    sleep <seconds>
    ps / processes
    run <path.app|path.bin>
    ls [path] / dir [path]
    cd [path]
    pwd

Shell parsing is forgiving: command names are case-insensitive and extra spaces or tabs between tokens are accepted.
    cat <path>
    diskls [path]
    diskcat <path>
    diskwrite <name> [text]
    mkdir <name>
    rm <name>
    rmdir <name>
    edit <name>

The read-only initrd supports nested directories and safe relative paths.
`cd` checks the initrd first and then the ext2 data disk, so a directory created
with `mkdir` can be entered with `cd <name>`. While inside an ext2 directory,
`ls` and `cat` use the data disk. The prompt and `pwd` identify this as `ext2:`.
Bare `cd` or `cd /` returns to the initrd root; `cd ..` at the ext2 root stays
at the ext2 root. Other absolute paths and `..` work within the currently
selected filesystem. Paths cannot escape either filesystem's root.
`cat` output is capped at 2048 bytes. Add initrd files or directories under
`rootfs/` before building the boot image.

`diskls` and `diskcat` access the separate ext2 data disk used by the QEMU
image. Ext2 reads support twelve direct blocks, one single-indirect block, and
one double-indirect block. The ext2 writer supports direct, single-indirect,
and double-indirect blocks, limited by the available blocks on the data disk.
`diskwrite <path> [text]` is limited by the shell's 256-character input line,
while `edit` is capped at 12 KiB. Omitting text from `diskwrite` creates an
empty file. Names are limited to safe ASCII letters, digits, `.`, `_`, and
`-`. These commands do not modify the initrd.
`edit`, `diskwrite`, `mkdir`, `rm`, and `rmdir` modify the ext2 data disk only
and accept paths. For example, `mkdir projects/test` creates a directory
inside `/projects`. Relative paths are based on the current ext2 directory;
from the initrd, names are relative to the ext2 root. `rm` removes files,
while `rmdir` removes empty directories only; recursive deletion is not
supported. Removed inodes and direct data blocks are returned to the allocator.

`uptime` reports elapsed time since kernel startup, using the kernel's 100 Hz
PIT timer. It is not a wall clock.
`sleep <seconds>` waits using timer ticks and accepts whole seconds from 0 to
3600. While sleeping, the CPU halts between timer interrupts rather than
spinning.

`run <path.app|path.bin>` loads a user program from the current filesystem,
starts it as a scheduled user task, and reports its exit status when it
finishes. In the initrd it loads from the read-only archive; when the current
directory is on ext2, it loads from ext2 instead.
`diskrun <path.app|path.bin>` explicitly loads from the ext2 data disk; relative
paths start at the ext2 root unless the current directory is already on ext2.
Prefer the versioned LeafOS format: `run apps/smoke.app` or
`diskrun apps/smoke.app`. Raw `.bin` programs remain supported for
compatibility. Both loaders enforce format and size limits before allocating
an image buffer or creating a user address space. The smoke `.app` is generated
from `tests/fixtures/smoke_app.S` during normal and test image builds.
See [App-Format.md](./App-Format.md) for the v1 header and packaging tool.
Apps are not started automatically at boot. The initrd remains available for
recovery and bootstrap apps.
`ps` (or `processes`) lists scheduled tasks, including user apps, with their IDs
and states. Bare `info` shows a summary
including the kernel version, CPU, uptime, mapped heap, scheduler capacity,
timer rate, and available filesystems. `info -os`, `info -hw`, and `info -sw`
show expanded operating-system, hardware, and software details.

The shell keeps the latest eight commands, skipping consecutive duplicates:
use Up/Down to browse,
Left/Right to move within the current command, Home/End to jump to its
boundaries, and Delete/Backspace to edit it. PageUp/PageDown scroll the
terminal's bounded 256-line output history by half a screen at a time; press
Esc or any other key to return to the live prompt. `clear` also clears
scrollback. Mouse wheel scrolling is not available because the kernel does not
yet have a mouse driver. `edit <path>` opens or creates a UTF-8 file on the
ext2 disk, including paths such as `edit test/test.txt`.
The parent directory must already exist; if it does not, the shell reports the
missing path and suggests the corresponding `mkdir` command. Use the arrow keys to move, Enter to
insert a newline, Ctrl+S to save and exit, or Ctrl+Q/Esc to quit. If there are
unsaved edits, quitting requires confirmation. The editor shows a screen-sized
viewport that follows the cursor through files up to 12 KiB, with line and
column information and a visible cursor. It scrolls horizontally for long
lines, and updates only changed screen cells to avoid the full-screen blink
while typing. UTF-8 text is preserved; characters outside the built-in ASCII
font are displayed as `?`.

The ext2 write path only supports the generated 1 KiB-block, single block-group
images and files using direct, single-indirect, and double-indirect blocks.
Replacing, shrinking, and deleting files release unused data blocks and
indirect metadata blocks. Replacements stage data and indirect blocks in fresh
allocations before switching the inode, so a data-write failure before that
switch leaves the previous file contents intact. This requires enough free
space for the old and new copies at the same time. Writes are not fully
crash-consistent: reported write failures during file creation or replacement
trigger metadata rollback, and `RecoveryFailed` means rollback itself could not
be completed. Power loss can interrupt the transaction without rollback and
leave the filesystem inconsistent. Keep backups and treat the data disk as
experimental.