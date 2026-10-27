//! Process management syscalls
use crate::mm::{PTEFlags, PageTable, PhysAddr, VirtAddr};
use crate::task::{change_program_brk, exit_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_user_token, get_record_syscall, mmap_current, munmap_current};
use crate::timer::get_time_us;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

// AI 辅助：OpenAI Codex 提供并补全了本文件中的用户地址翻译、get_time、trace
// 和 mmap/munmap 接口实现；对应讨论与参考来源见 reports/lab2.md。
/// 将秒数和微秒余数写入用户的 TimeVal，支持跨页。
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

/// 读写当前任务的一个字节，或查询系统调用次数。
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    if trace_request == 2 {
        // 请求 2 中 id 是系统调用编号，不是地址
        return get_record_syscall(id);
    }
    let permission = match trace_request {
        0 => PTEFlags::R, // 请求 0：读取，需要 R 权限
        1 => PTEFlags::W, // 请求 1：写入，需要 W 权限
        _ => return -1,   // 其他请求不支持
    };
    let table = PageTable::from_token(current_user_token()); // 查询当前用户页表
    let pa = match user_pa(&table, id, permission) {
        Some(pa) => pa,    // 此时 id 被当作用户虚拟地址
        None => return -1, // 未映射或没有权限
    };
    if trace_request == 0 {
        unsafe { (pa as *const u8).read_volatile() as isize } // 读取一个字节，返回 0～255
    } else {
        unsafe { (pa as *mut u8).write_volatile(data as u8) }; // 只写 data 的低 8 位
        0 // 成功写入，返回 0
    }
}

/// 申请当前任务的匿名内存映射。
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    mmap_current(start, len, prot) // 查当前任务并修改其 MemorySet
}

/// 撤销当前任务的用户内存映射。
pub fn sys_munmap(start: usize, len: usize) -> isize {
    munmap_current(start, len) // 同上，具体释放逻辑在 MemorySet
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

// 把一个用户虚拟地址翻译成内核能访问的物理地址，并验证访问权限。
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
