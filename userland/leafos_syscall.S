.intel_syntax noprefix

.section .text

.global leafos_get_ticks
.type leafos_get_ticks, @function
leafos_get_ticks:
    xor eax, eax
    syscall
    ret

.global leafos_exit
.type leafos_exit, @function
leafos_exit:
    mov eax, 1
    syscall
    ud2

.global leafos_yield
.type leafos_yield, @function
leafos_yield:
    mov eax, 6
    syscall
    ret

.global leafos_sleep
.type leafos_sleep, @function
leafos_sleep:
    mov eax, 7
    syscall
    ret

.global leafos_write
.type leafos_write, @function
leafos_write:
    mov eax, 2
    syscall
    ret

.global leafos_read
.type leafos_read, @function
leafos_read:
    mov eax, 3
    syscall
    ret

.global leafos_read_file
.type leafos_read_file, @function
leafos_read_file:
    mov r10, rcx
    mov eax, 4
    syscall
    ret

.global leafos_write_file
.type leafos_write_file, @function
leafos_write_file:
    mov r10, rcx
    mov eax, 5
    syscall
    ret
