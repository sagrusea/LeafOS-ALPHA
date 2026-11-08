# LeafOS ALPHA 0.2.0

LeafOS ALPHA is a 64-bit kernel-first operating system project built as a minimal x86_64 environment for learning, experimentation, and low-level software design. It is not a general-purpose desktop OS yet; it is a focused attempt to understand how a bare-metal kernel boots, initializes hardware, manages memory, and exposes a stable runtime.

The project is intentionally small, direct, and architecture-aware. At this stage, the goal is less about shiny user-facing features and more about building a sound foundation: a boot path, a usable kernel model, interrupt handling, framebuffer output, and a clear mental model of how the OS starts.

## What LeafOS is trying to be

- a 64-bit bare-metal kernel project
- a learning platform for x86_64 operating system internals
- a minimal environment for interrupt handling and kernel startup
- a ground layer for future memory management, scheduling, and user-mode work
- a project centered on clarity over complexity

## Current direction

The current codebase focuses on the parts that matter most at the beginning of a kernel:

- booting into a usable kernel entry point
- setting up CPU and memory structures
- initialization of low-level services
- serial and graphical output for debugging
- handling early kernel panic and fault states reliably

This is the stage where a kernel proves it can stand up cleanly, not where it tries to become a full desktop environment.

## Design goals

LeafOS is built around a few simple principles:

- keep the kernel small and understandable
- prefer explicit hardware control over abstraction
- build the OS from the machine upward
- make debugging visible through serial and framebuffer output
- keep the architecture modern and 64-bit by default

## Kernel identity

LeafOS is best understood as a minimal x86_64 kernel project rather than a traditional user-facing operating system. It is designed to answer practical questions like:

- how does the CPU enter long mode?
- how are interrupt tables configured?
- how does the kernel expose output and failure states?
- what is the minimum runtime needed before user programs and services exist?

The OS is still in alpha, and that is intentional. The value is in the structure, the learning, and the disciplined approach to system design.

## Repo focus

The repository contains the core operating system components and supporting test infrastructure, including:

- bootloader and early startup code
- a Rust kernel crate
- low-level memory and interrupt setup
- framebuffer and serial debugging paths
- experimentation and OS-theory notes

## Documentation

- [wiki/Home.md](./wiki/Home.md)
- [wiki/Getting-Started.md](./wiki/Getting-Started.md)
- [wiki/Kernel-Architecture.md](./wiki/Kernel-Architecture.md)

## Notes

LeafOS is an alpha kernel project, not a polished end-user system. Its purpose is to explore and refine the fundamentals of operating-system design in a compact, readable form.
