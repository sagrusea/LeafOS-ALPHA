[org 0x7c00]
[bits 16]
; make sure stack is safe
xor ax, ax
mov ds, ax
mov es, ax
mov ss, ax
mov bp, 0x7c00
mov sp, bp

mov [BOOT_DRIVE], dl

mov si, msg
call print_string16

mov bx, 0x7e00
mov dh, 43
mov dl, [BOOT_DRIVE]
call disk_load

mov dx,bx
call print_hex

jmp 0x0000:0x7e00

BOOT_DRIVE: db 0
msg: db "LeafOS",0

%include "./src/bootloader/print.asm"
%include "./src/bootloader/disk.asm"

times 510-($-$$) db 0
dw 0xaa55