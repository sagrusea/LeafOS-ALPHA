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

.global leafos_spawn
.type leafos_spawn, @function
leafos_spawn:
    mov eax, 8
    syscall
    ret

.global leafos_wait
.type leafos_wait, @function
leafos_wait:
    mov eax, 9
    syscall
    ret

.global leafos_spawn_with_arguments
.type leafos_spawn_with_arguments, @function
leafos_spawn_with_arguments:
    mov r10, rcx
    mov eax, 10
    syscall
    ret

.global leafos_fd_read
.type leafos_fd_read, @function
leafos_fd_read:
    mov eax, 11
    syscall
    ret

.global leafos_fd_write
.type leafos_fd_write, @function
leafos_fd_write:
    mov eax, 12
    syscall
    ret

.global leafos_pipe
.type leafos_pipe, @function
leafos_pipe:
    mov r8, rdi
    mov r9, rsi
    mov eax, 13
    syscall
    test rax, rax
    js leafos_pipe_error
    mov rcx, rax
    mov edx, eax
    mov [r8], rdx
    shr rcx, 32
    mov [r9], rcx
    xor eax, eax
    ret
leafos_pipe_error:
    ret

.global leafos_close
.type leafos_close, @function
leafos_close:
    mov eax, 14
    syscall
    ret

.global leafos_dup2
.type leafos_dup2, @function
leafos_dup2:
    mov eax, 15
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

.global leafos_list_dir
.type leafos_list_dir, @function
leafos_list_dir:
    mov r10, rcx
    mov eax, 16
    syscall
    ret
