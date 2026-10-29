//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;
///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// A simple FIFO scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Take a process out of the ready queue
    /// 选择 stride 最小的就绪进程，并更新它的累计值。
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        const BIG_STRIDE: usize = 1 << 32;

        // 没有就绪进程
        if self.ready_queue.is_empty() {
            return None;
        }

        // 暂时认为第 0 个进程的 stride 最小
        let mut min_index = 0;
        let mut min_stride = self.ready_queue[0]
            .inner_exclusive_access()
            .stride;

        // 遍历其余进程，找出 stride 最小的那个
        for (index, task) in self.ready_queue.iter().enumerate().skip(1) {
            let stride = task.inner_exclusive_access().stride;

            // 使用环绕差值比较，处理累计值溢出的情况
            if (stride.wrapping_sub(min_stride) as isize) < 0 {
                min_index = index;
                min_stride = stride;
            }
        }

        // 从就绪队列中移除选中的进程
        let task = self.ready_queue.remove(min_index)?;

        {
            let mut inner = task.inner_exclusive_access();

            // 优先级越大，步长越小
            // max(1) 避免整数除法得到 0，导致累计值不增长
            let pass = (BIG_STRIDE / inner.priority).max(1);

            inner.stride = inner.stride.wrapping_add(pass);
        } // inner 在这里释放借用，下面才能移动 task

        Some(task)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
