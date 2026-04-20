[org 0x7e00]
stage_2_top:
[bits 16]

stage_2:
    ;mov eax, 0xE820
    ;int 0x15

    xor ax, ax
    mov ds, ax
    mov es, ax

    in al, 0x92
    or al, 2
    out 0x92, al

    cli
    lgdt [GDTDescriptor]

    mov eax, cr0
    or eax, 0x1
    mov cr0, eax

    jmp 0x08:init_pm

[bits 32]
init_pm:
    mov ax, 0x10
    mov ds, ax
    mov ss, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    mov ebp, 0x90000
    mov esp, ebp

    call begin_pm

begin_pm:
    jmp 0x8000
    jmp $

%include "src/bootloader/gdt.asm"

times 512-($-stage_2_top) db 0