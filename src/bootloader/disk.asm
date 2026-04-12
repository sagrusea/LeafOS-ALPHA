disk_load:
    push dx
    mov ah, 0x02
    mov al, dh
    mov ch, 0x00
    mov dh, 0x00
    mov cl, 0x02

    int 0x13

    jc .disk_error
    pop dx
    ret

.disk_error:
    mov si, ERR_MSG
    call print_string16
    jmp $

ERR_MSG: db 'DriveERR', 0