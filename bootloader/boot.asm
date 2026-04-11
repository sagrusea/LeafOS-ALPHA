[org 0x7c00]

mov si, msg
call print_string

mov bx, 0x7e00
mov dh, 1
mov dl, 0
call disk_load

done:
    jmp $

msg:
    db "LeafOS",0


%include "print.asm"
%include "disk.asm"
times 510-($-$$) db 0
dw 0xaa55