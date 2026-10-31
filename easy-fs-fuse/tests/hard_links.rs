use easy_fs::{BlockDevice, EasyFileSystem};
use std::sync::{Arc, Mutex};

struct MemoryDisk(Mutex<Vec<u8>>);
impl BlockDevice for MemoryDisk {
    fn read_block(&self, id: usize, buf: &mut [u8]) {
        buf.copy_from_slice(&self.0.lock().unwrap()[id * 512..id * 512 + buf.len()]);
    }
    fn write_block(&self, id: usize, buf: &[u8]) {
        self.0.lock().unwrap()[id * 512..id * 512 + buf.len()].copy_from_slice(buf);
    }
}

#[test]
fn hard_links_keep_open_files_alive_and_recycle_storage() {
    let device: Arc<dyn BlockDevice> = Arc::new(MemoryDisk(Mutex::new(vec![0; 4096 * 512])));
    let fs = EasyFileSystem::create(Arc::clone(&device), 4096, 1);
    let root = EasyFileSystem::root_inode(&fs);
    let file = root.create("original").unwrap();
    file.open_ref();
    let id = file.stat().0;
    let data = vec![0x5a; 80_000]; // Exercise direct and both indirect levels.
    file.write_at(0, &data);
    assert_eq!(file.stat().2, 1);
    assert!(root.link("missing", "alias").is_none());
    assert!(root.link("original", "original").is_none());
    assert!(root.link("original", &"x".repeat(28)).is_none());
    root.link("original", "alias").unwrap();
    assert!(root.link("original", "alias").is_none());
    let alias = root.find("alias").unwrap();
    alias.open_ref(); // A distinct Inode wrapper, not an Arc clone of file.
    assert_eq!(alias.stat(), (id, false, 2));
    alias.write_at(123, b"changed");
    let mut buf = [0; 7];
    file.read_at(123, &mut buf);
    assert_eq!(&buf, b"changed");
    root.unlink("original").unwrap();
    assert!(root.find("original").is_none());
    assert_eq!(alias.stat().2, 1);
    root.unlink("alias").unwrap();
    assert_eq!(file.stat().2, 0);
    assert!(root.ls().is_empty());
    assert!(root.unlink("alias").is_none());
    let replacement = root.create("original").unwrap();
    assert_ne!(replacement.stat().0, id);
    file.close_ref();
    alias.read_at(123, &mut buf);
    assert_eq!(&buf, b"changed");
    alias.close_ref(); // The unlinked inode can now be reclaimed.
    let reused = root.create("reused").unwrap();
    assert_eq!(reused.stat().0, id);
    assert_eq!(reused.read_at(0, &mut buf), 0);
    root.unlink("reused").unwrap();
    root.unlink("original").unwrap();
    // Without data-block recycling this exceeds the disk capacity.
    for _ in 0..40 {
        let f = root.create("cycle").unwrap();
        f.write_at(0, &data);
        root.unlink("cycle").unwrap();
    }
    let saved = root.create("saved").unwrap();
    saved.write_at(0, b"persistent");
    root.link("saved", "saved_alias").unwrap();
    let reopened = EasyFileSystem::open(device);
    let root2 = EasyFileSystem::root_inode(&reopened);
    assert_eq!(root2.find("saved_alias").unwrap().stat().2, 2);
    assert_eq!(root2.ls().len(), 2);
}
