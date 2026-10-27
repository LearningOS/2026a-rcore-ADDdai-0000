//! Implementation of [`MapArea`] and [`MemorySet`].

use super::{frame_alloc, FrameTracker};
use super::{PTEFlags, PageTable, PageTableEntry};
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
use crate::config::{
    KERNEL_STACK_SIZE, MEMORY_END, PAGE_SIZE, TRAMPOLINE, TRAP_CONTEXT_BASE, USER_STACK_SIZE,
};
use crate::sync::UPSafeCell;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
use lazy_static::*;
use riscv::register::satp;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn strampoline();
}

lazy_static! {
    /// The kernel's initial memory mapping(kernel address space)
    pub static ref KERNEL_SPACE: Arc<UPSafeCell<MemorySet>> =
        Arc::new(unsafe { UPSafeCell::new(MemorySet::new_kernel()) });
}
/// address space
pub struct MemorySet {
    page_table: PageTable,
    areas: Vec<MapArea>,
}

impl MemorySet {
    // AI 辅助：OpenAI Codex 提供并补全了本章范围校验、匿名映射和区域拆分回收逻辑。
    // 参考课程 chapter4/7exercise.html，交流与验证记录见 reports/lab2.md。
    // 将字节区间转换成左闭右开的页号区间。
    fn checked_page_range(start: usize, len: usize) -> Option<VPNRange> {
        if start % PAGE_SIZE != 0 {
            return None; // 起始地址必须按 4096 字节对齐
        }
        let va = VirtAddr::from(start);
        if usize::from(va) != start {
            return None; // 拒绝非法的 SV39 地址，防止高位被截断后访问别处
        }
        let first = va.floor(); // 起始虚拟页号
        if len == 0 {
            return Some(VPNRange::new(first, first)); // 空区间不需要任何页面
        }
        let last_addr = start.checked_add(len - 1)?; // 最后一个字节，溢出则失败
        let last_va = VirtAddr::from(last_addr);
        if usize::from(last_va) != last_addr || (start >> 38) != (last_addr >> 38) {
            return None; // 末地址也必须合法，而且区间不能穿过 SV39 地址空洞
        }
        let end = VirtPageNum(last_va.floor().0 + 1); // 最后一个页面的下一页
        Some(VPNRange::new(first, end)) // 遍历时包含 first，不包含 end
    }
    /// 为用户按页分配内存，成功返回 0，参数错误或页帧不足返回 -1。
    pub fn mmap(&mut self, start: usize, len: usize, prot: usize) -> isize {
        if prot & !0x7 != 0 || prot & 0x7 == 0 {
            return -1; // 只接受低三位，且至少指定一种权限
        }
        let range = match Self::checked_page_range(start, len) {
            Some(range) => range,
            None => return -1,
        };
        if range.get_start() == range.get_end() {
            return 0; // 长度为 0，无须分配
        }
        for vpn in range {
            if let Some(pte) = self.translate(vpn) {
                if pte.is_valid() {
                    return -1; // 先检查全部页面，已有映射就拒绝
                }
            }
        }
        let permission = MapPermission::from_bits((prot << 1) as u8).unwrap() | MapPermission::U; // prot 的 R/W/X 左移一位，再允许用户态访问
        let flags = PTEFlags::from_bits(permission.bits).unwrap(); // 转为页表项权限
        let mut area = MapArea {
            vpn_range: range,             // 记录完整虚拟页区间
            data_frames: BTreeMap::new(), // 稍后持有分配到的 FrameTracker
            map_type: MapType::Framed,    // 每个虚拟页分配一个独立物理页
            map_perm: permission,
        };
        for vpn in range {
            // 小闭包让 ? 可以在“分配一页”这一步返回 None。
            let mapped = (|| -> Option<()> {
                let frame = frame_alloc()?; // 分配并清零一个数据页
                self.page_table.try_map(vpn, frame.ppn, flags)?; // 建立页表映射
                area.data_frames.insert(vpn, frame); // 把页帧所有权交给 area
                Some(())
            })(); // 最后的 () 表示立即执行这个闭包
            if mapped.is_none() {
                for mapped_vpn in area.data_frames.keys() {
                    self.page_table.unmap(*mapped_vpn); // 撤销本次已经建立的映射
                }
                return -1; // area 离开作用域，其 FrameTracker 自动回收数据页
            }
        }
        self.areas.push(area); // 成功后保存区域，保证这些物理页继续存活
        0
    }
    /// 撤销用户页面映射并回收数据页，支持只撤销一个区域的中间部分。
    pub fn munmap(&mut self, start: usize, len: usize) -> isize {
        let range = match Self::checked_page_range(start, len) {
            Some(range) => range,
            None => return -1,
        };
        let left = range.get_start(); // 要删除的起始页，包含它
        let right = range.get_end(); // 要删除的终点页，不包含它
        if left == right {
            return 0; // 空区间
        }
        for vpn in range {
            let pte = match self.translate(vpn) {
                Some(pte) => pte,
                None => return -1, // 页表中不存在对应项
            };
            if !pte.flags().contains(PTEFlags::V | PTEFlags::U) {
                return -1; // 无效页不能删除，也不允许删除内核专用页
            }
            if !self
                .areas
                .iter()
                .any(|area| area.map_type == MapType::Framed && area.data_frames.contains_key(&vpn))
            {
                return -1; // 确认该用户页的物理页帧由我们的区域管理
            }
        }
        let old_areas = core::mem::take(&mut self.areas); // 取走原 Vec，原位置留下空 Vec
        for mut area in old_areas {
            let begin = area.vpn_range.get_start(); // 此区域的起点
            let end = area.vpn_range.get_end(); // 此区域的终点
            if begin == end || end <= left || begin >= right {
                self.areas.push(area); // 空区域或没有交集，原样保留
                continue;
            }
            let cut_begin = begin.max(left); // 实际删除范围：两个区间的交集
            let cut_end = end.min(right);
            let permission = area.map_perm; // 后面构造右半段时需要原权限
            let map_type = area.map_type;
            let right_frames = area.data_frames.split_off(&cut_end); // 取出右侧保留页面
            let removed_frames = area.data_frames.split_off(&cut_begin); // 取出中间待删页面
            for vpn in VPNRange::new(cut_begin, cut_end) {
                self.page_table.unmap(vpn); // 先让页表项失效
            }
            drop(removed_frames); // 再释放 FrameTracker，物理数据页归还分配器
            if begin < cut_begin {
                area.vpn_range = VPNRange::new(begin, cut_begin); // 左半段的新区间
                self.areas.push(area); // area 现在只持有左半段的页帧
            }
            if cut_end < end {
                self.areas.push(MapArea {
                    vpn_range: VPNRange::new(cut_end, end), // 右半段的新区间
                    data_frames: right_frames,              // 右侧原有页帧继续保留，不复制内容
                    map_type,
                    map_perm: permission,
                });
            }
        }
        0
    }

    /// Create a new empty `MemorySet`.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
        }
    }
    /// Get the page table token
    pub fn token(&self) -> usize {
        self.page_table.token()
    }
    /// Assume that no conflicts.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission),
            None,
        );
    }
    fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) {
        map_area.map(&mut self.page_table);
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data);
        }
        self.areas.push(map_area);
    }
    /// Mention that trampoline is not collected by areas.
    fn map_trampoline(&mut self) {
        self.page_table.map(
            VirtAddr::from(TRAMPOLINE).into(),
            PhysAddr::from(strampoline as usize).into(),
            PTEFlags::R | PTEFlags::X,
        );
    }
    /// Without kernel stacks.
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // map kernel sections
        info!(".text [{:#x}, {:#x})", stext as usize, etext as usize);
        info!(".rodata [{:#x}, {:#x})", srodata as usize, erodata as usize);
        info!(".data [{:#x}, {:#x})", sdata as usize, edata as usize);
        info!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as usize, ebss as usize
        );
        info!("mapping .text section");
        memory_set.push(
            MapArea::new(
                (stext as usize).into(),
                (etext as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::X,
            ),
            None,
        );
        info!("mapping .rodata section");
        memory_set.push(
            MapArea::new(
                (srodata as usize).into(),
                (erodata as usize).into(),
                MapType::Identical,
                MapPermission::R,
            ),
            None,
        );
        info!("mapping .data section");
        memory_set.push(
            MapArea::new(
                (sdata as usize).into(),
                (edata as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        info!("mapping .bss section");
        memory_set.push(
            MapArea::new(
                (sbss_with_stack as usize).into(),
                (ebss as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        info!("mapping physical memory");
        memory_set.push(
            MapArea::new(
                (ekernel as usize).into(),
                MEMORY_END.into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        memory_set
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// also returns user_sp_base and entry point.
    pub fn from_elf(elf_data: &[u8]) -> (Self, usize, usize) {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // map program headers of elf, with U flag
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        let mut max_end_vpn = VirtPageNum(0);
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize).into();
                let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize).into();
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                let map_area = MapArea::new(start_va, end_va, MapType::Framed, map_perm);
                max_end_vpn = map_area.vpn_range.get_end();
                memory_set.push(
                    map_area,
                    Some(&elf.input[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize]),
                );
            }
        }
        // map user stack with U flags
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_stack_bottom: usize = max_end_va.into();
        // guard page
        user_stack_bottom += PAGE_SIZE;
        let user_stack_top = user_stack_bottom + USER_STACK_SIZE;
        memory_set.push(
            MapArea::new(
                user_stack_bottom.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
        );
        // used in sbrk
        memory_set.push(
            MapArea::new(
                user_stack_top.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
        );
        // map TrapContext
        memory_set.push(
            MapArea::new(
                TRAP_CONTEXT_BASE.into(),
                TRAMPOLINE.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        (
            memory_set,
            user_stack_top,
            elf.header.pt2.entry_point() as usize,
        )
    }
    /// Change page table by writing satp CSR Register.
    pub fn activate(&self) {
        let satp = self.page_table.token();
        unsafe {
            satp::write(satp);
            asm!("sfence.vma");
        }
    }
    /// Translate a virtual page number to a page table entry
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.page_table.translate(vpn)
    }
    /// shrink the area to new_end
    #[allow(unused)]
    pub fn shrink_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.shrink_to(&mut self.page_table, new_end.ceil());
            true
        } else {
            false
        }
    }

    /// append the area to new_end
    #[allow(unused)]
    pub fn append_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.append_to(&mut self.page_table, new_end.ceil());
            true
        } else {
            false
        }
    }
}
/// map area structure, controls a contiguous piece of virtual memory
pub struct MapArea {
    vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,
    map_type: MapType,
    map_perm: MapPermission,
}

impl MapArea {
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
    ) -> Self {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(),
            map_type,
            map_perm,
        }
    }
    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
            }
            MapType::Framed => {
                let frame = frame_alloc().unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
        }
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        page_table.map(vpn, ppn, pte_flags);
    }
    #[allow(unused)]
    pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        if self.map_type == MapType::Framed {
            self.data_frames.remove(&vpn);
        }
        page_table.unmap(vpn);
    }
    pub fn map(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.map_one(page_table, vpn);
        }
    }
    #[allow(unused)]
    pub fn unmap(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.unmap_one(page_table, vpn);
        }
    }
    #[allow(unused)]
    pub fn shrink_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        for vpn in VPNRange::new(new_end, self.vpn_range.get_end()) {
            self.unmap_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    #[allow(unused)]
    pub fn append_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        for vpn in VPNRange::new(self.vpn_range.get_end(), new_end) {
            self.map_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    /// data: start-aligned but maybe with shorter length
    /// assume that all frames were cleared before
    pub fn copy_data(&mut self, page_table: &mut PageTable, data: &[u8]) {
        assert_eq!(self.map_type, MapType::Framed);
        let mut start: usize = 0;
        let mut current_vpn = self.vpn_range.get_start();
        let len = data.len();
        loop {
            let src = &data[start..len.min(start + PAGE_SIZE)];
            let dst = &mut page_table
                .translate(current_vpn)
                .unwrap()
                .ppn()
                .get_bytes_array()[..src.len()];
            dst.copy_from_slice(src);
            start += PAGE_SIZE;
            if start >= len {
                break;
            }
            current_vpn.step();
        }
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
/// map type for memory set: identical or framed
pub enum MapType {
    Identical,
    Framed,
}

bitflags! {
    /// map permission corresponding to that in pte: `R W X U`
    pub struct MapPermission: u8 {
        ///Readable
        const R = 1 << 1;
        ///Writable
        const W = 1 << 2;
        ///Excutable
        const X = 1 << 3;
        ///Accessible in U mode
        const U = 1 << 4;
    }
}

/// Return (bottom, top) of a kernel stack in kernel space.
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    let top = TRAMPOLINE - app_id * (KERNEL_STACK_SIZE + PAGE_SIZE);
    let bottom = top - KERNEL_STACK_SIZE;
    (bottom, top)
}

/// remap test in kernel space
#[allow(unused)]
pub fn remap_test() {
    let mut kernel_space = KERNEL_SPACE.exclusive_access();
    let mid_text: VirtAddr = ((stext as usize + etext as usize) / 2).into();
    let mid_rodata: VirtAddr = ((srodata as usize + erodata as usize) / 2).into();
    let mid_data: VirtAddr = ((sdata as usize + edata as usize) / 2).into();
    assert!(!kernel_space
        .page_table
        .translate(mid_text.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_rodata.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_data.floor())
        .unwrap()
        .executable(),);
    println!("remap_test passed!");
}
