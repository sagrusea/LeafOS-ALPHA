use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const MAX_TASKS: usize = 16;
const KERNEL_STACK_SIZE: usize = 64 * 1024;

pub type KernelTaskEntry = fn();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(u64);

impl TaskId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked,
    Sleeping,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct CpuContext {
    pub stack_pointer: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rflags: u64,
}

impl CpuContext {
    pub const fn empty() -> Self {
        Self {
            stack_pointer: 0,
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rflags: 0,
        }
    }
}

struct Task {
    id: TaskId,
    state: TaskState,
    context: CpuContext,
    entry: Option<KernelTaskEntry>,
    _stack: Option<Box<[u64]>>,
    wake_at: Option<u64>,
    is_idle: bool,
}

pub struct Scheduler {
    tasks: [Option<Task>; MAX_TASKS],
    current: Option<usize>,
    next_id: u64,
    idle_index: Option<usize>,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: [const { None }; MAX_TASKS],
            current: None,
            next_id: 1,
            idle_index: None,
        }
    }

    pub fn create_task(&mut self, context: CpuContext) -> Result<TaskId, SchedulerError> {
        self.insert_task(context, None, None)
    }

    fn create_kernel_task(&mut self, entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
        // Fail before allocating the kernel stack. In particular, a full task
        // table must return `Full` even when the allocator is exhausted.
        self.check_task_capacity()?;
        self.next_id
            .checked_add(1)
            .ok_or(SchedulerError::TaskIdExhausted)?;

        let mut stack =
            vec![0u64; KERNEL_STACK_SIZE / core::mem::size_of::<u64>()].into_boxed_slice();
        let stack_base = stack.as_mut_ptr() as usize;
        let stack_top = stack_base
            .checked_add(stack.len() * core::mem::size_of::<u64>())
            .ok_or(SchedulerError::InvalidContext)?;
        let aligned_top = stack_top & !0xf;
        let initial_stack = aligned_top
            .checked_sub(2 * core::mem::size_of::<u64>())
            .ok_or(SchedulerError::InvalidContext)?;
        let stack_index = initial_stack
            .checked_sub(stack_base)
            .filter(|offset| offset.is_multiple_of(core::mem::size_of::<u64>()))
            .map(|offset| offset / core::mem::size_of::<u64>())
            .filter(|index| *index < stack.len())
            .ok_or(SchedulerError::InvalidContext)?;
        stack[stack_index] = task_trampoline as *const () as usize as u64;

        let mut context = CpuContext::empty();
        context.stack_pointer = initial_stack as u64;
        context.rflags = 0x202;
        self.insert_task(context, Some(entry), Some(stack))
    }

    fn check_task_capacity(&self) -> Result<usize, SchedulerError> {
        self.tasks
            .iter()
            .position(Option::is_none)
            .ok_or(SchedulerError::Full)
    }

    fn insert_task(
        &mut self,
        context: CpuContext,
        entry: Option<KernelTaskEntry>,
        stack: Option<Box<[u64]>>,
    ) -> Result<TaskId, SchedulerError> {
        let slot = self.check_task_capacity()?;
        let id = TaskId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(SchedulerError::TaskIdExhausted)?;
        self.tasks[slot] = Some(Task {
            id,
            state: TaskState::Ready,
            context,
            entry,
            _stack: stack,
            wake_at: None,
            is_idle: false,
        });
        Ok(id)
    }

    pub fn start(&mut self) -> Result<TaskId, SchedulerError> {
        if self.current.is_some() {
            return Err(SchedulerError::AlreadyStarted);
        }
        self.select_next(0)
    }

    /// Mark the running task ready and select the next ready task.
    ///
    /// With only one runnable task, that task is selected again.
    pub fn yield_current(&mut self) -> Result<TaskId, SchedulerError> {
        self.transition_current(TaskState::Ready)
    }

    /// Block the running task and select another ready task.
    ///
    /// If no other task is ready, the current task remains blocked and
    /// `current_task()` becomes `None`; `NoRunnableTasks` reports that there
    /// was no task to select, not that the transition was rolled back.
    pub fn block_current(&mut self) -> Result<TaskId, SchedulerError> {
        self.transition_current(TaskState::Blocked)
    }

    /// Terminate the running task and select another ready task.
    ///
    /// If no other task is ready, the task remains terminated and
    /// `current_task()` becomes `None`.
    pub fn terminate_current(&mut self) -> Result<TaskId, SchedulerError> {
        self.transition_current(TaskState::Terminated)
    }

    fn transition_current(&mut self, next_state: TaskState) -> Result<TaskId, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::NotStarted)?;
        let task = self.task_mut(current)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidState);
        }
        if task.is_idle && next_state != TaskState::Ready {
            return Err(SchedulerError::InvalidState);
        }
        task.state = next_state;
        if next_state != TaskState::Sleeping {
            task.wake_at = None;
        }
        self.current = None;
        self.select_next((current + 1) % MAX_TASKS)
    }

    fn sleep_current_until(&mut self, deadline: u64) -> Result<TaskId, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::NotStarted)?;
        let task = self.task_mut(current)?;
        if task.state != TaskState::Running || task.is_idle {
            return Err(SchedulerError::InvalidState);
        }
        task.state = TaskState::Sleeping;
        task.wake_at = Some(deadline);
        self.current = None;
        self.select_next((current + 1) % MAX_TASKS)
    }

    fn wake_expired_sleepers(&mut self, now: u64) {
        for task in self.tasks.iter_mut().flatten() {
            if task.state == TaskState::Sleeping
                && task
                    .wake_at
                    .is_some_and(|deadline| now.wrapping_sub(deadline) < (1u64 << 63))
            {
                task.state = TaskState::Ready;
                task.wake_at = None;
            }
        }
    }

    fn has_live_non_idle_tasks(&self) -> bool {
        self.tasks
            .iter()
            .flatten()
            .any(|task| !task.is_idle && task.state != TaskState::Terminated)
    }

    pub fn wake(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        let task = self
            .tasks
            .iter_mut()
            .flatten()
            .find(|task| task.id == id)
            .ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Blocked {
            return Err(SchedulerError::InvalidState);
        }
        task.state = TaskState::Ready;
        Ok(())
    }

    pub fn save_current_context(&mut self, context: CpuContext) -> Result<(), SchedulerError> {
        let current = self.current.ok_or(SchedulerError::NotStarted)?;
        self.task_mut(current)?.context = context;
        Ok(())
    }

    pub fn reap_terminated(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        let index = self
            .tasks
            .iter()
            .position(|task| task.as_ref().is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::UnknownTask)?;
        if self.current == Some(index)
            || self.tasks[index]
                .as_ref()
                .is_none_or(|task| task.state != TaskState::Terminated)
        {
            return Err(SchedulerError::InvalidState);
        }
        self.tasks[index] = None;
        Ok(())
    }

    pub fn current_task(&self) -> Option<TaskId> {
        self.current
            .and_then(|index| self.tasks[index].as_ref().map(|task| task.id))
    }

    pub fn task_state(&self, id: TaskId) -> Result<TaskState, SchedulerError> {
        self.task(id).map(|task| task.state)
    }

    pub fn task_context(&self, id: TaskId) -> Result<CpuContext, SchedulerError> {
        self.task(id).map(|task| task.context)
    }

    fn select_next(&mut self, start: usize) -> Result<TaskId, SchedulerError> {
        // Prefer ordinary runnable tasks. The idle task is a permanent fallback.
        self.current = None;
        for pass in 0..2 {
            for offset in 0..MAX_TASKS {
                let index = (start + offset) % MAX_TASKS;
                if let Some(task) = self.tasks[index].as_mut()
                    && task.state == TaskState::Ready
                    && ((pass == 0 && !task.is_idle) || (pass == 1 && task.is_idle))
                {
                    task.state = TaskState::Running;
                    self.current = Some(index);
                    return Ok(task.id);
                }
            }
        }
        Err(SchedulerError::NoRunnableTasks)
    }

    fn task(&self, id: TaskId) -> Result<&Task, SchedulerError> {
        self.tasks
            .iter()
            .flatten()
            .find(|task| task.id == id)
            .ok_or(SchedulerError::UnknownTask)
    }

    fn task_mut(&mut self, index: usize) -> Result<&mut Task, SchedulerError> {
        self.tasks[index]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    Full,
    AlreadyStarted,
    NotStarted,
    NoRunnableTasks,
    UnknownTask,
    InvalidState,
    InvalidContext,
    TaskIdExhausted,
}

static GLOBAL_SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());
static mut BOOTSTRAP_CONTEXT: CpuContext = CpuContext::empty();

core::arch::global_asm!(
    ".global leafos_context_switch",
    "leafos_context_switch:",
    "mov [rdi + 8], rbx",
    "mov [rdi + 16], rbp",
    "mov [rdi + 24], r12",
    "mov [rdi + 32], r13",
    "mov [rdi + 40], r14",
    "mov [rdi + 48], r15",
    "mov [rdi], rsp",
    "pushfq",
    "pop rax",
    "mov [rdi + 56], rax",
    "mov rax, [rsi + 56]",
    "mov rbx, [rsi + 8]",
    "mov rbp, [rsi + 16]",
    "mov r12, [rsi + 24]",
    "mov r13, [rsi + 32]",
    "mov r14, [rsi + 40]",
    "mov r15, [rsi + 48]",
    "mov rsp, [rsi]",
    "push rax",
    "popfq",
    "ret",
);

unsafe extern "C" {
    fn leafos_context_switch(old: *mut CpuContext, new: *const CpuContext);
}

pub fn spawn(entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
    spawn_into(&mut GLOBAL_SCHEDULER.lock(), entry)
}

fn spawn_into(scheduler: &mut Scheduler, entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
    // Keep one slot available for the permanent idle task until it is installed.
    if scheduler.idle_index.is_none()
        && scheduler.tasks.iter().filter(|slot| slot.is_none()).count() <= 1
    {
        return Err(SchedulerError::Full);
    }
    scheduler.create_kernel_task(entry)
}

pub fn run_tasks() -> Result<(), SchedulerError> {
    let next_context = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        if scheduler.idle_index.is_none() {
            let id = scheduler.create_kernel_task(idle_task_entry)?;
            let index = scheduler
                .tasks
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|task| task.id == id))
                .ok_or(SchedulerError::UnknownTask)?;
            scheduler.tasks[index]
                .as_mut()
                .ok_or(SchedulerError::UnknownTask)?
                .is_idle = true;
            scheduler.idle_index = Some(index);
        }
        scheduler.start()?;
        let index = scheduler.current.ok_or(SchedulerError::NotStarted)?;
        &scheduler.tasks[index]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *const CpuContext
    };
    let bootstrap = &raw mut BOOTSTRAP_CONTEXT;
    unsafe { leafos_context_switch(bootstrap, next_context) };
    let idle_task = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        match scheduler.idle_index {
            Some(index)
                if scheduler.current.is_none()
                    && scheduler.tasks[index]
                        .as_ref()
                        .is_some_and(|task| task.is_idle && task.state == TaskState::Ready) =>
            {
                scheduler.idle_index = None;
                scheduler.tasks[index].take()
            }
            _ => None,
        }
    };
    drop(idle_task);
    Ok(())
}

pub fn yield_now() -> Result<(), SchedulerError> {
    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler.current.ok_or(SchedulerError::NotStarted)?;
        scheduler.transition_current(TaskState::Ready)?;
        let next = scheduler.current.ok_or(SchedulerError::NoRunnableTasks)?;
        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *mut CpuContext;
        let next_context = &scheduler.tasks[next]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *const CpuContext;
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    Ok(())
}

/// Rotate away from the interrupted task after a timer tick.
///
/// The interrupt handler's compiler-generated register saves and hardware
/// interrupt frame remain on the interrupted task's stack. Switching from
/// this call boundary suspends that entire frame until the task is resumed.
pub(crate) fn preempt_from_timer() {
    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let Some(current) = scheduler.current else {
            return;
        };

        scheduler.wake_expired_sleepers(crate::time::timer::ticks());
        match scheduler.transition_current(TaskState::Ready) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => panic!("timer preemption failed: {:?}", error),
        }

        let Some(next) = scheduler.current else {
            return;
        };
        if next == current {
            return;
        }

        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .expect("interrupted task slot is empty")
            .context as *mut CpuContext;
        let next_context = &scheduler.tasks[next]
            .as_ref()
            .expect("selected task slot is empty")
            .context as *const CpuContext;
        (old_context, next_context)
    };

    unsafe { leafos_context_switch(old_context, next_context) };
}

/// Block the current kernel task. The idle task runs if no other task is ready;
/// when this task is later woken and scheduled again, this call resumes and
/// returns `Ok(())`.
pub fn block_now() -> Result<(), SchedulerError> {
    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler.current.ok_or(SchedulerError::NotStarted)?;

        match scheduler.transition_current(TaskState::Blocked) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => return Err(error),
        }

        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *mut CpuContext;
        let next_context = match scheduler.current {
            Some(next) => {
                &scheduler.tasks[next]
                    .as_ref()
                    .ok_or(SchedulerError::UnknownTask)?
                    .context as *const CpuContext
            }
            None => &raw const BOOTSTRAP_CONTEXT,
        };
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    Ok(())
}

/// Sleep the current task for a number of PIT ticks without polling.
/// The idle task wakes expired sleepers after each timer interrupt.
pub fn sleep_ticks(duration: u64) -> Result<(), SchedulerError> {
    if duration == 0 {
        return Ok(());
    }
    let deadline = crate::time::timer::ticks().wrapping_add(duration);
    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler.current.ok_or(SchedulerError::NotStarted)?;
        match scheduler.sleep_current_until(deadline) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => return Err(error),
        }
        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *mut CpuContext;
        let next_context = match scheduler.current {
            Some(next) => {
                &scheduler.tasks[next]
                    .as_ref()
                    .ok_or(SchedulerError::UnknownTask)?
                    .context as *const CpuContext
            }
            None => &raw const BOOTSTRAP_CONTEXT,
        };
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    Ok(())
}

fn idle_task_entry() {
    loop {
        let context_to_bootstrap = {
            let mut scheduler = GLOBAL_SCHEDULER.lock();
            scheduler.wake_expired_sleepers(crate::time::timer::ticks());
            if !scheduler.has_live_non_idle_tasks() {
                let current = scheduler.current.expect("idle task without current task");
                let idle = scheduler.tasks[current]
                    .as_mut()
                    .expect("current idle task slot is empty");
                assert!(idle.is_idle, "non-idle task entered idle loop");
                idle.state = TaskState::Ready;
                scheduler.current = None;
                Some(
                    &mut scheduler.tasks[current]
                        .as_mut()
                        .expect("current idle task slot is empty")
                        .context as *mut CpuContext,
                )
            } else {
                None
            }
        };
        if let Some(idle_context) = context_to_bootstrap {
            unsafe {
                leafos_context_switch(idle_context, &raw const BOOTSTRAP_CONTEXT);
            }
            continue;
        }
        // Hand control to any task that just became runnable before idling.
        let _ = yield_now();
        // Interrupts are enabled atomically with halting, so a PIT tick cannot
        // be lost between checking deadlines and sleeping the CPU.
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

/// Wake a blocked task. Waking changes its state to `Ready` but does not
/// preempt a currently running task. If the scheduler is idle, call
/// `run_tasks()` to resume scheduling from the bootstrap context.
pub fn wake_task(id: TaskId) -> Result<(), SchedulerError> {
    GLOBAL_SCHEDULER.lock().wake(id)
}

/// Terminate the current kernel task without returning to its entry function.
/// The terminated task can be released later with `reap_terminated()`.
pub fn exit_current() -> ! {
    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler.current.expect("task exit without current task");

        match scheduler.transition_current(TaskState::Terminated) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => panic!("scheduler failed to terminate current task: {:?}", error),
        }

        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .expect("terminated task slot is empty")
            .context as *mut CpuContext;
        let next_context = match scheduler.current {
            Some(next) => {
                &scheduler.tasks[next]
                    .as_ref()
                    .expect("selected task slot is empty")
                    .context as *const CpuContext
            }
            None => &raw const BOOTSTRAP_CONTEXT,
        };
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    unreachable!("a terminated task must never resume")
}

pub fn task_state(id: TaskId) -> Result<TaskState, SchedulerError> {
    GLOBAL_SCHEDULER.lock().task_state(id)
}

pub fn reap_terminated(id: TaskId) -> Result<(), SchedulerError> {
    GLOBAL_SCHEDULER.lock().reap_terminated(id)
}

extern "C" fn task_trampoline() -> ! {
    let entry = {
        let scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler
            .current
            .expect("task trampoline without current task");
        scheduler.tasks[current]
            .as_ref()
            .and_then(|task| task.entry)
            .expect("kernel task missing entry")
    };
    entry();
    exit_current()
}

#[test_case]
fn scheduler_sleep_deadlines_wake_only_when_expired() {
    let mut scheduler = Scheduler::new();
    let task = scheduler
        .create_task(CpuContext::empty())
        .expect("create task");
    assert_eq!(scheduler.start(), Ok(task));
    assert_eq!(
        scheduler.sleep_current_until(10),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.task_state(task), Ok(TaskState::Sleeping));
    scheduler.wake_expired_sleepers(9);
    assert_eq!(scheduler.task_state(task), Ok(TaskState::Sleeping));
    scheduler.wake_expired_sleepers(10);
    assert_eq!(scheduler.task_state(task), Ok(TaskState::Ready));
    assert_eq!(scheduler.start(), Ok(task));
}

#[test_case]
fn scheduler_rotates_ready_tasks_in_round_robin_order() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");
    let third = scheduler
        .create_task(CpuContext::empty())
        .expect("create third task");

    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.yield_current(), Ok(second));
    assert_eq!(scheduler.yield_current(), Ok(third));
    assert_eq!(scheduler.yield_current(), Ok(first));
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Running));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Ready));
}

#[test_case]
fn scheduler_blocks_wakes_and_terminates_tasks() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");

    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.block_current(), Ok(second));
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Blocked));
    assert_eq!(scheduler.wake(first), Ok(()));
    assert_eq!(scheduler.terminate_current(), Ok(first));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Terminated));
    assert_eq!(
        scheduler.terminate_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.current_task(), None);
}

#[test_case]
fn scheduler_rejects_invalid_transitions_and_capacity_overflow() {
    let mut scheduler = Scheduler::new();
    let id = scheduler
        .create_task(CpuContext::empty())
        .expect("create task");

    assert_eq!(scheduler.start(), Ok(id));
    assert_eq!(scheduler.start(), Err(SchedulerError::AlreadyStarted));
    assert_eq!(scheduler.wake(id), Err(SchedulerError::InvalidState));

    for _ in 1..MAX_TASKS {
        scheduler
            .create_task(CpuContext::empty())
            .expect("fill scheduler");
    }
    assert_eq!(
        scheduler.create_task(CpuContext::empty()),
        Err(SchedulerError::Full)
    );
}

#[test_case]
fn scheduler_saves_context_and_reuses_reaped_slots_without_reusing_ids() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    assert_eq!(scheduler.start(), Ok(first));
    let context = CpuContext {
        stack_pointer: 0x1000,
        rbx: 1,
        rbp: 2,
        r12: 3,
        r13: 4,
        r14: 5,
        r15: 6,
        rflags: 0x202,
    };
    assert_eq!(scheduler.save_current_context(context), Ok(()));
    assert_eq!(scheduler.task_context(first), Ok(context));
    assert_eq!(
        scheduler.terminate_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.reap_terminated(first), Ok(()));

    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("reuse task slot");
    assert_ne!(first, second);
    assert_eq!(second.get(), first.get() + 1);
    assert_eq!(
        scheduler.task_state(first),
        Err(SchedulerError::UnknownTask)
    );
}

#[test_case]
fn scheduler_stays_consistent_when_no_task_is_runnable() {
    let mut scheduler = Scheduler::new();
    assert_eq!(scheduler.start(), Err(SchedulerError::NoRunnableTasks));
    assert_eq!(scheduler.current_task(), None);
    assert_scheduler_invariants(&scheduler);

    let task = scheduler
        .create_task(CpuContext::empty())
        .expect("create task");
    assert_eq!(scheduler.start(), Ok(task));
    assert_eq!(
        scheduler.block_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.current_task(), None);
    assert_eq!(scheduler.task_state(task), Ok(TaskState::Blocked));
    assert_eq!(scheduler.start(), Err(SchedulerError::NoRunnableTasks));
    assert_scheduler_invariants(&scheduler);

    assert_eq!(scheduler.wake(task), Ok(()));
    assert_eq!(scheduler.task_state(task), Ok(TaskState::Ready));
    assert_eq!(scheduler.start(), Ok(task));
    assert_eq!(
        scheduler.terminate_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.current_task(), None);
    assert_eq!(scheduler.task_state(task), Ok(TaskState::Terminated));
    assert_scheduler_invariants(&scheduler);

    assert_eq!(scheduler.reap_terminated(task), Ok(()));
    assert_eq!(scheduler.task_state(task), Err(SchedulerError::UnknownTask));
    assert_scheduler_invariants(&scheduler);
}

#[test_case]
fn scheduler_can_wake_tasks_while_idle_and_resume_round_robin_scheduling() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");

    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.block_current(), Ok(second));
    assert_eq!(
        scheduler.block_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.current_task(), None);
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Blocked));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Blocked));
    assert_scheduler_invariants(&scheduler);

    assert_eq!(scheduler.wake(first), Ok(()));
    assert_eq!(scheduler.current_task(), None);
    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.wake(second), Ok(()));
    assert_eq!(scheduler.yield_current(), Ok(second));
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Ready));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Running));
    assert_scheduler_invariants(&scheduler);
}

#[test_case]
fn scheduler_rejects_invalid_wake_reap_and_prestart_transitions() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");

    assert_eq!(scheduler.yield_current(), Err(SchedulerError::NotStarted));
    assert_eq!(scheduler.block_current(), Err(SchedulerError::NotStarted));
    assert_eq!(
        scheduler.terminate_current(),
        Err(SchedulerError::NotStarted)
    );
    assert_eq!(
        scheduler.save_current_context(CpuContext::empty()),
        Err(SchedulerError::NotStarted)
    );
    assert_eq!(scheduler.wake(first), Err(SchedulerError::InvalidState));
    assert_eq!(
        scheduler.wake(TaskId(u64::MAX)),
        Err(SchedulerError::UnknownTask)
    );
    assert_eq!(
        scheduler.reap_terminated(first),
        Err(SchedulerError::InvalidState)
    );

    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.wake(first), Err(SchedulerError::InvalidState));
    assert_eq!(
        scheduler.reap_terminated(first),
        Err(SchedulerError::InvalidState)
    );
    assert_eq!(scheduler.start(), Err(SchedulerError::AlreadyStarted));
    assert_eq!(scheduler.block_current(), Ok(second));
    assert_eq!(scheduler.wake(first), Ok(()));
    assert_eq!(scheduler.wake(first), Err(SchedulerError::InvalidState));
    assert_eq!(
        scheduler.reap_terminated(first),
        Err(SchedulerError::InvalidState)
    );
    assert_scheduler_invariants(&scheduler);
}

#[test_case]
fn scheduler_retains_terminated_tasks_until_reaped_and_then_reuses_the_slot() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");

    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.terminate_current(), Ok(second));
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Terminated));
    assert_eq!(scheduler.wake(first), Err(SchedulerError::InvalidState));
    assert_eq!(scheduler.reap_terminated(first), Ok(()));
    assert_eq!(
        scheduler.task_state(first),
        Err(SchedulerError::UnknownTask)
    );

    assert_eq!(
        scheduler.terminate_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Terminated));
    assert_eq!(scheduler.reap_terminated(second), Ok(()));
    assert_eq!(
        scheduler.reap_terminated(second),
        Err(SchedulerError::UnknownTask)
    );

    let replacement = scheduler
        .create_task(CpuContext::empty())
        .expect("reuse released slot");
    assert_eq!(replacement.get(), second.get() + 1);
    assert_ne!(replacement, first);
    assert_ne!(replacement, second);
    assert_eq!(scheduler.start(), Ok(replacement));
    assert_scheduler_invariants(&scheduler);
}

#[test_case]
fn scheduler_does_not_consume_task_ids_when_capacity_is_full_or_ids_are_exhausted() {
    let mut scheduler = Scheduler::new();
    for _ in 0..MAX_TASKS {
        scheduler
            .create_task(CpuContext::empty())
            .expect("fill scheduler");
    }
    let next_id = scheduler.next_id;
    assert_eq!(
        scheduler.create_task(CpuContext::empty()),
        Err(SchedulerError::Full)
    );
    assert_eq!(scheduler.next_id, next_id);

    let mut exhausted = Scheduler::new();
    exhausted.next_id = u64::MAX;
    assert_eq!(
        exhausted.create_task(CpuContext::empty()),
        Err(SchedulerError::TaskIdExhausted)
    );
    assert_eq!(exhausted.next_id, u64::MAX);
    assert_eq!(
        exhausted.tasks.iter().filter(|task| task.is_some()).count(),
        0
    );
}

#[test_case]
fn scheduler_preflights_kernel_task_capacity_and_id_before_stack_allocation() {
    let mut full = Scheduler::new();
    for _ in 0..MAX_TASKS {
        full.create_task(CpuContext::empty())
            .expect("fill scheduler");
    }
    assert_eq!(full.check_task_capacity(), Err(SchedulerError::Full));
    assert_eq!(
        spawn_into(&mut full, dummy_kernel_task),
        Err(SchedulerError::Full)
    );

    let mut exhausted = Scheduler::new();
    exhausted.next_id = u64::MAX;
    assert_eq!(
        spawn_into(&mut exhausted, dummy_kernel_task),
        Err(SchedulerError::TaskIdExhausted)
    );
    assert_eq!(
        exhausted.tasks.iter().filter(|task| task.is_some()).count(),
        0
    );
}

fn dummy_kernel_task() {}

fn assert_scheduler_invariants(scheduler: &Scheduler) {
    let running_count = scheduler
        .tasks
        .iter()
        .filter(|task| {
            task.as_ref()
                .is_some_and(|task| task.state == TaskState::Running)
        })
        .count();

    match scheduler.current {
        Some(index) => {
            assert_eq!(running_count, 1);
            assert_eq!(
                scheduler.tasks[index].as_ref().map(|task| task.state),
                Some(TaskState::Running)
            );
        }
        None => assert_eq!(running_count, 0),
    }
}

#[test_case]
fn scheduler_allows_task_creation_while_another_task_is_running() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    assert_eq!(scheduler.start(), Ok(first));

    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create task while scheduler is running");
    assert_eq!(scheduler.current_task(), Some(first));
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Running));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Ready));
    assert_eq!(scheduler.yield_current(), Ok(second));
    assert_eq!(scheduler.task_state(first), Ok(TaskState::Ready));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Running));
    assert_scheduler_invariants(&scheduler);
}


static SLEEP_TEST_STARTED: AtomicBool = AtomicBool::new(false);
static SLEEP_TEST_OTHER_RAN: AtomicBool = AtomicBool::new(false);
static SLEEP_TEST_FINISHED: AtomicBool = AtomicBool::new(false);
static SLEEP_TEST_TASK_ID: AtomicU64 = AtomicU64::new(0);
static SLEEP_TEST_OTHER_TASK_ID: AtomicU64 = AtomicU64::new(0);
static SLEEP_TEST_START_TICK: AtomicU64 = AtomicU64::new(0);
static SLEEP_TEST_RESUME_TICK: AtomicU64 = AtomicU64::new(0);

const SLEEP_TEST_DURATION: u64 = 5;

fn sleep_ticks_test_sleeper() {
    let start = crate::time::timer::ticks();

    SLEEP_TEST_START_TICK.store(start, Ordering::SeqCst);
    SLEEP_TEST_STARTED.store(true, Ordering::SeqCst);

    // This must context-switch to the other task instead of polling.
    sleep_ticks(SLEEP_TEST_DURATION).expect("sleep_ticks should succeed");

    let resumed = crate::time::timer::ticks();
    SLEEP_TEST_RESUME_TICK.store(resumed, Ordering::SeqCst);

    assert!(
        SLEEP_TEST_OTHER_RAN.load(Ordering::SeqCst),
        "another task should run while the sleeper sleeps"
    );

    let deadline = start.wrapping_add(SLEEP_TEST_DURATION);
    assert!(
        resumed.wrapping_sub(deadline) < (1u64 << 63),
        "sleeper resumed before its deadline"
    );

    SLEEP_TEST_FINISHED.store(true, Ordering::SeqCst);
}

fn sleep_ticks_test_other_task() {
    assert!(
        SLEEP_TEST_STARTED.load(Ordering::SeqCst),
        "sleeper should run first"
    );

    assert!(
        !SLEEP_TEST_FINISHED.load(Ordering::SeqCst),
        "sleeper must not finish before the other task runs"
    );

    let sleeper = TaskId(SLEEP_TEST_TASK_ID.load(Ordering::SeqCst));

    assert_eq!(
        task_state(sleeper),
        Ok(TaskState::Sleeping),
        "sleeper should be sleeping while this task runs"
    );

    SLEEP_TEST_OTHER_RAN.store(true, Ordering::SeqCst);

    // Terminate this task and let the idle task wait for the deadline.
    exit_current()
}

#[test_case]
fn scheduler_sleep_ticks_switches_tasks_and_resumes_after_deadline() {
    SLEEP_TEST_STARTED.store(false, Ordering::SeqCst);
    SLEEP_TEST_OTHER_RAN.store(false, Ordering::SeqCst);
    SLEEP_TEST_FINISHED.store(false, Ordering::SeqCst);
    SLEEP_TEST_TASK_ID.store(0, Ordering::SeqCst);
    SLEEP_TEST_OTHER_TASK_ID.store(0, Ordering::SeqCst);
    SLEEP_TEST_START_TICK.store(0, Ordering::SeqCst);
    SLEEP_TEST_RESUME_TICK.store(0, Ordering::SeqCst);

    let sleeper =
        spawn(sleep_ticks_test_sleeper).expect("spawn sleeping task");

    SLEEP_TEST_TASK_ID.store(sleeper.get(), Ordering::SeqCst);

    let other =
        spawn(sleep_ticks_test_other_task).expect("spawn second task");

    SLEEP_TEST_OTHER_TASK_ID.store(other.get(), Ordering::SeqCst);

    // Run actual kernel tasks, context switches, and PIT-driven wakeups.
    run_tasks().expect("run scheduled tasks");

    assert!(
        SLEEP_TEST_STARTED.load(Ordering::SeqCst),
        "sleeper should have started"
    );
    assert!(
        SLEEP_TEST_OTHER_RAN.load(Ordering::SeqCst),
        "the other task should have run while the sleeper slept"
    );
    assert!(
        SLEEP_TEST_FINISHED.load(Ordering::SeqCst),
        "sleeper should resume and finish"
    );

    let start = SLEEP_TEST_START_TICK.load(Ordering::SeqCst);
    let resumed = SLEEP_TEST_RESUME_TICK.load(Ordering::SeqCst);
    let deadline = start.wrapping_add(SLEEP_TEST_DURATION);

    assert!(
        resumed.wrapping_sub(deadline) < (1u64 << 63),
        "sleeper must finish no earlier than its deadline"
    );

    // run_tasks returns after all non-idle tasks terminate.
    assert_eq!(
        task_state(sleeper),
        Ok(TaskState::Terminated),
        "sleeper should be terminated after finishing"
    );
    assert_eq!(
        task_state(other),
        Ok(TaskState::Terminated),
        "the second task should be terminated"
    );

    // Release the global scheduler slots for later tests.
    reap_terminated(sleeper).expect("reap sleeping task");
    reap_terminated(other).expect("reap second task");
}
