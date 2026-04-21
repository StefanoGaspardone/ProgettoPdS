#![cfg(target_os = "windows")]

#[allow(dead_code)]
mod apis {
    include!("../src/apis.rs");
}

#[allow(dead_code)]
mod cache {
    include!("../src/cache.rs");
}

#[allow(dead_code)]
mod dokany {
    include!("../src/dokany.rs");

    #[cfg(test)]
    mod tests {
        use super::*;
        use httpmock::Method::GET;
        use httpmock::MockServer;
        use serde_json::json;
        use tokio::runtime::Runtime;

        fn make_fs(server: &MockServer) -> (Runtime, DokanyFs) {
            let rt = Runtime::new().expect("failed to create tokio runtime");
            let api = ApiClient::new(server.base_url(), rt.handle().clone())
                .expect("failed to create api client");

            let fs = DokanyFs {
                api_client: Arc::new(api),
                ino_counter: Mutex::new(2),
                cache: Mutex::new(CacheManager::new()),
                last_cleanup: Mutex::new(Instant::now()),
                pending_writes: Mutex::new(HashMap::new()),
            };

            (rt, fs)
        }

        fn test_entry(name: &str, is_dir: bool) -> FileEntry {
            FileEntry {
                name: name.to_string(),
                is_dir,
                size: 12,
                mtime: 10.0,
                ctime: 9.0,
                mode: if is_dir { 0o755 } else { 0o644 },
            }
        }

        #[test]
        fn posix_to_ntstatus_maps_common_errors() {
            assert_eq!(posix_to_ntstatus(0), STATUS_SUCCESS);
            assert_eq!(posix_to_ntstatus(ENOENT), STATUS_OBJECT_NAME_NOT_FOUND);
            assert_eq!(posix_to_ntstatus(ENOTCONN), STATUS_DEVICE_NOT_CONNECTED);
            assert_eq!(posix_to_ntstatus(EIO), STATUS_IO_DEVICE_ERROR);
            assert_eq!(posix_to_ntstatus(libc::EINVAL), STATUS_ACCESS_DENIED);
        }

        #[test]
        fn normalize_remote_path_handles_backslashes_and_leading_slashes() {
            assert_eq!(normalize_remote_path(r"\\foo\\bar"), "foo//bar");
            assert_eq!(normalize_remote_path(r"/foo/bar"), "foo/bar");
            assert_eq!(normalize_remote_path(r"foo\bar"), "foo/bar");
        }

        #[test]
        fn split_parent_and_name_handles_root_and_nested_paths() {
            assert_eq!(split_parent_and_name(""), ("/".to_string(), "".to_string()));
            assert_eq!(
                split_parent_and_name("one"),
                ("/".to_string(), "one".to_string())
            );
            assert_eq!(
                split_parent_and_name("one/two"),
                ("/one".to_string(), "two".to_string())
            );
            assert_eq!(
                split_parent_and_name("/one/two/"),
                ("/one".to_string(), "two".to_string())
            );
        }

        #[test]
        fn next_ino_increments_monotonically() {
            let server = MockServer::start();
            let (_rt, fs) = make_fs(&server);

            assert_eq!(fs.next_ino(), 2);
            assert_eq!(fs.next_ino(), 3);
            assert_eq!(fs.next_ino(), 4);
        }

        #[test]
        fn lookup_entry_returns_virtual_root_for_empty_path() {
            let server = MockServer::start();
            let (_rt, fs) = make_fs(&server);

            let (ino, entry) = fs.lookup_entry("").expect("root lookup should succeed");
            assert_eq!(ino, 1);
            assert!(entry.is_dir);
            assert_eq!(entry.name, "/");
        }

        #[test]
        fn lookup_entry_fetches_and_then_reuses_directory_cache() {
            let server = MockServer::start();
            let list_mock = server.mock(|when, then| {
                when.method(GET).path("/list/dir");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({"entries": [test_entry("file.txt", false)]}));
            });

            let (_rt, fs) = make_fs(&server);
            let _guard = fs.api_client.enter_runtime();

            let (_, first) = fs
                .lookup_entry("/dir/file.txt")
                .expect("first lookup should hit api");
            let (_, second) = fs
                .lookup_entry("/dir/file.txt")
                .expect("second lookup should hit cache");

            assert_eq!(first.name, "file.txt");
            assert_eq!(second.name, "file.txt");
            list_mock.assert_hits(1);
        }

        #[test]
        fn lookup_entry_fresh_bypasses_cache() {
            let server = MockServer::start();
            let list_mock = server.mock(|when, then| {
                when.method(GET).path("/list/dir");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({"entries": [test_entry("x.txt", false)]}));
            });

            let (_rt, fs) = make_fs(&server);
            let _guard = fs.api_client.enter_runtime();

            let _ = fs.lookup_entry("/dir/x.txt").expect("lookup should succeed");
            let _ = fs
                .lookup_entry_fresh("/dir/x.txt")
                .expect("fresh lookup should also succeed");

            list_mock.assert_hits(2);
        }

        #[test]
        fn lookup_entry_returns_enoent_when_name_missing() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/list/dir");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({"entries": [test_entry("other.txt", false)]}));
            });

            let (_rt, fs) = make_fs(&server);
            let _guard = fs.api_client.enter_runtime();

            let err = fs.lookup_entry("/dir/missing.txt").unwrap_err();
            assert_eq!(err, ENOENT);
        }

        #[test]
        fn invalidate_path_clears_related_file_and_directory_cache() {
            let server = MockServer::start();
            let (_rt, fs) = make_fs(&server);

            {
                let mut cache = fs.cache.lock().expect("cache lock");
                cache.store_file_chunk("dir/file.txt", 0, vec![1, 2, 3]);
                cache.store_directory_listing("/dir", vec![test_entry("file.txt", false)]);
            }

            fs.invalidate_path("dir/file.txt");

            let mut cache = fs.cache.lock().expect("cache lock");
            assert!(cache.read_from_cache("dir/file.txt", 0, 3).is_none());
            assert!(cache.get_cached_directory("/dir").is_none());
        }

        #[test]
        fn maybe_cleanup_cache_updates_timestamp_when_due() {
            let server = MockServer::start();
            let (_rt, fs) = make_fs(&server);

            {
                let mut last_cleanup = fs.last_cleanup.lock().expect("last_cleanup lock");
                *last_cleanup = Instant::now() - METADATA_CACHE_TTL - Duration::from_millis(1);
            }

            fs.maybe_cleanup_cache();

            let elapsed = fs
                .last_cleanup
                .lock()
                .expect("last_cleanup lock")
                .elapsed();
            assert!(elapsed < Duration::from_secs(1));
        }

        #[test]
        fn test_dokany_async_write_and_flush_performance() {
            let server = MockServer::start();
            let write_mock = server.mock(|when, then| {
                when.method(httpmock::Method::PATCH).path("/files/dokany_speed.bin");
                // Simulate a 100ms network delay per chunk
                then.delay(Duration::from_millis(100)).status(200);
            });

            let (_rt, fs) = make_fs(&server);
            let _guard = fs.api_client.enter_runtime();

            let path = "dokany_speed.bin".to_string();
            
            let start_queue = Instant::now();
            
            // Simulate 10 concurrent chunks being written
            for i in 0..10 {
                let api_clone = fs.api_client.clone();
                let path_clone = path.clone();
                let offset = i * 1024;
                
                let handle = fs.api_client.spawn_task_with_handle(async move {
                    api_clone.write_file_chunk_async(&path_clone, offset, vec![0; 1024]).await
                });
                
                fs.pending_writes.lock().unwrap().entry(path.clone()).or_default().handles.push(handle);
            }
            
            let queue_duration = start_queue.elapsed();
            let start_flush = Instant::now();
            
            // Wait for internal cleanup/flush buffers
            let handles = fs.pending_writes.lock().unwrap().remove(&path).map(|s| s.handles).unwrap_or_default();
            let mut success = true;
            fs.api_client.block_on(async {
                for handle in handles {
                    if let Ok(Err(_)) | Err(_) = handle.await {
                        success = false;
                    }
                }
            });
            
            let flush_duration = start_flush.elapsed();
            
            assert!(success, "All writes should succeed");
            write_mock.assert_hits(10);
            
            // Similar to FUSE, verify it operates asynchronously
            assert!(queue_duration < Duration::from_millis(200), "Queuing took too long: {:?}", queue_duration);
            assert!(flush_duration >= Duration::from_millis(100), "Flush too fast: {:?}", flush_duration);
            assert!(flush_duration < Duration::from_millis(800), "Flush too slow (not concurrent): {:?}", flush_duration);
        }

        #[test]
        fn test_dokany_async_write_failure_propagation() {
            let server = MockServer::start();
            let write_mock = server.mock(|when, then| {
                when.method(httpmock::Method::PATCH).path("/files/fail.bin");
                then.status(400); // Fails immediately, no retry loop
            });

            let (_rt, fs) = make_fs(&server);
            let _guard = fs.api_client.enter_runtime();

            let path = "fail.bin".to_string();
            
            let api_clone = fs.api_client.clone();
            let path_clone = path.clone();
            let handle = fs.api_client.spawn_task_with_handle(async move {
                api_clone.write_file_chunk_async(&path_clone, 0, vec![1, 2, 3]).await
            });
            fs.pending_writes.lock().unwrap().entry(path.clone()).or_default().handles.push(handle);
            
            let handles = fs.pending_writes.lock().unwrap().remove(&path).map(|s| s.handles).unwrap_or_default();
            let mut success = true;
            fs.api_client.block_on(async {
                for handle in handles {
                    if let Ok(Err(_)) | Err(_) = handle.await {
                        success = false;
                    }
                }
            });
            
            assert!(!success, "Write should fail and propagate I/O rejection to flush");
            write_mock.assert_hits(1);
        }
    }
}
