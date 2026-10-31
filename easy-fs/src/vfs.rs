use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        if !Self::valid_name(name) {
            return None;
        }
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        if !Self::valid_name(name) {
            return None;
        }
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            self.insert_entry(root_inode, &mut fs, name, new_inode_id);
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 27
            && !name.contains('/')
            && !name.contains('\0')
            && name != "."
            && name != ".."
    }
    fn insert_entry(
        &self,
        dir: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
        name: &str,
        id: u32,
    ) {
        let count = dir.size as usize / DIRENT_SZ;
        let mut slot = count;
        for i in 0..count {
            let mut entry = DirEntry::empty();
            dir.read_at(i * DIRENT_SZ, entry.as_bytes_mut(), &self.block_device);
            if entry.name().is_empty() {
                slot = i;
                break;
            }
        }
        if slot == count {
            self.increase_size(((count + 1) * DIRENT_SZ) as u32, dir, fs);
        }
        dir.write_at(
            slot * DIRENT_SZ,
            DirEntry::new(name, id).as_bytes(),
            &self.block_device,
        );
    }
    /// Create another directory entry referring to the same inode.
    pub fn link(&self, old: &str, new: &str) -> Option<()> {
        if !Self::valid_name(old) || !Self::valid_name(new) || old == new {
            return None;
        }
        let mut fs = self.fs.lock();
        let id = self.read_disk_inode(|dir| {
            if self.find_inode_id(new, dir).is_some() {
                None
            } else {
                self.find_inode_id(old, dir)
            }
        })?;
        self.modify_disk_inode(|dir| self.insert_entry(dir, &mut fs, new, id));
        block_cache_sync_all();
        Some(())
    }
    /// Remove one name and reclaim storage when no links or open descriptions remain.
    pub fn unlink(&self, name: &str) -> Option<()> {
        if !Self::valid_name(name) {
            return None;
        }
        let mut fs = self.fs.lock();
        let (id, remaining) = self.modify_disk_inode(|dir| {
            let id = self.find_inode_id(name, dir)?;
            let mut remaining = 0;
            for i in 0..dir.size as usize / DIRENT_SZ {
                let mut entry = DirEntry::empty();
                dir.read_at(i * DIRENT_SZ, entry.as_bytes_mut(), &self.block_device);
                if entry.name() == name {
                    dir.write_at(
                        i * DIRENT_SZ,
                        DirEntry::empty().as_bytes(),
                        &self.block_device,
                    );
                } else if !entry.name().is_empty() && entry.inode_id() == id {
                    remaining += 1;
                }
            }
            Some((id, remaining))
        })?;
        if remaining == 0 {
            if fs.open_counts.get(&id).copied().unwrap_or(0) == 0 {
                fs.reclaim_inode(id);
            } else {
                fs.unlinked.insert(id);
            }
        }
        block_cache_sync_all();
        Some(())
    }
    /// Return inode number, directory flag and hard-link count.
    pub fn stat(&self) -> (u32, bool, u32) {
        let fs = self.fs.lock();
        let id = fs.inode_id(self.block_id, self.block_offset);
        let is_dir = self.read_disk_inode(|inode| inode.is_dir());
        if id == 0 {
            return (id, is_dir, 1);
        }
        let (block, offset) = fs.get_disk_inode_pos(0);
        let links = get_block_cache(block as usize, self.block_device.clone())
            .lock()
            .read(offset, |root: &DiskInode| {
                let mut links = 0;
                for i in 0..root.size as usize / DIRENT_SZ {
                    let mut entry = DirEntry::empty();
                    root.read_at(i * DIRENT_SZ, entry.as_bytes_mut(), &self.block_device);
                    if !entry.name().is_empty() && entry.inode_id() == id {
                        links += 1;
                    }
                }
                links
            });
        (id, is_dir, links)
    }
    /// Retain an open file description.
    pub fn open_ref(&self) {
        let mut fs = self.fs.lock();
        let id = fs.inode_id(self.block_id, self.block_offset);
        *fs.open_counts.entry(id).or_insert(0) += 1;
    }
    /// Release an open description, reclaiming a pending deletion on the last close.
    pub fn close_ref(&self) {
        let mut fs = self.fs.lock();
        let id = fs.inode_id(self.block_id, self.block_offset);
        let count = fs.open_counts.get_mut(&id).unwrap();
        *count -= 1;
        if *count == 0 {
            fs.open_counts.remove(&id);
            if fs.unlinked.contains(&id) {
                fs.reclaim_inode(id);
            }
        }
        block_cache_sync_all();
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if !dirent.name().is_empty() {
                    v.push(String::from(dirent.name()));
                }
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
