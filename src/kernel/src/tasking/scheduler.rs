use crate::arch::user::{UserContext, UserContextError};
use crate::memory::{self, AddressSpace};
#[cfg(test)]
use crate::serial_println;
use crate::sync::Mutex;
use crate::tasking::fd::{self, FileDescriptor, MAX_FDS};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::VirtAddr;

pub const PER_CORE_MAX_TASKS: usize = 8;
pub const MAX_TASKS: usize = PER_CORE_MAX_TASKS * crate::arch::syscall::MAX_CPUS;
const KERNEL_STACK_SIZE: usize = 64 * 1024;
const STACK_ALIGNMENT: usize = 16;

pub type KernelTaskEntry = fn();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdMapping {
    pub source: usize,
    pub target: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(u64);

impl TaskId {
    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn from_raw(value: u64) -> Self {
        Self(value)
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
pub struct TaskInfo {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    pub state: TaskState,
    pub exit_code: Option<u64>,
    pub is_idle: bool,
    pub is_user: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSchedulerStats {
    pub context_switches: u64,
    pub timer_preemptions: u64,
    pub idle_ticks: u64,
    pub tasks_executed: u64,
    pub runnable_tasks: usize,
    pub steal_attempts: u64,
    pub successful_steals: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerStats {
    pub cpus: [Option<CpuSchedulerStats>; crate::arch::syscall::MAX_CPUS],
    pub work_stealing_events: u64,
}

struct CpuCounters {
    context_switches: AtomicU64,
    timer_preemptions: AtomicU64,
    idle_ticks: AtomicU64,
    tasks_executed: AtomicU64,
    steal_attempts: AtomicU64,
    successful_steals: AtomicU64,
}

impl CpuCounters {
    const fn new() -> Self {
        Self {
            context_switches: AtomicU64::new(0),
            timer_preemptions: AtomicU64::new(0),
            idle_ticks: AtomicU64::new(0),
            tasks_executed: AtomicU64::new(0),
            steal_attempts: AtomicU64::new(0),
            successful_steals: AtomicU64::new(0),
        }
    }
}

static SCHEDULER_STATS: [CpuCounters; crate::arch::syscall::MAX_CPUS] =
    [const { CpuCounters::new() }; crate::arch::syscall::MAX_CPUS];
static TOTAL_SUCCESSFUL_STEALS: AtomicU64 = AtomicU64::new(0);
static STEAL_VICTIM_CURSOR: [AtomicU64; crate::arch::syscall::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::arch::syscall::MAX_CPUS];
static LAST_STEAL_TICK: [AtomicU64; crate::arch::syscall::MAX_CPUS] =
    [const { AtomicU64::new(u64::MAX) }; crate::arch::syscall::MAX_CPUS];
// Prevent task-ID lookups from observing a gap while ownership moves between queues.
static TASK_MIGRATION_LOCK: Mutex<()> = Mutex::new(());
const STEAL_RETRY_TICKS: u64 = 10;

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

const _: () = {
    assert!(core::mem::offset_of!(CpuContext, stack_pointer) == 0);
    assert!(core::mem::offset_of!(CpuContext, rbx) == 8);
    assert!(core::mem::offset_of!(CpuContext, rbp) == 16);
    assert!(core::mem::offset_of!(CpuContext, r12) == 24);
    assert!(core::mem::offset_of!(CpuContext, r13) == 32);
    assert!(core::mem::offset_of!(CpuContext, r14) == 40);
    assert!(core::mem::offset_of!(CpuContext, r15) == 48);
    assert!(core::mem::offset_of!(CpuContext, rflags) == 56);
};

struct Task {
    id: TaskId,
    parent: Option<TaskId>,
    state: TaskState,
    exit_code: Option<u64>,
    context: CpuContext,
    entry: Option<KernelTaskEntry>,
    user: Option<UserTask>,
    _stack: Option<Box<[u128]>>,
    stack_top: Option<VirtAddr>,
    wake_at: Option<u64>,
    is_idle: bool,
    kill_pending: bool,
    wake_pending: bool,
    waiting_for_child: bool,
    auto_reap: bool,
    switch_pending_cpu: Option<usize>,
    cleanup_pending: bool,
    reaping: bool,
    // `None` is unpinned; `Some(cpu)` is an explicit CPU-affinity constraint.
    cpu_affinity: Option<usize>,
    fds: [Option<FileDescriptor>; MAX_FDS],
}

struct UserTask {
    address_space: AddressSpace,
    context: UserContext,
    syscall_user_rsp: u64,
}

fn save_current_user_rsp(scheduler: &mut Scheduler, current: usize) {
    if let Some(user) = scheduler.tasks[current]
        .as_mut()
        .and_then(|task| task.user.as_mut())
    {
        user.syscall_user_rsp = unsafe { crate::arch::syscall::user_rsp() };
    }
}

fn find_free_fd_pair<T>(fds: &[Option<T>; MAX_FDS]) -> Option<(usize, usize)> {
    let mut free_fds = fds
        .iter()
        .enumerate()
        .filter_map(|(index, fd)| fd.is_none().then_some(index));
    Some((free_fds.next()?, free_fds.next()?))
}

fn find_free_fd<T>(fds: &[Option<T>; MAX_FDS]) -> Option<usize> {
    fds.iter().position(Option::is_none)
}

fn apply_fd_mappings(
    fds: &mut [Option<FileDescriptor>; MAX_FDS],
    mappings: &[FdMapping],
    closes: &[usize],
) -> Result<(), SchedulerError> {
    for mapping in mappings {
        if mapping.source >= MAX_FDS || mapping.target >= MAX_FDS || fds[mapping.source].is_none() {
            return Err(SchedulerError::InvalidFd);
        }
    }
    if closes.iter().any(|&fd_number| fd_number >= MAX_FDS) {
        return Err(SchedulerError::InvalidFd);
    }

    let mut source_snapshot: [Option<FileDescriptor>; MAX_FDS] = core::array::from_fn(|_| None);
    for mapping in mappings {
        if source_snapshot[mapping.source].is_none() {
            source_snapshot[mapping.source] = fds[mapping.source].as_ref().cloned();
        }
    }

    for mapping in mappings {
        let descriptor = source_snapshot[mapping.source]
            .as_ref()
            .ok_or(SchedulerError::InvalidFd)?
            .clone();
        if let Some(previous) = fds[mapping.target].replace(descriptor) {
            fd::close(previous);
        }
    }

    for mapping in mappings {
        let source_is_destination = mappings
            .iter()
            .any(|candidate| candidate.target == mapping.source);
        if mapping.source != mapping.target
            && !source_is_destination
            && let Some(descriptor) = fds[mapping.source].take()
        {
            fd::close(descriptor);
        }
    }

    for &fd_number in closes {
        if let Some(descriptor) = fds[fd_number].take() {
            fd::close(descriptor);
        }
    }
    Ok(())
}

pub struct RunQueue {
    tasks: [Option<Task>; PER_CORE_MAX_TASKS],
    current: Option<usize>,
    next_id: u64,
    idle_index: Option<usize>,
}

type Scheduler = RunQueue;

impl RunQueue {
    pub const fn new() -> Self {
        Self {
            tasks: [const { None }; PER_CORE_MAX_TASKS],
            current: None,
            next_id: 1,
            idle_index: None,
        }
    }

    fn current_index(&self, _cpu_index: usize) -> Option<usize> {
        self.current
    }

    fn set_current_index(&mut self, _cpu_index: usize, task_index: Option<usize>) {
        self.current = task_index;
    }

    fn task_is_running(&self, task_index: usize) -> bool {
        self.current == Some(task_index)
    }

    pub fn create_task(&mut self, context: CpuContext) -> Result<TaskId, SchedulerError> {
        self.insert_task(context, None, None, None, None, None)
    }

    fn create_kernel_task(
        &mut self,
        entry: KernelTaskEntry,
        parent: Option<TaskId>,
    ) -> Result<TaskId, SchedulerError> {
        self.check_task_capacity()?;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(SchedulerError::TaskIdExhausted)?;
        let id = allocate_task_id()?;
        let (context, stack, stack_top) = create_task_stack()?;
        let task = self.insert_task_with_id(
            id,
            context,
            parent,
            Some(entry),
            None,
            Some(stack),
            Some(stack_top),
        );
        if task.is_ok() {
            self.next_id = next_id;
        }
        task
    }

    fn create_user_task(
        &mut self,
        address_space: &mut Option<AddressSpace>,
        context: UserContext,
        parent: Option<TaskId>,
        parent_fds: Option<[Option<FileDescriptor>; MAX_FDS]>,
        fd_mappings: &[FdMapping],
        fd_closes: &[usize],
    ) -> Result<TaskId, SchedulerError> {
        self.check_spawn_capacity()?;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or(SchedulerError::TaskIdExhausted)?;
        let mut fds = parent_fds.unwrap_or_else(fd::initial_table);
        apply_fd_mappings(&mut fds, fd_mappings, fd_closes)?;
        let id = allocate_task_id()?;
        let (cpu_context, stack, stack_top) = create_task_stack()?;
        let address_space = address_space.take().ok_or(SchedulerError::InvalidContext)?;
        let slot = self
            .check_spawn_capacity()
            .expect("reserved user task slot remains available");
        self.next_id = next_id;
        self.tasks[slot] = Some(Task {
            id,
            parent,
            state: TaskState::Ready,
            exit_code: None,
            context: cpu_context,
            entry: None,
            user: Some(UserTask {
                address_space,
                context,
                syscall_user_rsp: 0,
            }),
            _stack: Some(stack),
            stack_top: Some(stack_top),
            wake_at: None,
            is_idle: false,
            kill_pending: false,
            wake_pending: false,
            waiting_for_child: false,
            auto_reap: false,
            switch_pending_cpu: None,
            cleanup_pending: false,
            reaping: false,
            cpu_affinity: None,
            fds,
        });
        Ok(id)
    }

    fn check_task_capacity(&self) -> Result<usize, SchedulerError> {
        self.tasks
            .iter()
            .position(Option::is_none)
            .ok_or(SchedulerError::Full)
    }

    fn check_spawn_capacity(&self) -> Result<usize, SchedulerError> {
        let free = self.tasks.iter().filter(|task| task.is_none()).count();
        if self.idle_index.is_none() && free <= 1 {
            return Err(SchedulerError::Full);
        }
        self.check_task_capacity()
    }

    fn insert_task(
        &mut self,
        context: CpuContext,
        parent: Option<TaskId>,
        entry: Option<KernelTaskEntry>,
        user: Option<UserTask>,
        stack: Option<Box<[u128]>>,
        stack_top: Option<VirtAddr>,
    ) -> Result<TaskId, SchedulerError> {
        let slot = self.check_task_capacity()?;
        let id = TaskId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(SchedulerError::TaskIdExhausted)?;
        self.insert_task_at(slot, id, context, parent, entry, user, stack, stack_top);
        Ok(id)
    }

    fn insert_task_with_id(
        &mut self,
        id: TaskId,
        context: CpuContext,
        parent: Option<TaskId>,
        entry: Option<KernelTaskEntry>,
        user: Option<UserTask>,
        stack: Option<Box<[u128]>>,
        stack_top: Option<VirtAddr>,
    ) -> Result<TaskId, SchedulerError> {
        let slot = self.check_task_capacity()?;
        self.insert_task_at(slot, id, context, parent, entry, user, stack, stack_top);
        Ok(id)
    }

    fn insert_task_at(
        &mut self,
        slot: usize,
        id: TaskId,
        context: CpuContext,
        parent: Option<TaskId>,
        entry: Option<KernelTaskEntry>,
        user: Option<UserTask>,
        stack: Option<Box<[u128]>>,
        stack_top: Option<VirtAddr>,
    ) {
        self.tasks[slot] = Some(Task {
            id,
            parent,
            state: TaskState::Ready,
            exit_code: None,
            context,
            entry,
            user,
            _stack: stack,
            stack_top,
            wake_at: None,
            is_idle: false,
            kill_pending: false,
            wake_pending: false,
            waiting_for_child: false,
            auto_reap: false,
            switch_pending_cpu: None,
            cleanup_pending: false,
            reaping: false,
            cpu_affinity: None,
            fds: fd::initial_table(),
        });
    }

    pub fn start(&mut self) -> Result<TaskId, SchedulerError> {
        let cpu_index = current_runqueue_index();
        if self.current_index(cpu_index).is_some() {
            return Err(SchedulerError::AlreadyStarted);
        }
        self.select_next(0, cpu_index)
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
        let cpu_index = current_runqueue_index();
        let current = self
            .current_index(cpu_index)
            .ok_or(SchedulerError::NotStarted)?;
        let task = self.task_mut(current)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidState);
        }
        if task.is_idle && next_state != TaskState::Ready {
            return Err(SchedulerError::InvalidState);
        }
        task.state = next_state;
        if next_state == TaskState::Blocked && task.wake_pending {
            task.wake_pending = false;
            task.state = TaskState::Ready;
            task.waiting_for_child = false;
        }
        if next_state != TaskState::Sleeping {
            task.wake_at = None;
        }
        self.set_current_index(cpu_index, None);
        self.select_next((current + 1) % PER_CORE_MAX_TASKS, cpu_index)
    }

    fn mark_switch_pending(&mut self, index: usize, cpu_index: usize) {
        if self.current_index(cpu_index) != Some(index) {
            if let Some(task) = self.tasks[index].as_mut() {
                task.switch_pending_cpu = Some(cpu_index);
            }
        }
    }

    fn sleep_current_until(&mut self, deadline: u64) -> Result<TaskId, SchedulerError> {
        let cpu_index = current_runqueue_index();
        let current = self
            .current_index(cpu_index)
            .ok_or(SchedulerError::NotStarted)?;
        let task = self.task_mut(current)?;
        if task.state != TaskState::Running || task.is_idle {
            return Err(SchedulerError::InvalidState);
        }
        task.state = TaskState::Sleeping;
        task.wake_at = Some(deadline);
        self.set_current_index(cpu_index, None);
        self.select_next((current + 1) % PER_CORE_MAX_TASKS, cpu_index)
    }

    fn wake_expired_sleepers(&mut self, now: u64) -> bool {
        let mut woke_any = false;
        for task in self.tasks.iter_mut().flatten() {
            if task.state == TaskState::Sleeping
                && task
                    .wake_at
                    .is_some_and(|deadline| now.wrapping_sub(deadline) < (1u64 << 63))
            {
                task.state = TaskState::Ready;
                task.wake_at = None;
                woke_any = true;
            }
        }
        woke_any
    }

    #[cfg(test)]
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
        match task.state {
            TaskState::Blocked => {
                task.state = TaskState::Ready;
                task.waiting_for_child = false;
            }
            TaskState::Running if task.waiting_for_child => task.wake_pending = true,
            _ => return Err(SchedulerError::InvalidState),
        }
        Ok(())
    }

    pub fn terminate_task(&mut self, id: TaskId, exit_code: u64) -> Result<(), SchedulerError> {
        let index = self
            .tasks
            .iter()
            .position(|task| task.as_ref().is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::UnknownTask)?;
        if self.task_is_running(index) {
            return Err(SchedulerError::InvalidState);
        }
        let should_wake_parent = {
            let task = self.tasks[index]
                .as_mut()
                .ok_or(SchedulerError::UnknownTask)?;
            if task.is_idle || task.state == TaskState::Terminated {
                return Err(SchedulerError::InvalidState);
            }
            task.exit_code = Some(exit_code);
            task.state = TaskState::Terminated;
            task.cleanup_pending = true;
            task.switch_pending_cpu.is_none()
        };
        if should_wake_parent {
            self.wake_parent(index);
        }
        Ok(())
    }

    fn request_terminate_task(
        &mut self,
        id: TaskId,
        exit_code: u64,
    ) -> Result<bool, SchedulerError> {
        let index = self
            .tasks
            .iter()
            .position(|task| task.as_ref().is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::UnknownTask)?;
        let running = self.task_is_running(index);
        let (on_cpu, wake_parent) = {
            let task = self.tasks[index]
                .as_mut()
                .ok_or(SchedulerError::UnknownTask)?;
            if task.is_idle {
                return Err(SchedulerError::InvalidState);
            }
            task.auto_reap = true;
            if task.state == TaskState::Terminated {
                return Ok(false);
            }
            let switching_out = task.switch_pending_cpu.is_some();
            task.exit_code = Some(exit_code);
            task.cleanup_pending = true;
            if running {
                task.kill_pending = true;
            } else {
                task.state = TaskState::Terminated;
            }
            (running || switching_out, !running && !switching_out)
        };
        if wake_parent {
            self.wake_parent(index);
        }
        Ok(on_cpu)
    }

    fn terminate_group_member(
        &mut self,
        id: TaskId,
        exit_code: Option<u64>,
    ) -> Result<(Option<TaskId>, bool), SchedulerError> {
        let index = self
            .tasks
            .iter()
            .position(|task| task.as_ref().is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::UnknownTask)?;
        let running = self.task_is_running(index);
        let task = self.tasks[index]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?;
        if task.is_idle {
            return Err(SchedulerError::InvalidState);
        }
        if task.state == TaskState::Terminated {
            return Ok((task.parent, false));
        }
        task.exit_code = exit_code;
        task.cleanup_pending = true;
        let parent = task.parent;
        if running {
            task.kill_pending = true;
        } else {
            task.state = TaskState::Terminated;
            self.wake_parent(index);
        }
        Ok((parent, !running))
    }

    fn wake_all_blocked(&mut self) {
        for task in self.tasks.iter_mut().flatten() {
            if task.state == TaskState::Blocked {
                task.state = TaskState::Ready;
                task.waiting_for_child = false;
                task.wake_pending = false;
            }
        }
    }

    pub fn save_current_context(&mut self, context: CpuContext) -> Result<(), SchedulerError> {
        let current = self
            .current_index(current_runqueue_index())
            .ok_or(SchedulerError::NotStarted)?;
        self.task_mut(current)?.context = context;
        Ok(())
    }

    fn begin_reap(
        &mut self,
        id: TaskId,
        reserved: bool,
    ) -> Result<Option<UserTask>, SchedulerError> {
        let index = self
            .tasks
            .iter()
            .position(|task| task.as_ref().is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::UnknownTask)?;
        let is_running = self.task_is_running(index);
        let task = self.tasks[index]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?;
        if is_running
            || task.state != TaskState::Terminated
            || task.switch_pending_cpu.is_some()
            || task.reaping != reserved
        {
            return Err(SchedulerError::InvalidState);
        }
        if !reserved {
            task.reaping = true;
        }
        Ok(task.user.take())
    }

    fn abort_reap(&mut self, id: TaskId, user: Option<UserTask>) {
        if let Some(task) = self.tasks.iter_mut().flatten().find(|task| task.id == id) {
            task.user = user;
            task.reaping = false;
        }
    }

    fn finish_reap(&mut self, id: TaskId) -> Result<Task, SchedulerError> {
        let index = self
            .tasks
            .iter()
            .position(|task| task.as_ref().is_some_and(|task| task.id == id))
            .ok_or(SchedulerError::UnknownTask)?;
        let task = self.tasks[index]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Terminated
            || task.switch_pending_cpu.is_some()
            || !task.reaping
            || self.task_is_running(index)
        {
            return Err(SchedulerError::InvalidState);
        }
        self.tasks[index].take().ok_or(SchedulerError::UnknownTask)
    }

    #[cfg(test)]
    fn reap_terminated(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        let mut user = self.begin_reap(id, false)?;
        if let Some(user_task) = user.as_mut() {
            if user_task.address_space.destroy().is_err() {
                self.abort_reap(id, user);
                return Err(SchedulerError::AddressSpaceCleanup);
            }
        }
        drop(self.finish_reap(id)?);
        Ok(())
    }

    fn terminated_child(
        &self,
        parent: TaskId,
        child: Option<TaskId>,
    ) -> Result<TaskId, SchedulerError> {
        let mut matching = self.tasks.iter().flatten().filter(|task| {
            task.parent == Some(parent) && child.is_none_or(|child_id| task.id == child_id)
        });
        let Some(task) = matching
            .clone()
            .find(|task| task.state == TaskState::Terminated && task.switch_pending_cpu.is_none())
        else {
            if matching.next().is_some() {
                return Err(SchedulerError::ChildRunning);
            }
            return Err(if child.is_some() {
                SchedulerError::UnknownTask
            } else {
                SchedulerError::NoChild
            });
        };
        Ok(task.id)
    }

    fn wake_parent(&mut self, child_index: usize) {
        let parent = self.tasks[child_index]
            .as_ref()
            .and_then(|task| task.parent);
        if let Some(parent) = parent
            && let Some(task) = self
                .tasks
                .iter_mut()
                .flatten()
                .find(|task| task.id == parent)
            && task.state == TaskState::Blocked
        {
            task.state = TaskState::Ready;
            task.waiting_for_child = false;
            task.wake_pending = false;
        }
    }

    pub fn current_task(&self) -> Option<TaskId> {
        self.current_index(current_runqueue_index())
            .and_then(|index| self.tasks[index].as_ref().map(|task| task.id))
    }

    fn set_waiting_for_child(&mut self, waiting: bool) -> Result<(), SchedulerError> {
        let current = self
            .current_index(current_runqueue_index())
            .ok_or(SchedulerError::NotStarted)?;
        let task = self.tasks[current]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?;
        task.waiting_for_child = waiting;
        if !waiting {
            task.wake_pending = false;
        }
        Ok(())
    }

    pub fn task_state(&self, id: TaskId) -> Result<TaskState, SchedulerError> {
        self.task(id).map(|task| task.state)
    }

    pub fn task_context(&self, id: TaskId) -> Result<CpuContext, SchedulerError> {
        self.task(id).map(|task| task.context)
    }

    pub fn task_snapshot(&self) -> [Option<TaskInfo>; PER_CORE_MAX_TASKS] {
        let mut snapshot = [None; PER_CORE_MAX_TASKS];
        for (entry, task) in snapshot.iter_mut().zip(self.tasks.iter()) {
            if let Some(task) = task {
                *entry = Some(TaskInfo {
                    id: task.id,
                    parent: task.parent,
                    state: task.state,
                    exit_code: task.exit_code,
                    is_idle: task.is_idle,
                    is_user: task.user.is_some(),
                });
            }
        }
        snapshot
    }

    fn select_next(&mut self, start: usize, cpu_index: usize) -> Result<TaskId, SchedulerError> {
        // Prefer ordinary runnable tasks. The idle task is a permanent fallback.
        // switch_pending_cpu prevents migration until the context is saved; user-task
        // activation restores CPU-local syscall state and the destination address space.
        self.set_current_index(cpu_index, None);
        for pass in 0..2 {
            for offset in 0..PER_CORE_MAX_TASKS {
                let index = (start + offset) % PER_CORE_MAX_TASKS;
                if let Some(task) = self.tasks[index].as_mut()
                    && task.state == TaskState::Ready
                    && ((pass == 0 && !task.is_idle) || (pass == 1 && task.is_idle))
                    && task
                        .cpu_affinity
                        .is_none_or(|affinity| affinity == cpu_index)
                {
                    task.state = TaskState::Running;
                    let id = task.id;
                    self.set_current_index(cpu_index, Some(index));
                    return Ok(id);
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

impl Default for RunQueue {
    fn default() -> Self {
        Self::new()
    }
}

fn migratable_task_index(source: &RunQueue, destination_cpu: usize) -> Option<usize> {
    source.tasks.iter().enumerate().find_map(|(index, slot)| {
        slot.as_ref()
            .filter(|task| {
                task.state == TaskState::Ready
                    && !source.task_is_running(index)
                    && !task.is_idle
                    && !task.kill_pending
                    && !task.cleanup_pending
                    && !task.reaping
                    && task.switch_pending_cpu.is_none()
                    && task
                        .cpu_affinity
                        .is_none_or(|affinity| affinity == destination_cpu)
            })
            .map(|_| index)
    })
}

fn migrate_ready_task(
    source: &mut RunQueue,
    destination: &mut RunQueue,
    destination_cpu: usize,
) -> Result<TaskId, SchedulerError> {
    // The caller holds both queues in ascending CPU-index order. A user task's
    // kernel stack is task-owned; destination selection reloads its CR3, GS
    // syscall stack/user RSP, and TSS RSP0 before resuming the saved context.
    // CR3 is used without PCID, so that load flushes the destination's
    // non-global translations. Address-space mappings must not be changed
    // concurrently with a running task; future shared mappings need shootdowns.
    let source_index =
        migratable_task_index(source, destination_cpu).ok_or(SchedulerError::NoRunnableTasks)?;
    let destination_index = destination.check_task_capacity()?;
    let id = source.tasks[source_index]
        .as_ref()
        .ok_or(SchedulerError::UnknownTask)?
        .id;
    if destination.task(id).is_ok() {
        return Err(SchedulerError::InvalidState);
    }

    let task = source.tasks[source_index]
        .take()
        .ok_or(SchedulerError::UnknownTask)?;
    destination.tasks[destination_index] = Some(task);
    Ok(id)
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
    InvalidUserContext(UserContextError),
    AddressSpaceCleanup,
    TaskIdExhausted,
    InvalidFd,
    WouldBlock,
    BrokenPipe,
    Io,
    NoChild,
    ChildRunning,
    NotChild,
}

static RUN_QUEUES: [Mutex<RunQueue>; crate::arch::syscall::MAX_CPUS] =
    [const { Mutex::new(RunQueue::new()) }; crate::arch::syscall::MAX_CPUS];
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static mut BOOTSTRAP_CONTEXTS: [CpuContext; crate::arch::syscall::MAX_CPUS] =
    [const { CpuContext::empty() }; crate::arch::syscall::MAX_CPUS];

fn allocate_task_id() -> Result<TaskId, SchedulerError> {
    NEXT_TASK_ID
        .try_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
        .map(TaskId)
        .map_err(|_| SchedulerError::TaskIdExhausted)
}

fn current_runqueue_index() -> usize {
    let cpu_index = crate::arch::syscall::current_cpu_index();
    if cpu_index < crate::arch::syscall::MAX_CPUS {
        cpu_index
    } else {
        0
    }
}

fn online_runqueue(index: usize) -> bool {
    index == 0 || crate::smp::cpu_is_online(index)
}

fn current_runqueue() -> &'static Mutex<RunQueue> {
    &RUN_QUEUES[current_runqueue_index()]
}

#[cfg(test)]
fn has_live_non_idle_tasks() -> bool {
    RUN_QUEUES
        .iter()
        .any(|runqueue| runqueue.lock().has_live_non_idle_tasks())
}

fn find_task_queue(id: TaskId) -> Result<usize, SchedulerError> {
    for (index, runqueue) in RUN_QUEUES.iter().enumerate() {
        if runqueue.lock().task(id).is_ok() {
            return Ok(index);
        }
    }
    Err(SchedulerError::UnknownTask)
}

fn with_task_queue<T>(
    id: TaskId,
    operation: impl FnOnce(usize, &mut RunQueue) -> Result<T, SchedulerError>,
) -> Result<T, SchedulerError> {
    let _migration_guard = TASK_MIGRATION_LOCK.lock();
    let index = find_task_queue(id)?;
    let mut queue = RUN_QUEUES[index].lock();
    if queue.task(id).is_err() {
        return Err(SchedulerError::UnknownTask);
    }
    operation(index, &mut queue)
}

fn find_terminated_child(
    parent: TaskId,
    child: Option<TaskId>,
) -> Result<(TaskId, Option<u64>), SchedulerError> {
    let _migration_guard = TASK_MIGRATION_LOCK.lock();
    let mut has_child = false;
    for runqueue in RUN_QUEUES.iter() {
        let queue = runqueue.lock();
        match queue.terminated_child(parent, child) {
            Ok(child_id) => {
                let exit_code = queue.task(child_id)?.exit_code;
                return Ok((child_id, exit_code));
            }
            Err(SchedulerError::ChildRunning) => has_child = true,
            Err(SchedulerError::UnknownTask) | Err(SchedulerError::NoChild) => {}
            Err(error) => return Err(error),
        }
    }
    if has_child {
        Err(SchedulerError::ChildRunning)
    } else {
        Err(if child.is_some() {
            SchedulerError::UnknownTask
        } else {
            SchedulerError::NoChild
        })
    }
}

fn runnable_task_count(queue: &RunQueue, cpu_index: usize) -> usize {
    queue
        .tasks
        .iter()
        .flatten()
        .filter(|task| {
            !task.is_idle
                && matches!(task.state, TaskState::Ready | TaskState::Running)
                && task
                    .cpu_affinity
                    .is_none_or(|affinity| affinity == cpu_index)
        })
        .count()
}

fn select_runqueue() -> Result<usize, SchedulerError> {
    RUN_QUEUES
        .iter()
        .enumerate()
        .filter(|(index, _)| online_runqueue(*index))
        .filter_map(|(index, runqueue)| {
            let queue = runqueue.lock();
            let free = queue.tasks.iter().filter(|task| task.is_none()).count();
            (free > usize::from(queue.idle_index.is_none()))
                .then_some((index, runnable_task_count(&queue, index)))
        })
        .min_by_key(|(index, active)| (*active, *index))
        .map(|(index, _)| index)
        .ok_or(SchedulerError::Full)
}

fn steal_one_from(thief_cpu: usize, victim_cpu: usize) -> bool {
    let _migration_guard = TASK_MIGRATION_LOCK.lock();
    let result = if thief_cpu < victim_cpu {
        let mut thief = RUN_QUEUES[thief_cpu].lock();
        let mut victim = RUN_QUEUES[victim_cpu].lock();
        if runnable_task_count(&thief, thief_cpu) != 0 {
            return false;
        }
        migrate_ready_task(&mut victim, &mut thief, thief_cpu)
    } else {
        let mut victim = RUN_QUEUES[victim_cpu].lock();
        let mut thief = RUN_QUEUES[thief_cpu].lock();
        if runnable_task_count(&thief, thief_cpu) != 0 {
            return false;
        }
        migrate_ready_task(&mut victim, &mut thief, thief_cpu)
    };

    if result.is_ok() {
        SCHEDULER_STATS[thief_cpu]
            .successful_steals
            .fetch_add(1, Ordering::Relaxed);
        TOTAL_SUCCESSFUL_STEALS.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

fn steal_retry_allowed(cpu_index: usize, now: u64, force: bool) -> bool {
    let last_attempt = &LAST_STEAL_TICK[cpu_index];
    let mut previous = last_attempt.load(Ordering::Relaxed);
    loop {
        if !force && previous != u64::MAX && now.wrapping_sub(previous) < STEAL_RETRY_TICKS {
            return false;
        }
        match last_attempt.compare_exchange_weak(
            previous,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(actual) => previous = actual,
        }
    }
}

fn attempt_work_steal(cpu_index: usize, force: bool) -> bool {
    if cpu_index >= RUN_QUEUES.len() || !online_runqueue(cpu_index) {
        return false;
    }
    let idle_and_empty = {
        let queue = RUN_QUEUES[cpu_index].lock();
        let is_idle = queue
            .current_index(cpu_index)
            .and_then(|current| queue.tasks[current].as_ref())
            .is_some_and(|task| task.is_idle)
            || (queue.current_index(cpu_index).is_none() && queue.idle_index.is_some());
        is_idle && runnable_task_count(&queue, cpu_index) == 0
    };
    if !idle_and_empty || !steal_retry_allowed(cpu_index, crate::time::timer::ticks(), force) {
        return false;
    }

    let cursor = STEAL_VICTIM_CURSOR[cpu_index].fetch_add(1, Ordering::Relaxed) as usize;
    for offset in 0..RUN_QUEUES.len() {
        let victim_cpu = (cursor.wrapping_add(offset)) % RUN_QUEUES.len();
        if victim_cpu == cpu_index || !online_runqueue(victim_cpu) {
            continue;
        }
        SCHEDULER_STATS[cpu_index]
            .steal_attempts
            .fetch_add(1, Ordering::Relaxed);
        if steal_one_from(cpu_index, victim_cpu) {
            return true;
        }
    }
    false
}

fn has_stealable_task_for(source_cpu: usize, destination_cpu: usize) -> bool {
    let source = RUN_QUEUES[source_cpu].lock();
    migratable_task_index(&source, destination_cpu).is_some()
}

fn notify_one_idle_cpu(source_cpu: usize) {
    for destination_cpu in 1..RUN_QUEUES.len() {
        if destination_cpu == source_cpu || !online_runqueue(destination_cpu) {
            continue;
        }
        let idle = {
            let queue = RUN_QUEUES[destination_cpu].lock();
            queue
                .current_index(destination_cpu)
                .and_then(|index| queue.tasks[index].as_ref())
                .is_some_and(|task| task.is_idle)
                && runnable_task_count(&queue, destination_cpu) == 0
        };
        if idle && has_stealable_task_for(source_cpu, destination_cpu) {
            crate::smp::wake_cpu(destination_cpu);
            return;
        }
    }
}

fn bootstrap_context(cpu_index: usize) -> *mut CpuContext {
    let contexts = &raw mut BOOTSTRAP_CONTEXTS;
    unsafe { (*contexts).as_mut_ptr().add(cpu_index) }
}

fn running_task_index(scheduler: &Scheduler) -> Option<usize> {
    scheduler.current_index(current_runqueue_index())
}

struct InterruptGuard {
    restore_enabled: bool,
}

impl InterruptGuard {
    fn new() -> Self {
        let restore_enabled = x86_64::instructions::interrupts::are_enabled();
        x86_64::instructions::interrupts::disable();
        Self { restore_enabled }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        if self.restore_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}

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

fn create_task_stack() -> Result<(CpuContext, Box<[u128]>, VirtAddr), SchedulerError> {
    let mut stack =
        vec![0u128; KERNEL_STACK_SIZE / core::mem::size_of::<u128>()].into_boxed_slice();
    let stack_base = stack.as_mut_ptr().cast::<u8>() as usize;
    let stack_top = stack_base
        .checked_add(stack.len() * core::mem::size_of::<u128>())
        .ok_or(SchedulerError::InvalidContext)?;
    let aligned_top = stack_top & !(STACK_ALIGNMENT - 1);
    let initial_stack = aligned_top
        .checked_sub(2 * core::mem::size_of::<u64>())
        .ok_or(SchedulerError::InvalidContext)?;
    let stack_index = initial_stack
        .checked_sub(stack_base)
        .filter(|offset| offset.is_multiple_of(core::mem::size_of::<u64>()))
        .map(|offset| offset / core::mem::size_of::<u64>())
        .filter(|index| {
            index
                .checked_add(1)
                .is_some_and(|end| end <= KERNEL_STACK_SIZE / core::mem::size_of::<u64>())
        })
        .ok_or(SchedulerError::InvalidContext)?;
    unsafe {
        (stack_base as *mut u8)
            .cast::<u64>()
            .add(stack_index)
            .write(task_trampoline as *const () as usize as u64);
    }

    if stack_base % STACK_ALIGNMENT != 0
        || initial_stack % STACK_ALIGNMENT != 0
        || initial_stack < stack_base
        || initial_stack
            .checked_add(core::mem::size_of::<u64>())
            .is_none_or(|end| end > stack_top)
    {
        return Err(SchedulerError::InvalidContext);
    }

    let mut context = CpuContext::empty();
    context.stack_pointer = initial_stack as u64;
    context.rflags = 0x202;
    Ok((context, stack, VirtAddr::new(aligned_top as u64)))
}

fn destroy_unspawned_address_space(
    address_space: &mut Option<AddressSpace>,
) -> Result<(), SchedulerError> {
    if let Some(mut address_space) = address_space.take() {
        address_space
            .destroy()
            .map_err(|_| SchedulerError::AddressSpaceCleanup)?;
    }
    Ok(())
}

fn activate_task_address_space(scheduler: &Scheduler, index: Option<usize>) {
    let Some(index) = index else {
        unsafe { memory::activate_kernel_address_space() }
            .expect("restore kernel address space before bootstrap");
        return;
    };

    let task = scheduler.tasks[index]
        .as_ref()
        .expect("selected task slot is empty");
    if let Some(user) = task.user.as_ref() {
        unsafe {
            crate::arch::syscall::initialize(task.stack_top.expect("user task has no kernel stack"))
        };
        unsafe { crate::arch::syscall::set_user_rsp(user.syscall_user_rsp) };
        unsafe { user.address_space.activate() }.expect("activate selected user address space");
    } else {
        unsafe { memory::activate_kernel_address_space() }
            .expect("activate kernel address space for kernel task");
    }
}

/// Create a scheduled user task. Its address space is activated whenever the
/// scheduler selects it and the boot address space is restored for kernel
/// tasks and the bootstrap context.
pub fn spawn_user(
    address_space: AddressSpace,
    entry: VirtAddr,
    stack: VirtAddr,
) -> Result<TaskId, SchedulerError> {
    spawn_user_with_arguments(address_space, entry, stack, 0, VirtAddr::zero())
}

pub fn spawn_user_with_arguments(
    address_space: AddressSpace,
    entry: VirtAddr,
    stack: VirtAddr,
    argument_count: u64,
    argument_vector: VirtAddr,
) -> Result<TaskId, SchedulerError> {
    spawn_user_with_arguments_and_fds(
        address_space,
        entry,
        stack,
        argument_count,
        argument_vector,
        &[],
        &[],
    )
}

pub fn spawn_user_with_arguments_and_fds(
    address_space: AddressSpace,
    entry: VirtAddr,
    stack: VirtAddr,
    argument_count: u64,
    argument_vector: VirtAddr,
    fd_mappings: &[FdMapping],
    fd_closes: &[usize],
) -> Result<TaskId, SchedulerError> {
    let mut address_space = Some(address_space);
    let context = match UserContext::for_address_space_with_arguments(
        address_space.as_ref().expect("user address space missing"),
        entry,
        stack,
        argument_count,
        argument_vector,
    ) {
        Ok(context) => context,
        Err(error) => {
            destroy_unspawned_address_space(&mut address_space)?;
            return Err(SchedulerError::InvalidUserContext(error));
        }
    };

    let (parent, parent_fds) = {
        let queue = current_runqueue().lock();
        let current = queue.current_index(current_runqueue_index());
        current
            .and_then(|index| queue.tasks[index].as_ref())
            .map(|task| (Some(task.id), Some(task.fds.clone())))
            .unwrap_or((None, None))
    };
    let result = match select_runqueue() {
        Ok(index) => RUN_QUEUES[index].lock().create_user_task(
            &mut address_space,
            context,
            parent,
            parent_fds,
            fd_mappings,
            fd_closes,
        ),
        Err(error) => Err(error),
    };
    if result.is_err() {
        destroy_unspawned_address_space(&mut address_space)?;
    } else {
        crate::smp::wake_secondaries();
    }
    result
}

pub fn spawn(entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
    let parent = current_runqueue().lock().current_task();
    let result = select_runqueue().and_then(|index| {
        let mut queue = RUN_QUEUES[index].lock();
        spawn_child_into(&mut queue, entry, parent)
    });
    if result.is_ok() {
        crate::smp::wake_secondaries();
    }
    result
}

/// Spawn a kernel task without making it a child of the task interrupted on
/// the current CPU.
pub fn spawn_detached(entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
    let result = select_runqueue().and_then(|index| {
        let mut queue = RUN_QUEUES[index].lock();
        if queue.idle_index.is_none() {
            spawn_detached_into(&mut queue, entry)
        } else {
            queue.create_kernel_task(entry, None)
        }
    });
    if result.is_ok() {
        crate::smp::wake_secondaries();
    }
    result
}

fn spawn_detached_into(
    scheduler: &mut Scheduler,
    entry: KernelTaskEntry,
) -> Result<TaskId, SchedulerError> {
    scheduler.check_spawn_capacity()?;
    scheduler.create_kernel_task(entry, None)
}

#[cfg(test)]
fn spawn_into(scheduler: &mut Scheduler, entry: KernelTaskEntry) -> Result<TaskId, SchedulerError> {
    let parent = scheduler.current_task();
    spawn_child_into(scheduler, entry, parent)
}

fn spawn_child_into(
    scheduler: &mut Scheduler,
    entry: KernelTaskEntry,
    parent: Option<TaskId>,
) -> Result<TaskId, SchedulerError> {
    scheduler.check_spawn_capacity()?;
    scheduler.create_kernel_task(entry, parent)
}

pub fn run_tasks() -> Result<(), SchedulerError> {
    let _interrupt_guard = InterruptGuard::new();
    let cpu_index = current_runqueue_index();
    let next_context = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        if scheduler.idle_index.is_none() {
            let id = scheduler.create_kernel_task(idle_task_entry, None)?;
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
        let index = scheduler
            .current_index(cpu_index)
            .ok_or(SchedulerError::NotStarted)?;
        activate_task_address_space(&scheduler, Some(index));
        &scheduler.tasks[index]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *const CpuContext
    };
    let bootstrap = bootstrap_context(cpu_index);
    unsafe { leafos_context_switch(bootstrap, next_context) };
    complete_cpu_switch(current_runqueue_index(), true);
    Ok(())
}

/// Run the CPU-local scheduler on a secondary processor.
pub fn run_secondary(cpu_index: usize) -> ! {
    {
        let mut queue = RUN_QUEUES[cpu_index].lock();
        if queue.idle_index.is_none() {
            let id = queue
                .create_kernel_task(idle_task_entry, None)
                .expect("could not create secondary idle task");
            let index = queue
                .tasks
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|task| task.id == id))
                .expect("secondary idle task was not inserted");
            queue.tasks[index]
                .as_mut()
                .expect("secondary idle slot is empty")
                .is_idle = true;
            queue.idle_index = Some(index);
        }
    }
    loop {
        let _interrupt_guard = InterruptGuard::new();
        attempt_work_steal(cpu_index, true);
        let next_context = {
            let mut scheduler = RUN_QUEUES[cpu_index].lock();
            scheduler.wake_expired_sleepers(crate::time::timer::ticks());
            if scheduler.current_index(cpu_index).is_some() {
                panic!("secondary scheduler entered with a running task");
            }
            scheduler
                .select_next(0, cpu_index)
                .expect("secondary idle task missing");
            let index = scheduler
                .current_index(cpu_index)
                .expect("secondary idle task missing");
            activate_task_address_space(&scheduler, Some(index));
            &scheduler.tasks[index]
                .as_ref()
                .expect("selected task slot is empty")
                .context as *const CpuContext
        };
        // Keep bootstrap interrupts disabled until the context switch has
        // installed the selected task's stack and register state.
        unsafe {
            leafos_context_switch(bootstrap_context(cpu_index), next_context);
        }
        complete_cpu_switch(cpu_index, true);
    }
}

pub fn yield_now() -> Result<(), SchedulerError> {
    let _interrupt_guard = InterruptGuard::new();
    let cpu_index = current_runqueue_index();
    let (old_context, next_context) = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        let current = scheduler
            .current_index(cpu_index)
            .ok_or(SchedulerError::NotStarted)?;
        save_current_user_rsp(&mut scheduler, current);
        scheduler.transition_current(TaskState::Ready)?;
        let next = scheduler
            .current_index(cpu_index)
            .ok_or(SchedulerError::NoRunnableTasks)?;
        scheduler.mark_switch_pending(current, cpu_index);
        activate_task_address_space(&scheduler, Some(next));
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
    complete_cpu_switch(current_runqueue_index(), true);
    Ok(())
}

/// Rotate away from the interrupted task after a timer tick.
///
/// The interrupt handler's compiler-generated register saves and hardware
/// interrupt frame remain on the interrupted task's stack. Switching from
/// this call boundary suspends that entire frame until the task is resumed.
pub(crate) fn preempt_from_timer(interrupted_user_rsp: Option<u64>, force_steal: bool) {
    let _interrupt_guard = InterruptGuard::new();
    let cpu_index = current_runqueue_index();
    let woke_sleepers = wake_expired_sleepers_all(crate::time::timer::ticks());
    attempt_work_steal(cpu_index, force_steal);
    let (switch, notify_secondaries) = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        let mut notify_secondaries = woke_sleepers;
        let switch = if let Some(current) = scheduler.current_index(cpu_index) {
            if let Some(user_rsp) = interrupted_user_rsp
                && let Some(user) = scheduler.tasks[current]
                    .as_mut()
                    .and_then(|task| task.user.as_mut())
            {
                user.syscall_user_rsp = user_rsp;
            }
            let kill_pending = scheduler.tasks[current]
                .as_ref()
                .is_some_and(|task| task.kill_pending);
            if kill_pending {
                scheduler.tasks[current]
                    .as_mut()
                    .expect("interrupted task slot is empty")
                    .switch_pending_cpu = Some(cpu_index);
                scheduler.tasks[current]
                    .as_mut()
                    .expect("interrupted task slot is empty")
                    .cleanup_pending = true;
            }
            let next_state = if kill_pending {
                scheduler.wake_parent(current);
                notify_secondaries = true;
                TaskState::Terminated
            } else {
                TaskState::Ready
            };
            if scheduler.tasks[current]
                .as_ref()
                .is_some_and(|task| task.is_idle)
            {
                SCHEDULER_STATS[cpu_index]
                    .idle_ticks
                    .fetch_add(1, Ordering::Relaxed);
            }
            let no_runnable_tasks = match scheduler.transition_current(next_state) {
                Ok(_) => false,
                Err(SchedulerError::NoRunnableTasks) => true,
                Err(error) => panic!("timer preemption failed: {:?}", error),
            };
            scheduler.mark_switch_pending(current, cpu_index);

            match scheduler.current_index(cpu_index) {
                Some(next) if next == current && !kill_pending => None,
                Some(next) => {
                    activate_task_address_space(&scheduler, Some(next));
                    SCHEDULER_STATS[cpu_index]
                        .timer_preemptions
                        .fetch_add(1, Ordering::Relaxed);
                    let old_context = &mut scheduler.tasks[current]
                        .as_mut()
                        .expect("interrupted task slot is empty")
                        .context as *mut CpuContext;
                    let next_context = &scheduler.tasks[next]
                        .as_ref()
                        .expect("selected task slot is empty")
                        .context as *const CpuContext;
                    Some((old_context, next_context))
                }
                None if kill_pending || no_runnable_tasks => {
                    activate_task_address_space(&scheduler, None);
                    SCHEDULER_STATS[cpu_index]
                        .timer_preemptions
                        .fetch_add(1, Ordering::Relaxed);
                    let old_context = &mut scheduler.tasks[current]
                        .as_mut()
                        .expect("interrupted task slot is empty")
                        .context as *mut CpuContext;
                    Some((
                        old_context,
                        bootstrap_context(cpu_index) as *const CpuContext,
                    ))
                }
                None => None,
            }
        } else {
            None
        };
        (switch, notify_secondaries)
    };

    if notify_secondaries {
        crate::smp::wake_secondaries();
    }
    if let Some((old_context, next_context)) = switch {
        unsafe { leafos_context_switch(old_context, next_context) };
        complete_cpu_switch(current_runqueue_index(), false);
    }
}

/// Block the current kernel task. The idle task runs if no other task is ready;
/// when this task is later woken and scheduled again, this call resumes and
/// returns `Ok(())`.
pub fn block_now() -> Result<(), SchedulerError> {
    let _interrupt_guard = InterruptGuard::new();
    let cpu_index = current_runqueue_index();
    let (old_context, next_context) = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        let current = scheduler
            .current_index(cpu_index)
            .ok_or(SchedulerError::NotStarted)?;
        save_current_user_rsp(&mut scheduler, current);

        match scheduler.transition_current(TaskState::Blocked) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => return Err(error),
        }
        scheduler.mark_switch_pending(current, cpu_index);

        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *mut CpuContext;
        let next_context = match scheduler.current_index(cpu_index) {
            Some(next) => {
                activate_task_address_space(&scheduler, Some(next));
                &scheduler.tasks[next]
                    .as_ref()
                    .ok_or(SchedulerError::UnknownTask)?
                    .context as *const CpuContext
            }
            None => {
                activate_task_address_space(&scheduler, None);
                bootstrap_context(cpu_index) as *const CpuContext
            }
        };
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    complete_cpu_switch(current_runqueue_index(), true);
    Ok(())
}

/// Sleep the current task for a number of PIT ticks without polling.
/// The idle task wakes expired sleepers after each timer interrupt.
pub fn sleep_ticks(duration: u64) -> Result<(), SchedulerError> {
    if duration == 0 {
        return Ok(());
    }
    let _interrupt_guard = InterruptGuard::new();
    let deadline = crate::time::timer::ticks().wrapping_add(duration);
    let cpu_index = current_runqueue_index();
    let (old_context, next_context) = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        let current = scheduler
            .current_index(cpu_index)
            .ok_or(SchedulerError::NotStarted)?;
        save_current_user_rsp(&mut scheduler, current);
        match scheduler.sleep_current_until(deadline) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => return Err(error),
        }
        scheduler.mark_switch_pending(current, cpu_index);
        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .ok_or(SchedulerError::UnknownTask)?
            .context as *mut CpuContext;
        let next_context = match scheduler.current_index(cpu_index) {
            Some(next) => {
                activate_task_address_space(&scheduler, Some(next));
                &scheduler.tasks[next]
                    .as_ref()
                    .ok_or(SchedulerError::UnknownTask)?
                    .context as *const CpuContext
            }
            None => {
                activate_task_address_space(&scheduler, None);
                bootstrap_context(cpu_index) as *const CpuContext
            }
        };
        (old_context, next_context)
    };
    unsafe { leafos_context_switch(old_context, next_context) };
    complete_cpu_switch(current_runqueue_index(), true);
    Ok(())
}

fn idle_task_entry() {
    loop {
        #[cfg(test)]
        if current_runqueue_index() == 0 && !has_live_non_idle_tasks() {
            let _interrupt_guard = InterruptGuard::new();
            let (old_context, next_context) = {
                let mut scheduler = RUN_QUEUES[0].lock();
                let Some(current) = scheduler.current_index(0) else {
                    panic!("test idle task lost its runqueue slot");
                };
                let task = scheduler.tasks[current]
                    .as_mut()
                    .expect("test idle task slot is empty");
                assert!(task.is_idle, "test scheduler switched to a non-idle task");
                task.state = TaskState::Ready;
                scheduler.set_current_index(0, None);
                activate_task_address_space(&scheduler, None);
                (
                    &mut scheduler.tasks[current]
                        .as_mut()
                        .expect("test idle task disappeared")
                        .context as *mut CpuContext,
                    bootstrap_context(0) as *const CpuContext,
                )
            };
            unsafe { leafos_context_switch(old_context, next_context) };
            complete_cpu_switch(0, true);
        }
        let cpu_index = current_runqueue_index();
        if attempt_work_steal(cpu_index, false) {
            yield_now().expect("idle task could not schedule a stolen task");
        }
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

fn wake_expired_sleepers_all(now: u64) -> bool {
    let mut woke_any = false;
    for (index, runqueue) in RUN_QUEUES.iter().enumerate() {
        if online_runqueue(index) {
            woke_any |= runqueue.lock().wake_expired_sleepers(now);
        }
    }
    woke_any
}

/// Wake a blocked task. Waking changes its state to `Ready` but does not
/// preempt a currently running task. If the scheduler is idle, call
/// `run_tasks()` to resume scheduling from the bootstrap context.
pub fn wake_task(id: TaskId) -> Result<(), SchedulerError> {
    let result = with_task_queue(id, |_, queue| queue.wake(id));
    if result.is_ok() {
        crate::smp::wake_secondaries();
    }
    result
}

/// Terminate a non-running task from kernel control code.
pub fn terminate_task(id: TaskId, exit_code: u64) -> Result<(), SchedulerError> {
    let (index, parent, result) = with_task_queue(id, |index, queue| {
        let parent = queue.task(id).ok().and_then(|task| task.parent);
        Ok((index, parent, queue.terminate_task(id, exit_code)))
    })?;
    if let Some(parent) = parent {
        let _ = wake_task(parent);
    }
    if result.is_ok() {
        complete_cpu_switch(index, true);
        crate::smp::wake_secondaries();
    }
    result
}

/// Request termination of a task that may currently be executing on another
/// CPU. Closed terminal tasks are reaped after their context has been switched
/// off its stack.
pub fn request_terminate_task(id: TaskId, exit_code: u64) -> Result<(), SchedulerError> {
    let (index, parent, running) = with_task_queue(id, |index, scheduler| {
        let parent = scheduler.task(id).ok().and_then(|task| task.parent);
        let running = scheduler.request_terminate_task(id, exit_code)?;
        Ok((index, parent, running))
    })?;
    if !running && let Some(parent) = parent {
        let _ = wake_task(parent);
    }
    if running {
        crate::smp::wake_secondaries();
    } else {
        complete_cpu_switch(index, true);
    }
    Ok(())
}

/// Wake tasks waiting for descriptor activity or another external event.
pub fn wake_all_blocked_tasks() {
    for (index, runqueue) in RUN_QUEUES.iter().enumerate() {
        if online_runqueue(index) {
            runqueue.lock().wake_all_blocked();
        }
    }
    crate::smp::wake_secondaries();
}

/// Return whether the currently executing scheduled task is a user task.
pub fn current_task_is_user() -> bool {
    let scheduler = current_runqueue().lock();
    scheduler
        .current_index(current_runqueue_index())
        .and_then(|index| scheduler.tasks[index].as_ref())
        .is_some_and(|task| task.user.is_some())
}

pub fn current_task_id() -> Option<TaskId> {
    current_runqueue().lock().current_task()
}

/// Check a user buffer against the running task's address-space metadata.
/// The scheduler lock keeps the task and its mappings stable during the check.
pub fn validate_current_user_range(
    address: u64,
    length: usize,
    writable: bool,
) -> Result<(), memory::UserMemoryError> {
    let scheduler = current_runqueue().lock();
    let task = scheduler
        .current_index(current_runqueue_index())
        .and_then(|index| scheduler.tasks[index].as_ref())
        .and_then(|task| task.user.as_ref())
        .ok_or(memory::UserMemoryError::InactiveAddressSpace)?;
    task.address_space
        .validate_user_range(address, length, writable)
}

/// Copy a bounded syscall argument out of the currently running user task.
pub fn copy_from_current_user(
    address: u64,
    output: &mut [u8],
) -> Result<(), memory::UserMemoryError> {
    let scheduler = current_runqueue().lock();
    let task = scheduler
        .current_index(current_runqueue_index())
        .and_then(|index| scheduler.tasks[index].as_ref())
        .and_then(|task| task.user.as_ref())
        .ok_or(memory::UserMemoryError::InactiveAddressSpace)?;
    task.address_space.copy_from_user(address, output)
}

/// Copy syscall output to a writable buffer in the currently running task.
pub fn copy_to_current_user(address: u64, input: &[u8]) -> Result<(), memory::UserMemoryError> {
    let scheduler = current_runqueue().lock();
    let task = scheduler
        .current_index(current_runqueue_index())
        .and_then(|index| scheduler.tasks[index].as_ref())
        .and_then(|task| task.user.as_ref())
        .ok_or(memory::UserMemoryError::InactiveAddressSpace)?;
    task.address_space.copy_to_user(address, input)
}

/// Terminate the current kernel task without returning to its entry function.
/// The terminated task can be released later with `reap_terminated()`.
pub fn exit_current() -> ! {
    exit_current_with_status(None)
}

/// Terminate the current task and retain its user-visible exit code until reaped.
pub fn exit_current_with_code(exit_code: u64) -> ! {
    exit_current_with_status(Some(exit_code))
}

/// Terminate the current user task and its sibling tasks created by the same
/// shell pipeline.
pub fn exit_current_group_with_code(exit_code: u64) -> ! {
    exit_current_with_status_and_group(Some(exit_code), true)
}

fn exit_current_with_status(exit_code: Option<u64>) -> ! {
    exit_current_with_status_and_group(exit_code, false)
}

fn exit_current_with_status_and_group(exit_code: Option<u64>, terminate_group: bool) -> ! {
    let _interrupt_guard = InterruptGuard::new();
    let cpu_index = current_runqueue_index();
    let (current_id, parent_id) = {
        let queue = RUN_QUEUES[cpu_index].lock();
        let current = queue
            .current_index(cpu_index)
            .expect("task exit without current task");
        let task = queue.tasks[current]
            .as_ref()
            .expect("current task slot is empty");
        (task.id, task.parent)
    };

    let mut group_members = Vec::new();
    if terminate_group && let Some(parent) = parent_id {
        let _migration_guard = TASK_MIGRATION_LOCK.lock();
        for runqueue in RUN_QUEUES.iter() {
            let queue = runqueue.lock();
            group_members.extend(
                queue
                    .tasks
                    .iter()
                    .flatten()
                    .filter(|task| task.id != current_id && task.parent == Some(parent))
                    .map(|task| task.id),
            );
        }
    }

    let mut notify_secondaries = false;
    for sibling in group_members {
        let (sibling_parent, newly_terminated) = match with_task_queue(sibling, |_, queue| {
            queue.terminate_group_member(sibling, exit_code)
        }) {
            Ok(result) => result,
            Err(error) => {
                crate::serial_println!(
                    "[WARN] Could not terminate sibling task {}: {:?}",
                    sibling.get(),
                    error
                );
                continue;
            }
        };
        if !newly_terminated && crate::scheduler::task_state(sibling) == Ok(TaskState::Running) {
            notify_secondaries = true;
        }
        if let Some(parent) = sibling_parent {
            let _ = wake_task(parent);
        }
    }

    let (old_context, next_context) = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        let current = scheduler
            .current_index(cpu_index)
            .expect("task exit without current task");
        save_current_user_rsp(&mut scheduler, current);
        scheduler.tasks[current]
            .as_mut()
            .expect("current task slot is empty")
            .exit_code = exit_code;
        scheduler.tasks[current]
            .as_mut()
            .expect("current task slot is empty")
            .cleanup_pending = true;
        scheduler.tasks[current]
            .as_mut()
            .expect("current task slot is empty")
            .switch_pending_cpu = Some(cpu_index);
        scheduler.wake_parent(current);

        match scheduler.transition_current(TaskState::Terminated) {
            Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
            Err(error) => panic!("scheduler failed to terminate current task: {:?}", error),
        }

        let old_context = &mut scheduler.tasks[current]
            .as_mut()
            .expect("terminated task slot is empty")
            .context as *mut CpuContext;
        let next = scheduler
            .current_index(cpu_index)
            .expect("idle task missing");
        activate_task_address_space(&scheduler, Some(next));
        let next_context = &scheduler.tasks[next]
            .as_ref()
            .expect("selected task slot is empty")
            .context as *const CpuContext;
        (old_context, next_context)
    };
    if let Some(parent) = parent_id {
        let _ = wake_task(parent);
    }
    if notify_secondaries || terminate_group {
        crate::smp::wake_secondaries();
    }
    unsafe { leafos_context_switch(old_context, next_context) };
    complete_cpu_switch(current_runqueue_index(), true);
    unreachable!("a terminated task must never resume")
}

pub fn task_state(id: TaskId) -> Result<TaskState, SchedulerError> {
    with_task_queue(id, |_, queue| queue.task_state(id))
}

pub fn task_parent(id: TaskId) -> Result<Option<TaskId>, SchedulerError> {
    with_task_queue(id, |_, queue| queue.task(id).map(|task| task.parent))
}

pub fn read_current_fd(fd_number: usize, output: &mut [u8]) -> Result<usize, SchedulerError> {
    let scheduler = current_runqueue().lock();
    let task = scheduler
        .current_index(current_runqueue_index())
        .and_then(|index| scheduler.tasks[index].as_ref())
        .ok_or(SchedulerError::NotStarted)?;
    let descriptor = task
        .fds
        .get(fd_number)
        .and_then(Option::as_ref)
        .ok_or(SchedulerError::InvalidFd)?;
    fd::read(descriptor, output).map_err(|error| match error {
        fd::ReadError::Invalid => SchedulerError::InvalidFd,
        fd::ReadError::WouldBlock => SchedulerError::WouldBlock,
        fd::ReadError::Io => SchedulerError::Io,
    })
}

pub fn write_current_fd(fd_number: usize, input: &[u8]) -> Result<usize, SchedulerError> {
    let scheduler = current_runqueue().lock();
    let task = scheduler
        .current_index(current_runqueue_index())
        .and_then(|index| scheduler.tasks[index].as_ref())
        .ok_or(SchedulerError::NotStarted)?;
    let descriptor = task
        .fds
        .get(fd_number)
        .and_then(Option::as_ref)
        .ok_or(SchedulerError::InvalidFd)?;
    fd::write_with_owner(descriptor, input, task.id.get()).map_err(|error| match error {
        fd::WriteError::Invalid => SchedulerError::InvalidFd,
        fd::WriteError::WouldBlock => SchedulerError::WouldBlock,
        fd::WriteError::BrokenPipe => SchedulerError::BrokenPipe,
        fd::WriteError::Io => SchedulerError::Io,
    })
}

pub fn create_file_write_for_current(path: &str) -> Result<usize, SchedulerError> {
    let _interrupt_guard = InterruptGuard::new();
    let file_fd = {
        let scheduler = current_runqueue().lock();
        let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
        let task = scheduler.tasks[current]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?;
        find_free_fd(&task.fds).ok_or(SchedulerError::Full)?
    };
    let descriptor = fd::create_file_write(path).map_err(|_| SchedulerError::Io)?;
    let mut scheduler = current_runqueue().lock();
    let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
    let task = scheduler.tasks[current]
        .as_mut()
        .ok_or(SchedulerError::UnknownTask)?;
    if task.fds[file_fd].is_some() {
        return Err(SchedulerError::Full);
    }
    task.fds[file_fd] = Some(descriptor);
    Ok(file_fd)
}

pub fn create_file_read_for_current(path: &str) -> Result<usize, SchedulerError> {
    let _interrupt_guard = InterruptGuard::new();
    let file_fd = {
        let scheduler = current_runqueue().lock();
        let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
        let task = scheduler.tasks[current]
            .as_ref()
            .ok_or(SchedulerError::UnknownTask)?;
        find_free_fd(&task.fds).ok_or(SchedulerError::Full)?
    };
    let descriptor = fd::create_file_read(path).map_err(|_| SchedulerError::Io)?;
    let mut scheduler = current_runqueue().lock();
    let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
    let task = scheduler.tasks[current]
        .as_mut()
        .ok_or(SchedulerError::UnknownTask)?;
    if task.fds[file_fd].is_some() {
        return Err(SchedulerError::Full);
    }
    task.fds[file_fd] = Some(descriptor);
    Ok(file_fd)
}

pub fn create_pipe_for_current() -> Result<(usize, usize), SchedulerError> {
    let (read_end, write_end) = fd::create_pipe().ok_or(SchedulerError::Full)?;
    let mut scheduler = current_runqueue().lock();
    let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
    let task = scheduler.tasks[current]
        .as_mut()
        .ok_or(SchedulerError::UnknownTask)?;
    let (read_fd, write_fd) = find_free_fd_pair(&task.fds).ok_or(SchedulerError::Full)?;
    task.fds[read_fd] = Some(read_end);
    task.fds[write_fd] = Some(write_end);
    Ok((read_fd, write_fd))
}

pub fn close_current_fd(fd_number: usize) -> Result<(), SchedulerError> {
    let mut scheduler = current_runqueue().lock();
    let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
    let task = scheduler
        .tasks
        .get_mut(current)
        .and_then(Option::as_mut)
        .ok_or(SchedulerError::InvalidFd)?;
    let descriptor = task
        .fds
        .get_mut(fd_number)
        .and_then(Option::take)
        .ok_or(SchedulerError::InvalidFd)?;
    fd::close(descriptor);
    drop(scheduler);
    wake_all_blocked_tasks();
    Ok(())
}

pub fn duplicate_current_fd(source_fd: usize, target_fd: usize) -> Result<(), SchedulerError> {
    if source_fd >= MAX_FDS || target_fd >= MAX_FDS {
        return Err(SchedulerError::InvalidFd);
    }
    if source_fd == target_fd {
        let scheduler = current_runqueue().lock();
        let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
        if scheduler.tasks[current]
            .as_ref()
            .and_then(|task| task.fds[source_fd].as_ref())
            .is_some()
        {
            return Ok(());
        }
        return Err(SchedulerError::InvalidFd);
    }

    let mut scheduler = current_runqueue().lock();
    let current = running_task_index(&scheduler).ok_or(SchedulerError::NotStarted)?;
    let task = scheduler
        .tasks
        .get_mut(current)
        .and_then(Option::as_mut)
        .ok_or(SchedulerError::InvalidFd)?;
    let duplicate = task.fds[source_fd]
        .as_ref()
        .cloned()
        .ok_or(SchedulerError::InvalidFd)?;
    if let Some(previous) = task.fds[target_fd].take() {
        fd::close(previous);
    }
    task.fds[target_fd] = Some(duplicate);
    Ok(())
}

/// Wait for one of the current task's children to terminate, then reap it.
///
/// Supplying a child ID restricts the wait to that child. The child must have
/// been created by the calling task; unrelated tasks cannot be waited on.
pub fn wait_for_child(child: Option<TaskId>) -> Result<(TaskId, Option<u64>), SchedulerError> {
    loop {
        let _interrupt_guard = InterruptGuard::new();
        let cpu_index = current_runqueue_index();
        let parent = RUN_QUEUES[cpu_index]
            .lock()
            .current_task()
            .ok_or(SchedulerError::NotStarted)?;
        RUN_QUEUES[cpu_index].lock().set_waiting_for_child(true)?;
        if let Some(child_id) = child {
            let child_parent = match with_task_queue(child_id, |_, queue| {
                queue.task(child_id).map(|task| task.parent)
            }) {
                Ok(parent) => parent,
                Err(error) => {
                    RUN_QUEUES[cpu_index].lock().set_waiting_for_child(false)?;
                    return Err(error);
                }
            };
            if child_parent != Some(parent) {
                RUN_QUEUES[cpu_index].lock().set_waiting_for_child(false)?;
                return Err(SchedulerError::NotChild);
            }
        }

        match find_terminated_child(parent, child) {
            Ok((child_id, exit_code)) => {
                RUN_QUEUES[cpu_index].lock().set_waiting_for_child(false)?;
                reap_terminated(child_id)?;
                return Ok((child_id, exit_code));
            }
            Err(SchedulerError::ChildRunning) => {}
            Err(error) => {
                RUN_QUEUES[cpu_index].lock().set_waiting_for_child(false)?;
                return Err(error);
            }
        }

        let (old_context, next_context) = {
            let mut scheduler = RUN_QUEUES[cpu_index].lock();
            let current = scheduler
                .current_index(cpu_index)
                .ok_or(SchedulerError::NotStarted)?;
            save_current_user_rsp(&mut scheduler, current);
            match scheduler.transition_current(TaskState::Blocked) {
                Ok(_) | Err(SchedulerError::NoRunnableTasks) => {}
                Err(error) => return Err(error),
            }
            scheduler.mark_switch_pending(current, cpu_index);
            let old_context = &mut scheduler.tasks[current]
                .as_mut()
                .ok_or(SchedulerError::UnknownTask)?
                .context as *mut CpuContext;
            let next_context = match scheduler.current_index(cpu_index) {
                Some(next) => {
                    activate_task_address_space(&scheduler, Some(next));
                    &scheduler.tasks[next]
                        .as_ref()
                        .ok_or(SchedulerError::UnknownTask)?
                        .context as *const CpuContext
                }
                None => {
                    activate_task_address_space(&scheduler, None);
                    bootstrap_context(cpu_index) as *const CpuContext
                }
            };
            (old_context, next_context)
        };
        unsafe { leafos_context_switch(old_context, next_context) };
        complete_cpu_switch(current_runqueue_index(), true);
    }
}

/// Return a terminated task's exit code, or `None` if it failed or was a kernel task.
pub fn task_exit_code(id: TaskId) -> Result<Option<u64>, SchedulerError> {
    with_task_queue(id, |_, queue| queue.task(id).map(|task| task.exit_code))
}

pub fn task_snapshot() -> [Option<TaskInfo>; MAX_TASKS] {
    let mut snapshot = [None; MAX_TASKS];
    let mut next = 0;
    for runqueue in RUN_QUEUES.iter() {
        let queue = runqueue.lock();
        for task in queue.tasks.iter().flatten() {
            snapshot[next] = Some(TaskInfo {
                id: task.id,
                parent: task.parent,
                state: task.state,
                exit_code: task.exit_code,
                is_idle: task.is_idle,
                is_user: task.user.is_some(),
            });
            next += 1;
        }
    }
    snapshot
}

pub fn scheduler_stats() -> SchedulerStats {
    let mut cpus = [None; crate::arch::syscall::MAX_CPUS];
    for (index, runqueue) in RUN_QUEUES.iter().enumerate() {
        if !online_runqueue(index) {
            continue;
        }
        let queue = runqueue.lock();
        let runnable_tasks = runnable_task_count(&queue, index);
        let counters = &SCHEDULER_STATS[index];
        cpus[index] = Some(CpuSchedulerStats {
            context_switches: counters.context_switches.load(Ordering::Relaxed),
            timer_preemptions: counters.timer_preemptions.load(Ordering::Relaxed),
            idle_ticks: counters.idle_ticks.load(Ordering::Relaxed),
            tasks_executed: counters.tasks_executed.load(Ordering::Relaxed),
            runnable_tasks,
            steal_attempts: counters.steal_attempts.load(Ordering::Relaxed),
            successful_steals: counters.successful_steals.load(Ordering::Relaxed),
        });
    }
    SchedulerStats {
        cpus,
        work_stealing_events: TOTAL_SUCCESSFUL_STEALS.load(Ordering::Relaxed),
    }
}

pub fn cpu_is_idle(cpu_index: usize) -> Option<bool> {
    let queue = RUN_QUEUES.get(cpu_index)?.lock();
    let current = queue.current_index(cpu_index)?;
    queue.tasks.get(current)?.as_ref().map(|task| task.is_idle)
}

pub fn reap_terminated(id: TaskId) -> Result<(), SchedulerError> {
    let index = find_task_queue(id)?;
    reap_task_from_runqueue(index, id, false)
}

#[cfg(test)]
static SMP_STEAL_TEST_CPU: AtomicU64 = AtomicU64::new(u64::MAX);
#[cfg(test)]
static SMP_STEAL_TEST_RUNS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn smp_steal_test_task() {
    SMP_STEAL_TEST_RUNS.fetch_add(1, Ordering::SeqCst);
    SMP_STEAL_TEST_CPU.store(current_runqueue_index() as u64, Ordering::Release);
}

#[cfg(test)]
pub fn run_smp_work_stealing_test() {
    serial_println!("SMP work-stealing integration...");
    SMP_STEAL_TEST_CPU.store(u64::MAX, Ordering::Relaxed);
    SMP_STEAL_TEST_RUNS.store(0, Ordering::Relaxed);
    let steals_before = scheduler_stats().work_stealing_events;
    let task = spawn_detached(smp_steal_test_task).expect("queue SMP migration test task");
    let started = crate::smp::start_secondaries();
    assert!(
        started >= 2,
        "SMP integration test requires two secondary CPUs"
    );

    let start = crate::time::timer::ticks();
    loop {
        if task_state(task) == Ok(TaskState::Terminated) {
            break;
        }
        assert!(
            crate::time::timer::ticks().wrapping_sub(start)
                < 5 * crate::time::timer::TICKS_PER_SECOND,
            "SMP migration task did not finish"
        );
        x86_64::instructions::interrupts::enable_and_hlt();
    }

    let task_cpu = SMP_STEAL_TEST_CPU.load(Ordering::Acquire) as usize;
    assert!(
        task_cpu > 0,
        "test task did not migrate off the bootstrap CPU"
    );
    assert!(
        crate::smp::cpu_is_online(task_cpu),
        "task ran on an offline CPU"
    );
    assert_eq!(
        SMP_STEAL_TEST_RUNS.load(Ordering::SeqCst),
        1,
        "migrated task must execute exactly once"
    );
    assert!(
        scheduler_stats().work_stealing_events > steals_before,
        "work-steal statistics did not record the migration"
    );
    serial_println!(
        "[OK] Task {} migrated to CPU {}; successful steals={}",
        task.get(),
        task_cpu,
        scheduler_stats().work_stealing_events - steals_before
    );
}

fn reap_task_from_runqueue(index: usize, id: TaskId, reserved: bool) -> Result<(), SchedulerError> {
    let mut user = RUN_QUEUES[index].lock().begin_reap(id, reserved)?;
    if let Some(user_task) = user.as_mut() {
        if user_task.address_space.destroy().is_err() {
            RUN_QUEUES[index].lock().abort_reap(id, user);
            return Err(SchedulerError::AddressSpaceCleanup);
        }
    }

    let task = RUN_QUEUES[index].lock().finish_reap(id)?;
    crate::gui::close_windows_owned(id.get());
    drop(task);
    Ok(())
}

fn process_deferred_cleanup() {
    for (queue_index, runqueue) in RUN_QUEUES.iter().enumerate() {
        if !online_runqueue(queue_index) {
            continue;
        }
        let (cleanup_tasks, reap_tasks) = {
            let mut scheduler = runqueue.lock();
            let mut cleanup_tasks = [None; PER_CORE_MAX_TASKS];
            let mut cleanup_count = 0;
            let mut reap_tasks = [None; PER_CORE_MAX_TASKS];
            let mut reap_count = 0;
            for index in 0..PER_CORE_MAX_TASKS {
                let ready_for_cleanup = scheduler.tasks[index].as_ref().is_some_and(|task| {
                    task.cleanup_pending
                        && task.state == TaskState::Terminated
                        && task.switch_pending_cpu.is_none()
                        && !scheduler.task_is_running(index)
                });
                if ready_for_cleanup {
                    let task = scheduler.tasks[index]
                        .as_mut()
                        .expect("cleanup task disappeared");
                    task.cleanup_pending = false;
                    cleanup_tasks[cleanup_count] = Some(task.id);
                    cleanup_count += 1;
                }

                let reap = scheduler.tasks[index].as_ref().is_some_and(|task| {
                    task.auto_reap
                        && task.state == TaskState::Terminated
                        && task.switch_pending_cpu.is_none()
                        && !task.reaping
                        && !scheduler.task_is_running(index)
                });
                if reap {
                    let task = scheduler.tasks[index]
                        .as_mut()
                        .expect("auto-reap task disappeared");
                    task.reaping = true;
                    reap_tasks[reap_count] = Some(task.id);
                    reap_count += 1;
                }
            }
            (cleanup_tasks, reap_tasks)
        };
        for task in cleanup_tasks.into_iter().flatten() {
            crate::gui::close_windows_owned(task.get());
        }
        for task in reap_tasks.into_iter().flatten() {
            if let Err(error) = reap_task_from_runqueue(queue_index, task, true) {
                crate::serial_println!(
                    "[WARN] Could not reap terminated task {}: {:?}",
                    task.get(),
                    error
                );
            }
        }
    }
}

fn complete_cpu_switch(cpu_index: usize, allow_cleanup: bool) {
    let owns_runqueue = cpu_index == current_runqueue_index();
    let (should_notify, parents) = {
        let mut scheduler = RUN_QUEUES[cpu_index].lock();
        SCHEDULER_STATS[cpu_index]
            .context_switches
            .fetch_add(1, Ordering::Relaxed);
        if scheduler
            .current_index(cpu_index)
            .and_then(|index| scheduler.tasks[index].as_ref())
            .is_some_and(|task| !task.is_idle)
        {
            SCHEDULER_STATS[cpu_index]
                .tasks_executed
                .fetch_add(1, Ordering::Relaxed);
        }
        let mut changed = false;
        let mut parents = [None; PER_CORE_MAX_TASKS];
        let mut parent_count = 0;
        if owns_runqueue {
            for index in 0..PER_CORE_MAX_TASKS {
                if scheduler.tasks[index]
                    .as_ref()
                    .is_some_and(|task| task.switch_pending_cpu == Some(cpu_index))
                {
                    if let Some(task) = scheduler.tasks[index].as_mut() {
                        task.switch_pending_cpu = None;
                        if task.state == TaskState::Terminated {
                            if let Some(parent) = task.parent {
                                parents[parent_count] = Some(parent);
                                parent_count += 1;
                            }
                            changed = true;
                        }
                    }
                    if scheduler.tasks[index]
                        .as_ref()
                        .is_some_and(|task| task.state == TaskState::Terminated)
                    {
                        scheduler.wake_parent(index);
                    }
                }
            }
        }
        (changed, parents)
    };
    for parent in parents.into_iter().flatten() {
        let _ = wake_task(parent);
    }
    if should_notify {
        crate::smp::wake_secondaries();
    }
    if allow_cleanup {
        process_deferred_cleanup();
    }
    notify_one_idle_cpu(cpu_index);
}

extern "C" fn task_trampoline() -> ! {
    complete_cpu_switch(current_runqueue_index(), true);
    let (entry, user_context, stack_top) = {
        let scheduler = current_runqueue().lock();
        let current = scheduler
            .current_index(current_runqueue_index())
            .expect("task trampoline without current task");
        let task = scheduler.tasks[current]
            .as_ref()
            .expect("current task slot is empty");
        (
            task.entry,
            task.user.as_ref().map(|user| user.context),
            task.stack_top,
        )
    };
    if let Some(context) = user_context {
        assert!(stack_top.is_some(), "user task has no kernel stack");
        unsafe { crate::arch::user::enter(context) }
    }
    let entry = entry.expect("kernel task missing entry");
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
fn scheduler_task_snapshot_reports_current_states() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");
    assert_eq!(scheduler.start(), Ok(first));

    let snapshot = scheduler.task_snapshot();
    assert_eq!(
        snapshot[0],
        Some(TaskInfo {
            id: first,
            parent: None,
            state: TaskState::Running,
            exit_code: None,
            is_idle: false,
            is_user: false,
        })
    );
    assert_eq!(
        snapshot[1],
        Some(TaskInfo {
            id: second,
            parent: None,
            state: TaskState::Ready,
            exit_code: None,
            is_idle: false,
            is_user: false,
        })
    );
    assert!(snapshot[2..].iter().all(Option::is_none));
}

#[test_case]
fn fd_pair_allocation_requires_two_slots_before_mutation() {
    let mut fds = [const { None }; MAX_FDS];
    fds[0] = Some(());
    for slot in fds.iter_mut().skip(2) {
        *slot = Some(());
    }
    assert_eq!(find_free_fd(&fds), Some(1));
    assert_eq!(find_free_fd_pair(&fds), None);
    assert!(fds[1].is_none());
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
fn scheduler_terminates_non_running_task_and_retains_exit_code() {
    let mut scheduler = Scheduler::new();
    let first = scheduler
        .create_task(CpuContext::empty())
        .expect("create first task");
    let second = scheduler
        .create_task(CpuContext::empty())
        .expect("create second task");

    assert_eq!(scheduler.start(), Ok(first));
    assert_eq!(scheduler.terminate_task(second, 130), Ok(()));
    assert_eq!(scheduler.task_state(second), Ok(TaskState::Terminated));
    assert_eq!(
        scheduler.task(second).map(|task| task.exit_code),
        Ok(Some(130))
    );
    assert_eq!(
        scheduler.terminate_task(first, 130),
        Err(SchedulerError::InvalidState)
    );
    assert_eq!(scheduler.reap_terminated(second), Ok(()));
}

#[test_case]
fn migration_moves_a_ready_kernel_task_with_its_stack_and_saved_context() {
    let mut source = RunQueue::new();
    let mut destination = RunQueue::new();
    let context = CpuContext {
        stack_pointer: 0x1234,
        rbx: 1,
        rbp: 2,
        r12: 3,
        r13: 4,
        r14: 5,
        r15: 6,
        rflags: 0x202,
    };
    let id = source
        .create_kernel_task(dummy_kernel_task, None)
        .expect("create migratable kernel task");
    let source_index = source
        .tasks
        .iter()
        .position(|task| task.as_ref().is_some_and(|task| task.id == id))
        .expect("find source task");
    source.tasks[source_index]
        .as_mut()
        .expect("source task disappeared")
        .context = context;
    let stack_top = source.tasks[source_index]
        .as_ref()
        .and_then(|task| task.stack_top);
    assert!(stack_top.is_some());

    assert_eq!(migrate_ready_task(&mut source, &mut destination, 1), Ok(id));
    assert!(source.task(id).is_err());
    let migrated = destination.task(id).expect("find migrated task");
    assert_eq!(migrated.state, TaskState::Ready);
    assert_eq!(migrated.context, context);
    assert_eq!(migrated.stack_top, stack_top);
    assert!(migrated._stack.is_some());
    assert_eq!(migrated.cpu_affinity, None);
}

#[test_case]
fn migration_rejects_running_blocked_sleeping_terminated_and_switching_tasks() {
    let mut source = RunQueue::new();
    let mut destination = RunQueue::new();
    for _ in 0..PER_CORE_MAX_TASKS {
        source
            .create_task(CpuContext::empty())
            .expect("create source task");
    }

    source.current = Some(0);
    source.tasks[0].as_mut().unwrap().state = TaskState::Running;
    source.tasks[1].as_mut().unwrap().state = TaskState::Blocked;
    source.tasks[2].as_mut().unwrap().state = TaskState::Sleeping;
    source.tasks[3].as_mut().unwrap().state = TaskState::Terminated;
    source.tasks[4].as_mut().unwrap().switch_pending_cpu = Some(0);
    source.tasks[5].as_mut().unwrap().kill_pending = true;
    source.tasks[6].as_mut().unwrap().cleanup_pending = true;
    source.tasks[7].as_mut().unwrap().reaping = true;

    assert_eq!(
        migrate_ready_task(&mut source, &mut destination, 1),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(destination.tasks.iter().flatten().count(), 0);
}

#[test_case]
fn migration_respects_affinity_and_competing_stealers_cannot_duplicate_a_task() {
    let mut source = RunQueue::new();
    let mut first_destination = RunQueue::new();
    let mut second_destination = RunQueue::new();
    let id = source
        .create_task(CpuContext::empty())
        .expect("create task to migrate");
    let source_index = source
        .tasks
        .iter()
        .position(|task| task.as_ref().is_some_and(|task| task.id == id))
        .expect("find source task");

    source.tasks[source_index]
        .as_mut()
        .expect("source task disappeared")
        .cpu_affinity = Some(0);
    assert_eq!(
        migrate_ready_task(&mut source, &mut first_destination, 1),
        Err(SchedulerError::NoRunnableTasks)
    );

    source.tasks[source_index]
        .as_mut()
        .expect("source task disappeared")
        .cpu_affinity = Some(1);
    assert_eq!(
        migrate_ready_task(&mut source, &mut first_destination, 1),
        Ok(id)
    );
    assert_eq!(
        migrate_ready_task(&mut source, &mut second_destination, 2),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(first_destination.task(id).unwrap().cpu_affinity, Some(1));
    assert!(second_destination.task(id).is_err());
}

#[test_case]
fn runqueue_load_counts_only_runnable_tasks() {
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
    assert_eq!(scheduler.block_current(), Ok(second));
    assert_eq!(scheduler.block_current(), Ok(third));
    assert_eq!(runnable_task_count(&scheduler, 0), 1);
    assert_eq!(
        scheduler.sleep_current_until(10),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(runnable_task_count(&scheduler, 0), 0);
    assert_eq!(scheduler.wake(first), Ok(()));
    assert_eq!(runnable_task_count(&scheduler, 0), 1);
}

#[test_case]
fn scheduler_defers_reaping_until_a_switching_task_is_off_its_stack() {
    let mut scheduler = Scheduler::new();
    let switching = scheduler
        .create_task(CpuContext::empty())
        .expect("create switching task");
    let next = scheduler
        .create_task(CpuContext::empty())
        .expect("create next task");
    assert_eq!(scheduler.start(), Ok(switching));
    assert_eq!(scheduler.transition_current(TaskState::Ready), Ok(next));
    scheduler.mark_switch_pending(0, 0);

    assert_eq!(scheduler.request_terminate_task(switching, 143), Ok(true));
    assert_eq!(
        scheduler.reap_terminated(switching),
        Err(SchedulerError::InvalidState)
    );

    scheduler.tasks[0]
        .as_mut()
        .expect("switching task disappeared")
        .switch_pending_cpu = None;
    assert_eq!(scheduler.reap_terminated(switching), Ok(()));
}

#[test_case]
fn waking_a_task_in_a_remote_runqueue_makes_it_runnable() {
    let mut remote_queue = Scheduler::new();
    let remote_task = remote_queue
        .create_task(CpuContext::empty())
        .expect("create remote task");
    assert_eq!(remote_queue.start(), Ok(remote_task));
    assert_eq!(
        remote_queue.block_current(),
        Err(SchedulerError::NoRunnableTasks)
    );
    assert_eq!(remote_queue.current_task(), None);

    assert_eq!(remote_queue.wake(remote_task), Ok(()));
    assert_eq!(remote_queue.task_state(remote_task), Ok(TaskState::Ready));
    assert_eq!(remote_queue.start(), Ok(remote_task));
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

    for _ in 1..PER_CORE_MAX_TASKS {
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
    for _ in 0..PER_CORE_MAX_TASKS {
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
    for _ in 0..PER_CORE_MAX_TASKS {
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

#[test_case]
fn fd_mappings_snapshot_sources_for_overlapping_permutations() {
    let mut fds = fd::initial_table();
    let (read_a, _write_a) = fd::create_pipe().expect("create first pipe");
    let (read_b, _write_b) = fd::create_pipe().expect("create second pipe");
    let pipe_a = match read_a {
        FileDescriptor::PipeRead(ref pipe) => pipe.clone(),
        _ => unreachable!(),
    };
    let pipe_b = match read_b {
        FileDescriptor::PipeRead(ref pipe) => pipe.clone(),
        _ => unreachable!(),
    };
    fds[3] = Some(read_a);
    fds[4] = Some(read_b);

    apply_fd_mappings(
        &mut fds,
        &[
            FdMapping {
                source: 3,
                target: 4,
            },
            FdMapping {
                source: 4,
                target: 3,
            },
        ],
        &[],
    )
    .expect("swap overlapping descriptors");

    let points_to_pipe = |descriptor: &Option<FileDescriptor>, expected: &_| matches!(descriptor, Some(FileDescriptor::PipeRead(pipe)) if alloc::sync::Arc::ptr_eq(pipe, expected));
    assert!(points_to_pipe(&fds[3], &pipe_b));
    assert!(points_to_pipe(&fds[4], &pipe_a));
}

#[test_case]
fn fd_mappings_repeated_destinations_use_last_mapping_and_close_sources() {
    let mut fds = fd::initial_table();
    let (read_a, _write_a) = fd::create_pipe().expect("create first pipe");
    let (read_b, _write_b) = fd::create_pipe().expect("create second pipe");
    let (old_destination, _old_writer) = fd::create_pipe().expect("create destination pipe");
    let expected = match read_b {
        FileDescriptor::PipeRead(ref pipe) => pipe.clone(),
        _ => unreachable!(),
    };
    fds[3] = Some(read_a);
    fds[4] = Some(read_b);
    fds[5] = Some(old_destination);

    apply_fd_mappings(
        &mut fds,
        &[
            FdMapping {
                source: 3,
                target: 5,
            },
            FdMapping {
                source: 4,
                target: 5,
            },
        ],
        &[],
    )
    .expect("apply repeated destination mappings");

    assert!(fds[3].is_none());
    assert!(fds[4].is_none());
    assert!(matches!(
        &fds[5],
        Some(FileDescriptor::PipeRead(pipe)) if alloc::sync::Arc::ptr_eq(pipe, &expected)
    ));
    assert_eq!(fd::write(&_write_a, b"x"), Err(fd::WriteError::BrokenPipe));
    assert_eq!(
        fd::write(&_old_writer, b"x"),
        Err(fd::WriteError::BrokenPipe)
    );
}

#[test_case]
fn invalid_fd_mappings_do_not_modify_the_descriptor_table() {
    let mut fds = fd::initial_table();
    let (reader, _writer) = fd::create_pipe().expect("create pipe");
    let expected = match &reader {
        FileDescriptor::PipeRead(pipe) => pipe.clone(),
        _ => unreachable!(),
    };
    fds[3] = Some(reader);
    let original_count = alloc::sync::Arc::strong_count(&expected);
    for mapping in [
        FdMapping {
            source: MAX_FDS,
            target: 0,
        },
        FdMapping {
            source: 3,
            target: MAX_FDS,
        },
        FdMapping {
            source: 7,
            target: 0,
        },
    ] {
        assert_eq!(
            apply_fd_mappings(&mut fds, &[mapping], &[]),
            Err(SchedulerError::InvalidFd)
        );
        assert_eq!(alloc::sync::Arc::strong_count(&expected), original_count);
        assert!(matches!(
            &fds[3],
            Some(FileDescriptor::PipeRead(pipe)) if alloc::sync::Arc::ptr_eq(pipe, &expected)
        ));
    }
    assert_eq!(
        apply_fd_mappings(
            &mut fds,
            &[
                FdMapping {
                    source: 3,
                    target: 4,
                },
                FdMapping {
                    source: 7,
                    target: 5,
                },
            ],
            &[],
        ),
        Err(SchedulerError::InvalidFd)
    );
    assert!(fds[4].is_none());
    assert!(matches!(
        &fds[3],
        Some(FileDescriptor::PipeRead(pipe)) if alloc::sync::Arc::ptr_eq(pipe, &expected)
    ));
    assert_eq!(
        apply_fd_mappings(&mut fds, &[], &[MAX_FDS]),
        Err(SchedulerError::InvalidFd)
    );
    assert_eq!(alloc::sync::Arc::strong_count(&expected), original_count);
}

#[test_case]
fn empty_fd_mappings_preserve_inherited_descriptors() {
    let mut fds = fd::initial_table();
    let (reader, _writer) = fd::create_pipe().expect("create pipe");
    let expected = match &reader {
        FileDescriptor::PipeRead(pipe) => pipe.clone(),
        _ => unreachable!(),
    };
    fds[3] = Some(reader);
    let original_count = alloc::sync::Arc::strong_count(&expected);

    apply_fd_mappings(&mut fds, &[], &[]).expect("empty mapping list");

    assert_eq!(alloc::sync::Arc::strong_count(&expected), original_count);
    assert!(matches!(
        &fds[3],
        Some(FileDescriptor::PipeRead(pipe)) if alloc::sync::Arc::ptr_eq(pipe, &expected)
    ));
}

#[test_case]
fn scheduler_assigns_distinct_ready_tasks_to_secondary_cpus() {
    let mut first_queue = RunQueue::new();
    let mut second_queue = RunQueue::new();
    let first = first_queue
        .insert_task_with_id(TaskId(1), CpuContext::empty(), None, None, None, None, None)
        .expect("create first task");
    let second = second_queue
        .insert_task_with_id(TaskId(2), CpuContext::empty(), None, None, None, None, None)
        .expect("create second task");

    assert_eq!(first_queue.select_next(0, 0), Ok(first));
    assert_eq!(second_queue.select_next(0, 1), Ok(second));
    assert_eq!(first_queue.current, Some(0));
    assert_eq!(second_queue.current, Some(0));
    assert_eq!(first_queue.task_state(first), Ok(TaskState::Running));
    assert_eq!(second_queue.task_state(second), Ok(TaskState::Running));
}

#[test_case]
fn detached_tasks_have_no_parent_and_termination_is_deferred_if_running() {
    let mut scheduler = Scheduler::new();
    let first =
        spawn_detached_into(&mut scheduler, dummy_kernel_task).expect("spawn detached shell task");
    assert_eq!(scheduler.task(first).map(|task| task.parent), Ok(None));
    assert_eq!(scheduler.select_next(0, 1), Ok(first));
    assert_eq!(scheduler.request_terminate_task(first, 0), Ok(true));
    let task = scheduler.task(first).expect("find requested task");
    assert!(task.kill_pending);
    assert!(task.auto_reap);
    assert_eq!(task.state, TaskState::Running);

    let mut queued = Scheduler::new();
    let queued_task = spawn_detached_into(&mut queued, dummy_kernel_task)
        .expect("spawn another detached shell task");
    assert_eq!(queued.request_terminate_task(queued_task, 0), Ok(false));
    let task = queued.task(queued_task).expect("find terminated task");
    assert!(task.auto_reap);
    assert_eq!(task.state, TaskState::Terminated);
    assert_eq!(task.switch_pending_cpu, None);
}

#[test_case]
fn new_task_stack_is_allocated_aligned_and_contains_its_entry() {
    let (context, stack, stack_top) = create_task_stack().expect("allocate task stack");
    let stack_start = stack.as_ptr() as usize;
    let stack_end = stack_start + core::mem::size_of_val(&*stack);
    let initial_stack = context.stack_pointer as usize;

    assert_eq!(stack_start % STACK_ALIGNMENT, 0);
    assert_eq!(initial_stack % STACK_ALIGNMENT, 0);
    assert!(initial_stack >= stack_start);
    assert!(initial_stack + core::mem::size_of::<u64>() <= stack_end);
    assert_eq!(stack_top.as_u64() as usize, stack_end);
    assert_eq!(
        unsafe { (initial_stack as *const u64).read() },
        task_trampoline as *const () as usize as u64
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
    while !SLEEP_TEST_STARTED.load(Ordering::SeqCst) {
        yield_now().expect("yield while waiting for the sleeper to start");
    }

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

    let sleeper = spawn(sleep_ticks_test_sleeper).expect("spawn sleeping task");

    SLEEP_TEST_TASK_ID.store(sleeper.get(), Ordering::SeqCst);

    let other = spawn(sleep_ticks_test_other_task).expect("spawn second task");

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
