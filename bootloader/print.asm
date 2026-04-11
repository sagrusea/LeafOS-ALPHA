print_string:
    mov ah, 0x0e
    .loop:
        mov al, [si]
        cmp al, 0
        je .done
        int 0x10
        inc si
        jmp .loop
    .done: ret

print_hex:
    pusha
    mov cx, 0

    .loop:
        cmp cx, 4
        je .done

        mov ax,dx
        and ax, 0x000F

        mov bx, HEX_STR
        add bx, ax
        mov al, [bx]
        mov ah, 0x0e
        int 0x10

        shr dx, 4
        add cx, 1
        jmp .loop

    .done:
        popa
        ret

HEX_STR: db '0123456789ABCDEF'