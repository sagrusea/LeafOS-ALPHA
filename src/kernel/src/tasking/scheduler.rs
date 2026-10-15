use alloc::boxed::Box;
use alloc::vec;
use spin::Mutex;

pub const MAX_TASKS: usize = 16;
const KERNEL_STACK_SIZE: usize = 16 * 1024;

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
        }
    }
}

struct Task {
    id: TaskId,
    state: TaskState,
    context: CpuContext,
    entry: Option<KernelTaskEntry>,
    _stack: Option<Box<[u64]>>,
}

pub struct Scheduler {
    tasks: [Option<Task>; MAX_TASKS],
    current: Option<usize>,
    next_id: u64,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: [const { None }; MAX_TASKS],
            current: None,
            next_id: 1,
        }
    }

    pub fn create_task(&mut self, context: CpuContext) -> Result<TaskId, SchedulerError> {
        self.insert_task(context, None, None)
    }

    fn create_kernel_task(&mut self, entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
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
        self.insert_task(context, Some(entry), Some(stack))
    }

    fn insert_task(
        &mut self,
        context: CpuContext,
        entry: Option<KernelTaskEntry>,
        stack: Option<Box<[u64]>>,
    ) -> Result<TaskId, SchedulerError> {
        let slot = self
            .tasks
            .iter()
            .position(Option::is_none)
            .ok_or(SchedulerError::Full)?;
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
        });
        Ok(id)
    }

    pub fn start(&mut self) -> Result<TaskId, SchedulerError> {
        if self.current.is_some() {
            return Err(SchedulerError::AlreadyStarted);
        }
        self.select_next(0)
    }

    pub fn yield_current(&mut self) -> Result<TaskId, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::NotStarted)?;
        self.task_mut(current)?.state = TaskState::Ready;
        self.current = None;
        self.select_next((current + 1) % MAX_TASKS)
    }

    pub fn block_current(&mut self) -> Result<TaskId, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::NotStarted)?;
        self.task_mut(current)?.state = TaskState::Blocked;
        self.current = None;
        self.select_next((current + 1) % MAX_TASKS)
    }

    pub fn terminate_current(&mut self) -> Result<TaskId, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::NotStarted)?;
        self.task_mut(current)?.state = TaskState::Terminated;
        self.current = None;
        self.select_next((current + 1) % MAX_TASKS)
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
        for offset in 0..MAX_TASKS {
            let index = (start + offset) % MAX_TASKS;
            if let Some(task) = self.tasks[index].as_mut()
                && task.state == TaskState::Ready
            {
                task.state = TaskState::Running;
                self.current = Some(index);
                return Ok(task.id);
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
    "mov rbx, [rsi + 8]",
    "mov rbp, [rsi + 16]",
    "mov r12, [rsi + 24]",
    "mov r13, [rsi + 32]",
    "mov r14, [rsi + 40]",
    "mov r15, [rsi + 48]",
    "mov rsp, [rsi]",
    "ret",
);

unsafe extern "C" {
    fn leafos_context_switch(old: *mut CpuContext, new: *const CpuContext);
}

pub fn spawn(entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
    GLOBAL_SCHEDULER.lock().create_kernel_task(entry)
}

pub fn run_tasks() -> Result<(), SchedulerError> {
    let next_context = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        scheduler.start()?;
        let index = scheduler.current.ok_or(SchedulerError::NotStarted)?;
        &scheduler.tasks[index]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *const CpuContext
    };
    let bootstrap = &raw mut BOOTSTRAP_CONTEXT;
    unsafe { leafos_context_switch(bootstrap, next_context) };
    Ok(())
}

pub fn yield_now() -> Result<(), SchedulerError> {
    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler.current.ok_or(SchedulerError::NotStarted)?;
        scheduler.task_mut(current)?.state = TaskState::Ready;
        scheduler.current = None;
        scheduler.select_next((current + 1) % MAX_TASKS)?;
        let next = scheduler.current.ok_or(SchedulerError::NotStarted)?;
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

    let (old_context, next_context) = {
        let mut scheduler = GLOBAL_SCHEDULER.lock();
        let current = scheduler.current.expect("task exit without current task");
        scheduler.tasks[current]
            .as_mut()
            .expect("current task slot is empty")
            .state = TaskState::Terminated;
        scheduler.current = None;
        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .expect("terminated task slot is empty")
            .context as *mut CpuContext;
        let next_context = match scheduler.select_next((current + 1) % MAX_TASKS) {
            Ok(_) => {
                let next = scheduler.current.expect("selected task missing");
                &scheduler.tasks[next]
                    .as_ref()
                    .expect("selected task slot is empty")
                    .context as *const CpuContext
            }
            Err(SchedulerError::NoRunnableTasks) => &raw const BOOTSTRAP_CONTEXT,
            Err(error) => panic!("scheduler failed to select next task: {:?}", error),
        };
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    unreachable!()
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
