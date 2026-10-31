//! Process management syscalls
use crate::mm::{PTEFlags, PageTable, PhysAddr, VirtAddr};
use crate::task::TaskControlBlock;
use crate::timer::get_time_us;
use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    let us = get_time_us(); // 取得本次时间快照，单位为微秒
    const WORD: usize = core::mem::size_of::<usize>(); // RV64 下是 8 字节
    const SIZE: usize = WORD * 2; // TimeVal 的两个 usize 共 16 字节
    let mut bytes = [0u8; SIZE]; // 在内核中准备待写入内容
    bytes[..WORD].copy_from_slice(&(us / 1_000_000).to_ne_bytes()); // 整秒数
    bytes[WORD..].copy_from_slice(&(us % 1_000_000).to_ne_bytes()); // 不足一秒的微秒数
    let table = PageTable::from_token(current_user_token()); // 用户自己的页表
    let mut addresses = [0usize; SIZE]; // 保存每个目标字节翻译后的物理地址
    for i in 0..SIZE {
        let va = match (ts as usize).checked_add(i) {
            // 防止地址相加溢出
            Some(va) => va,
            None => return -1,
        };
        addresses[i] = match user_pa(&table, va, PTEFlags::W) {
            Some(pa) => pa,    // 每个字节都翻译，所以能跨越不连续的物理页
            None => return -1, // 先检查全部地址，失败时尚未写入任何字节
        };
    }
    for i in 0..SIZE {
        unsafe { (addresses[i] as *mut u8).write_volatile(bytes[i]) }; // 逐字节写入
    }
    0 // 写入完成
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    inner.memory_set.mmap(start, len, prot)
}

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    inner.memory_set.munmap(start, len)
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(_path: *const u8) -> isize {
    let token = current_user_token();
    let path = translated_str(token, _path);
    let elf_data = match open_file(path.as_str(), OpenFlags::RDONLY) {
        Some(inode) => inode.read_all(),
        None => return -1,
    };

    let child = Arc::new(TaskControlBlock::new(&elf_data));
    let child_pid = child.pid.0;

    let parent = current_task().unwrap();
    child.inner_exclusive_access().parent = Some(Arc::downgrade(&parent));
    child.inner_exclusive_access().fd_table = parent.inner_exclusive_access().fd_table.clone();

    parent
        .inner_exclusive_access()
        .children
        .push(Arc::clone(&child));

    add_task(child);
    child_pid as isize
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    if _prio < 2 {
        return -1;
    }
    let task = current_task().unwrap();
    task.inner_exclusive_access().priority = _prio as usize;
    _prio
}

fn user_pa(table: &PageTable, addr: usize, permission: PTEFlags) -> Option<usize> {
    let va = VirtAddr::from(addr); // 封装为项目的虚拟地址类型
    if usize::from(va) != addr {
        // 检查 SV39 地址是否合法，不能只截断高位
        return None;
    }
    let pte = table.translate(va.floor())?; // 用虚拟页号查页表；查不到直接返回 None
    let required = PTEFlags::V | PTEFlags::U | permission; // 有效、用户可访问、指定读写权限
    if !pte.flags().contains(required) {
        // contains 检查这些位是否全部存在
        return None;
    }
    let page_start = PhysAddr::from(pte.ppn()); // 物理页号转换为物理页起始地址
    Some(page_start.0 + va.page_offset()) // 加上页内偏移，定位实际字节
}
