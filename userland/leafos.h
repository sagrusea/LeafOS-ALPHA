#ifndef LEAFOS_H
#define LEAFOS_H

typedef unsigned long leafos_size_t;
typedef unsigned long long leafos_u64;
typedef long long leafos_result_t;

#define LEAFOS_SYS_GET_TICKS 0
#define LEAFOS_SYS_EXIT 1
#define LEAFOS_SYS_WRITE 2
#define LEAFOS_SYS_READ 3
#define LEAFOS_SYS_READ_FILE 4
#define LEAFOS_SYS_WRITE_FILE 5
#define LEAFOS_SYS_YIELD 6
#define LEAFOS_SYS_SLEEP 7

#define LEAFOS_MAX_SYSCALL_BUFFER 4096
#define LEAFOS_MAX_SYSCALL_PATH 255
#define LEAFOS_TICKS_PER_SECOND 100
#define LEAFOS_MAX_SYSCALL_SLEEP_TICKS 360000
#define LEAFOS_MAX_SLEEP_SECONDS \
    (LEAFOS_MAX_SYSCALL_SLEEP_TICKS / LEAFOS_TICKS_PER_SECOND)

#define LEAFOS_E2BIG (-7LL)
#define LEAFOS_EIO (-5LL)
#define LEAFOS_EFAULT (-14LL)
#define LEAFOS_EINVAL (-22LL)
#define LEAFOS_ENOENT (-2LL)
#define LEAFOS_ENOSYS (-38LL)

leafos_u64 leafos_get_ticks(void);
void leafos_exit(leafos_u64 status) __attribute__((noreturn));
leafos_result_t leafos_yield(void);
leafos_result_t leafos_sleep(leafos_u64 ticks);

static inline leafos_result_t leafos_sleep_seconds(leafos_u64 seconds) {
    if (seconds > LEAFOS_MAX_SLEEP_SECONDS) {
        return LEAFOS_E2BIG;
    }
    return leafos_sleep(seconds * LEAFOS_TICKS_PER_SECOND);
}

leafos_result_t leafos_write(const void *buffer, leafos_size_t length);
leafos_result_t leafos_read(void *buffer, leafos_size_t capacity);
leafos_result_t leafos_read_file(
    const void *path,
    leafos_size_t path_length,
    void *buffer,
    leafos_size_t capacity
);
leafos_result_t leafos_write_file(
    const void *path,
    leafos_size_t path_length,
    const void *buffer,
    leafos_size_t length
);

#endif
