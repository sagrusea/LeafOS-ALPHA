# Getting Started

LeafOS starts with a simple idea: build the kernel from its architecture up, not from a desktop environment down.

## The operating system mindset

LeafOS is centered around a minimal x86_64 kernel model. That means the project is less about end-user software and more about the actual machinery that makes an OS possible:

- CPU mode and startup state
- memory layout and initialization
- interrupt and exception handling
- framebuffer and serial debugging
- panic and fault visibility
- a clean path toward higher-level services later

## What matters most

The first real milestone for an OS like this is not a polished shell or a GUI. The first milestone is a kernel that can reliably initialize itself, report status, and stay stable under basic fault conditions.

That is the stage LeafOS is exploring right now.

## Design direction

The project is intentionally built around a small number of strong principles:

- keep the kernel simple and readable
- prefer explicit system design over hidden abstractions
- treat debugging output as part of the OS, not as an afterthought
- move toward more advanced features only once the base is strong

## What the OS aims to become

The long-term direction is a more complete kernel with memory management, scheduling, device support, and eventually user-space services. But before any of that, the system must be stable, structured, and honest about what it can do.

This is why LeafOS is still alpha: it is a system under construction, with a clear foundation and a deliberate trajectory.
