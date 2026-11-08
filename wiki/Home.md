# LeafOS Wiki

LeafOS is an experimental 64-bit kernel project built around the idea that an operating system should be understandable from the ground up. This wiki is meant to describe the project as an OS design effort, not as a build manual.

## Wiki pages

- [Getting Started](./Getting-Started.md)
- [Kernel Architecture](./Kernel-Architecture.md)

## Project identity

LeafOS is not trying to be a polished desktop OS yet. It is a minimal kernel-centered system exploring how a x86_64 operating system boots, sets up CPU state, handles interrupts, and exposes a stable runtime for future features.

## Core ideas

- 64-bit first: the OS targets a modern x86_64 foundation
- minimal kernel: keep the runtime small and readable
- explicit hardware control: directly manage low-level interfaces instead of hiding them behind layers
- debugging as part of design: serial output and graphics are part of the kernel experience
- incremental growth: add features only after the base is solid

## Current focus

The project is currently centered on the foundations of a working kernel:

- early boot and entry state
- CPU initialization
- interrupt and exception handling
- framebuffer and serial output
- safe panic and error reporting
- a clean path toward memory management and user-mode work

## Why this exists

The real purpose of LeafOS is not to ship a final operating system immediately. It is to learn what makes an OS actually work and to keep that understanding visible in code, design, and documentation.
