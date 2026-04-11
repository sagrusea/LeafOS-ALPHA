[org 0x7e00]

mov si, STAGE2_MSG
call print_string

jmp $

%include "print.asm"

STAGE2_MSG: db 'Loaded into stage 2', 0