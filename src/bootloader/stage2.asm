[org 0x7e00]
[bits 16]
mov si, STAGE2_MSG
call print_string16

cli

lgdt [GDTDescriptor]

mov eax, cr0
or eax, 0x1
mov cr0, eax

jmp 0x08:init_pm

%include "gdt.asm"
%include "print.asm"

STAGE2_MSG: db 'Switching to 32-bit mode', 0

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

    jmp $

    %include "print32.asm"

    MSG_32_MODE: db "Now in 32 bits"