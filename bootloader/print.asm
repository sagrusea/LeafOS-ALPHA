print_string:
    mov ah, 0x0e
    mov al, [si]
    cmp al, 0
    je done
    int 0x10
    inc si
    jmp print_string

print_hex:
