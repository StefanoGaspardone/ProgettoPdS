#![cfg(any(target_os = "linux", target_os = "macos"))]

#[allow(dead_code)]
mod apis {
    include!("../src/apis.rs");
}

#[allow(dead_code)]
mod cache {
    include!("../src/cache.rs");
}

#[allow(dead_code)]
mod fuser {
    include!("../src/fuser.rs");

    #[cfg(test)]
    mod tests {
        use super::*;

        fn entry(name: &str, is_dir: bool, mode: u32, size: u64) -> FileEntry {
            FileEntry {
                name: name.to_string(),
                is_dir,
                size,
                mtime: 12.0,
                ctime: 11.0,
                mode,
            }
        }

        #[test]
        fn inode_reports_expired_metadata() {
            let mut inode = InodeTable::new(1000, 1000)
                .get_cloned(1)
                .expect("root inode should exist");
            assert!(!inode.is_metadata_expired());

            inode.cached_at = Instant::now() - METADATA_CACHE_TTL - Duration::from_millis(1);
            assert!(inode.is_metadata_expired());
        }

        #[test]
        fn inode_table_initializes_root() {
            let table = InodeTable::new(1001, 1002);

            let root = table.get(1).expect("root inode should exist");
            assert_eq!(root.path, "/");
            assert_eq!(root.attr.kind, FileType::Directory);
            assert_eq!(root.attr.uid, 1001);
            assert_eq!(root.attr.gid, 1002);
        }

        #[test]
        fn get_or_create_assigns_defaults_and_updates_existing_inode() {
            let mut table = InodeTable::new(1000, 1000);
            let file = entry("a.txt", false, 0, 513);

            let ino = table.get_or_create("/a.txt", &file);
            let created = table.get_cloned(ino).expect("created inode should exist");
            assert_eq!(created.attr.kind, FileType::RegularFile);
            assert_eq!(created.attr.perm, 0o644);
            assert_eq!(created.attr.blocks, 2);

            let mut updated_file = file.clone();
            updated_file.size = 2048;
            let ino_again = table.get_or_create("/a.txt", &updated_file);
            assert_eq!(ino_again, ino);
            let updated = table.get_cloned(ino).expect("updated inode should exist");
            assert_eq!(updated.attr.size, 2048);
        }

        #[test]
        fn get_or_create_sets_directory_permissions_with_owner_bits() {
            let mut table = InodeTable::new(1000, 1000);
            let dir = entry("dir", true, 0o055, 0);

            let ino = table.get_or_create("/dir", &dir);
            let inode = table.get_cloned(ino).expect("directory inode should exist");
            assert_eq!(inode.attr.kind, FileType::Directory);
            assert_eq!(inode.attr.perm, 0o755);
            assert_eq!(inode.attr.nlink, 2);
        }

        #[test]
        fn child_path_builds_expected_paths() {
            let mut table = InodeTable::new(1000, 1000);
            let dir = entry("dir", true, 0o755, 0);
            let dir_ino = table.get_or_create("/dir", &dir);

            let root_child = table
                .child_path(1, OsStr::new("a.txt"))
                .expect("root child path should be generated");
            assert_eq!(root_child, "/a.txt");

            let nested_child = table
                .child_path(dir_ino, OsStr::new("b.txt"))
                .expect("nested child path should be generated");
            assert_eq!(nested_child, "/dir/b.txt");
        }

        #[test]
        fn remove_rename_and_invalidate_metadata_update_table_consistently() {
            let mut table = InodeTable::new(1000, 1000);
            let file = entry("file.txt", false, 0o644, 10);
            let ino = table.get_or_create("/old.txt", &file);

            table.rename("/old.txt", "/new.txt");
            assert!(table.path_to_ino.contains_key("/new.txt"));
            assert!(!table.path_to_ino.contains_key("/old.txt"));
            assert_eq!(table.get(ino).expect("inode should exist").path, "/new.txt");

            table.invalidate_metadata("/new.txt");
            assert!(table.get(ino).expect("inode should exist").is_metadata_expired());

            table.remove_by_path("/new.txt");
            assert!(!table.path_to_ino.contains_key("/new.txt"));
            assert!(table.get(ino).is_none());
        }

        #[test]
        fn file_handle_table_allocates_and_closes_handles() {
            let mut table = FileHandleTable::new();
            let h1 = table.open("/a.txt".to_string());
            let h2 = table.open("/b.txt".to_string());

            assert_eq!(h1, 1);
            assert_eq!(h2, 2);
            assert_eq!(table.handles.get(&h1).expect("h1 should exist"), "/a.txt");

            table.close(h1);
            assert!(!table.handles.contains_key(&h1));
            assert!(table.handles.contains_key(&h2));
        }

        #[test]
        fn path_lock_manager_reuses_lock_for_same_path() {
            let manager = PathLockManager::new();
            let a1 = manager.get_lock("/same");
            let a2 = manager.get_lock("/same");
            let b = manager.get_lock("/other");

            assert!(Arc::ptr_eq(&a1, &a2));
            assert!(!Arc::ptr_eq(&a1, &b));
        }
    }
}
