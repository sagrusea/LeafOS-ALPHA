align 8
GDTStart:
    nullDescriptor:
        dd 0
        dd 0

    codeDescriptor:
        dw 0xffff
        dw 0
        db 0

        db 0b10011010

        db 0b11001111

        db 0

    dataDescriptor:
        dw 0xffff
        dw 0
        db 0

        db 0b10010010

        db 0b11001111

        db 0
GDTEnd:

GDTDescriptor:
    dw GDTEnd - GDTStart - 1
    dd GDTStart