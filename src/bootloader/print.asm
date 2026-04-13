print_string16:
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
        mov bx, HEX_OUT + 5
        sub bx, cx
        mov [bx], al

        shr dx, 4
        add cx, 1
        jmp .loop

    .done:
        mov si, HEX_OUT
        call print_string16
        popa
        ret

HEX_STR: db '0123456789ABCDEF'
HEX_OUT: db '0x0000', 0