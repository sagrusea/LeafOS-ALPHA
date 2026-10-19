.intel_syntax noprefix

.section .text
.global _start
.extern main
.extern leafos_exit

_start:
    call main
    mov edi, eax
    call leafos_exit
    ud2
