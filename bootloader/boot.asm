[org 0x7c00]

mov ah, 0x0e
mov al, 'B'
int 0x10

mov bx, 0x7e00

mov ah, 0x02
mov al, 1
mov ch, 0
mov cl, 2
mov dh, 0
mov dl, 0x80
int 0x13 

jc disk_error

jmp 0x0000:0x7e00

disk_error:
    mov ah, 0x0e
    mov al, 'E'
    int 0x10
    jmp $

times 510-($-$$) db 0
dw 0xaa55