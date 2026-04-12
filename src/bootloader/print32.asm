[bits 32]

VIDEO_MEMORY equ 0xb8000
WHITE_BLACK equ 0x0f

clear_screen:
    pushad
    mov edx, VIDEO_MEMORY
    mov al, ' '
    mov ah, WHITE_BLACK
    mov cx, 2000 ; 80*25
    .loop:
        mov [edx], ax
        add edx, 2
        loop .loop
        popad
        ret

print_string_32:
    pushad
    mov edx, VIDEO_MEMORY

    .loop:
        mov al, [ebx]
        mov ah, WHITE_BLACK

        cmp al, 0
        je .done

        mov [edx], ax

        add ebx, 1
        add edx, 2

        jmp .loop
    .done:
        popad
        ret