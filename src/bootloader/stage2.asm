[org 0x7e00]
[bits 16]

section .stage2_entry
global stage_2
stage_2:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax

    lgdt [GDTDescriptor]

    mov eax, cr0
    or eax, 0x1
    mov cr0, eax

    jmp 0x08:init_pm

    %include "src/bootloader/gdt.asm"

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

    call clear_screen
    
    mov ebx, MSG_32_MODE
    call print_string_32

    call 0x8000
    jmp $


%include "src/bootloader/print32.asm"
MSG_32_MODE: db "Now in 32 bits", 0

times 512-($-$$) db 0