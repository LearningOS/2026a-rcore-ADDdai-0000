//! File and filesystem-related syscalls
use crate::fs::{open_file, OpenFlags, Stat};
use crate::mm::{translated_byte_buffer, translated_str, UserBuffer};
use crate::task::{current_task, current_user_token};

pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        if !file.writable() {
            return -1;
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        if !file.readable() {
            return -1;
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("kernel: sys_read .. file.read");
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_open(path: *const u8, flags: u32) -> isize {
    trace!("kernel:pid[{}] sys_open", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(inode) = open_file(path.as_str(), OpenFlags::from_bits(flags).unwrap()) {
        let mut inner = task.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}

pub fn sys_close(fd: usize) -> isize {
    trace!("kernel:pid[{}] sys_close", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    inner.fd_table[fd].take();
    0
}

/// Translate one user byte with the requested access permission.
fn user_byte(token: usize, address: usize, permission: crate::mm::PTEFlags) -> Option<*mut u8> {
    use crate::mm::{PTEFlags, PageTable, PhysAddr, VirtAddr};
    let va = VirtAddr::from(address);
    if usize::from(va) != address {
        return None;
    }
    let pte = PageTable::from_token(token).translate(va.floor())?;
    if !pte.flags().contains(PTEFlags::V | PTEFlags::U | permission) {
        return None;
    }
    Some((PhysAddr::from(pte.ppn()).0 + va.page_offset()) as *mut u8)
}

fn user_name(token: usize, ptr: *const u8) -> Option<alloc::string::String> {
    let mut bytes = alloc::vec::Vec::new();
    for i in 0..28 {
        let address = (ptr as usize).checked_add(i)?;
        let byte = unsafe { *user_byte(token, address, crate::mm::PTEFlags::R)? };
        if byte == 0 {
            return alloc::string::String::from_utf8(bytes).ok();
        }
        bytes.push(byte);
    }
    None
}

/// Copy a complete stat to the user buffer, including across page boundaries.
pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    let file = match inner.fd_table.get(fd).and_then(|file| file.as_ref()) {
        Some(file) => file.clone(),
        None => return -1,
    };
    let token = inner.memory_set.token();
    drop(inner);
    let stat = match file.stat() {
        Some(stat) => stat,
        None => return -1,
    };
    let mut bytes = [0u8; core::mem::size_of::<Stat>()];
    bytes[0..8].copy_from_slice(&stat.dev.to_ne_bytes());
    bytes[8..16].copy_from_slice(&stat.ino.to_ne_bytes());
    bytes[16..20].copy_from_slice(&stat.mode.bits().to_ne_bytes());
    bytes[20..24].copy_from_slice(&stat.nlink.to_ne_bytes());
    let mut pointers = alloc::vec::Vec::new();
    for i in 0..bytes.len() {
        let address = match (st as usize).checked_add(i) {
            Some(a) => a,
            None => return -1,
        };
        match user_byte(
            token,
            address,
            crate::mm::PTEFlags::R | crate::mm::PTEFlags::W,
        ) {
            Some(p) => pointers.push(p),
            None => return -1,
        }
    }
    for (ptr, byte) in pointers.into_iter().zip(bytes) {
        unsafe {
            ptr.write(byte);
        }
    }
    0
}

/// Add a name for the existing inode.
pub fn sys_linkat(old_name: *const u8, new_name: *const u8) -> isize {
    let token = current_user_token();
    let (old, new) = match (user_name(token, old_name), user_name(token, new_name)) {
        (Some(old), Some(new)) => (old, new),
        _ => return -1,
    };
    if crate::fs::ROOT_INODE.link(&old, &new).is_some() {
        0
    } else {
        -1
    }
}

/// Remove a name and reclaim the file when no references remain.
pub fn sys_unlinkat(name: *const u8) -> isize {
    let name = match user_name(current_user_token(), name) {
        Some(name) => name,
        None => return -1,
    };
    if crate::fs::ROOT_INODE.unlink(&name).is_some() {
        0
    } else {
        -1
    }
}
