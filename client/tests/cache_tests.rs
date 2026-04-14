#[allow(dead_code)]
mod apis {
    include!("../src/apis.rs");
}

#[allow(dead_code)]
mod cache {
    include!("../src/cache.rs");

    #[cfg(test)]
    mod tests {
        use super::*;
        use httpmock::Method::GET;
        use httpmock::MockServer;
        use serde_json::json;
        use tokio::runtime::Runtime;

        fn make_client(base_url: String) -> (Runtime, ApiClient) {
            let rt = Runtime::new().expect("failed to create tokio runtime");
            let client = ApiClient::new(base_url, rt.handle().clone()).expect("failed to create api client");
            (rt, client)
        }

        fn file_entry(name: &str) -> FileEntry {
            FileEntry {
                name: name.to_string(),
                is_dir: false,
                size: 10,
                mtime: 1.0,
                ctime: 1.0,
                mode: 0o644,
            }
        }

        #[test]
        fn cached_entry_expiration_logic_works() {
            let mut entry = CachedEntry::new(42usize, Duration::from_secs(2));
            assert!(!entry.is_expired());

            entry.created_at = Instant::now() - Duration::from_secs(3);
            assert!(entry.is_expired());
        }

        #[test]
        fn store_and_get_directory_listing_roundtrip() {
            let mut manager = CacheManager::new();
            manager.store_directory_listing("/docs", vec![file_entry("a.txt")]);

            let cached = manager
                .get_cached_directory("/docs")
                .expect("directory should be in cache");
            assert_eq!(cached.len(), 1);
            assert_eq!(cached[0].name, "a.txt");
        }

        #[test]
        fn invalidate_directory_cache_removes_parent_directory_too() {
            let mut manager = CacheManager::new();
            manager.store_directory_listing("/root", vec![file_entry("root.txt")]);
            manager.store_directory_listing("/root/child", vec![file_entry("child.txt")]);

            manager.invalidate_directory_cache("/root/child");

            assert!(manager.get_cached_directory("/root/child").is_none());
            assert!(manager.get_cached_directory("/root").is_none());
        }

        #[test]
        fn invalidate_directory_cache_handles_root_parent() {
            let mut manager = CacheManager::new();
            manager.store_directory_listing("/", vec![file_entry("top")]);

            manager.invalidate_directory_cache("/file.txt");
            assert!(manager.get_cached_directory("/").is_none());
        }

        #[test]
        fn read_from_cache_returns_requested_slice() {
            let mut manager = CacheManager::new();
            manager.store_file_chunk("/blob", 0, (0u8..=31u8).collect());

            let data = manager
                .read_from_cache("/blob", 4, 5)
                .expect("chunk should be cached");
            assert_eq!(data, vec![4, 5, 6, 7, 8]);
        }

        #[test]
        fn read_from_cache_removes_expired_chunk_and_updates_size() {
            let mut manager = CacheManager::new();
            let path = "/expired".to_string();

            manager.file_cache.insert(
                path.clone(),
                FileCache {
                    chunks: HashMap::from([(
                        0,
                        CachedChunk {
                            data: vec![1, 2, 3, 4],
                            last_access: SystemTime::now(),
                            created_at: Instant::now() - DATA_CACHE_TTL - Duration::from_millis(1),
                        },
                    )]),
                    total_size: 4,
                },
            );

            let miss = manager.read_from_cache(&path, 0, 4);
            assert!(miss.is_none());

            let file_cache = manager
                .file_cache
                .get(&path)
                .expect("file cache entry should still exist");
            assert!(file_cache.chunks.is_empty());
            assert_eq!(file_cache.total_size, 0);
        }

        #[test]
        fn store_chunk_evicts_old_data_when_exceeding_max_size() {
            let mut manager = CacheManager::new();
            let path = "/big";

            manager.store_file_chunk(path, 0, vec![1u8; MAX_CACHE_SIZE]);
            manager.store_file_chunk(path, CHUNK_SIZE as u64, vec![2u8; 1]);

            let file_cache = manager.file_cache.get(path).expect("file cache should exist");
            assert!(!file_cache.chunks.contains_key(&0));
            assert!(file_cache.chunks.contains_key(&(CHUNK_SIZE as u64)));
            assert_eq!(file_cache.total_size, 1);
        }

        #[test]
        fn storing_same_offset_replaces_existing_chunk_size() {
            let mut manager = CacheManager::new();
            let path = "/replace";

            manager.store_file_chunk(path, 0, vec![1u8; 10]);
            manager.store_file_chunk(path, 0, vec![9u8; 4]);

            let file_cache = manager.file_cache.get(path).expect("file cache should exist");
            assert_eq!(file_cache.total_size, 4);
            assert_eq!(file_cache.chunks.len(), 1);
            assert_eq!(file_cache.chunks.get(&0).expect("chunk at 0").data, vec![9u8; 4]);
        }

        #[test]
        fn cleanup_expired_removes_stale_directories_and_files() {
            let mut manager = CacheManager::new();

            manager.directory_cache.insert(
                "/old-dir".to_string(),
                DirectoryCache {
                    entries: CachedEntry {
                        data: vec![file_entry("old")],
                        created_at: Instant::now() - DIRECTORY_CACHE_TTL - Duration::from_millis(1),
                        ttl: DIRECTORY_CACHE_TTL,
                    },
                },
            );

            manager.file_cache.insert(
                "/old-file".to_string(),
                FileCache {
                    chunks: HashMap::from([(
                        0,
                        CachedChunk {
                            data: vec![1, 2, 3],
                            last_access: SystemTime::now(),
                            created_at: Instant::now() - DATA_CACHE_TTL - Duration::from_millis(1),
                        },
                    )]),
                    total_size: 3,
                },
            );

            manager.cleanup_expired();

            assert!(manager.directory_cache.is_empty());
            assert!(manager.file_cache.is_empty());
        }

        #[test]
        fn clear_removes_everything() {
            let mut manager = CacheManager::new();
            manager.store_directory_listing("/d", vec![file_entry("x")]);
            manager.store_file_chunk("/f", 0, vec![1, 2, 3]);

            manager.clear();

            assert!(manager.directory_cache.is_empty());
            assert!(manager.file_cache.is_empty());
        }

        #[test]
        fn list_directory_cached_fetches_once_then_uses_cache() {
            let server = MockServer::start();
            let entry = file_entry("cached.txt");
            let list_mock = server.mock(|when, then| {
                when.method(GET).path("/list/data");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({"entries": [entry]}));
            });

            let (_rt, api) = make_client(server.base_url());
            let _guard = api.enter_runtime();
            let mut manager = CacheManager::new();

            let first = manager
                .list_directory_cached("/data", &api)
                .expect("first list call should hit api");
            let second = manager
                .list_directory_cached("/data", &api)
                .expect("second list call should hit cache");

            assert_eq!(first.len(), 1);
            assert_eq!(second.len(), 1);
            assert_eq!(second[0].name, "cached.txt");
            list_mock.assert_hits(1);
        }

        #[test]
        fn read_with_cache_fetches_once_then_serves_from_cache() {
            let server = MockServer::start();
            let payload: Vec<u8> = (0u8..=15u8).collect();

            let read_mock = server.mock(|when, then| {
                when.method(GET)
                    .path("/files/blob")
                    .query_param("offset", "0")
                    .query_param("size", &CHUNK_SIZE.to_string());
                then.status(200).body(payload.clone());
            });

            let (_rt, api) = make_client(server.base_url());
            let _guard = api.enter_runtime();
            let mut manager = CacheManager::new();

            let first = manager
                .read_with_cache("/blob", 2, 4, &api)
                .expect("first read should hit api");
            let second = manager
                .read_with_cache("/blob", 3, 3, &api)
                .expect("second read should hit cache");

            assert_eq!(first, vec![2, 3, 4, 5]);
            assert_eq!(second, vec![3, 4, 5]);
            read_mock.assert_hits(1);
        }

        #[test]
        fn read_with_cache_spanning_chunks_stitches_full_response() {
            let server = MockServer::start();
            let first_chunk: Vec<u8> = (0..CHUNK_SIZE as usize)
                .map(|i| (i % 251) as u8)
                .collect();
            let second_chunk: Vec<u8> = (0..CHUNK_SIZE as usize)
                .map(|i| ((i + 17) % 251) as u8)
                .collect();

            let first_mock = server.mock(|when, then| {
                when.method(GET)
                    .path("/files/blob")
                    .query_param("offset", "0")
                    .query_param("size", &CHUNK_SIZE.to_string());
                then.status(200).body(first_chunk.clone());
            });

            let second_mock = server.mock(|when, then| {
                when.method(GET)
                    .path("/files/blob")
                    .query_param("offset", &CHUNK_SIZE.to_string())
                    .query_param("size", &CHUNK_SIZE.to_string());
                then.status(200).body(second_chunk.clone());
            });

            let (_rt, api) = make_client(server.base_url());
            let _guard = api.enter_runtime();
            let mut manager = CacheManager::new();

            let warmup = manager
                .read_with_cache("/blob", 0, CHUNK_SIZE, &api)
                .expect("initial read should fetch first chunk");
            assert_eq!(warmup.len(), CHUNK_SIZE as usize);

            let overlap = 4096usize;
            let start = CHUNK_SIZE as u64 - overlap as u64;
            let requested = (overlap * 2) as u32;

            let stitched = manager
                .read_with_cache("/blob", start, requested, &api)
                .expect("spanning read should stitch data across chunks");

            let mut expected = Vec::with_capacity(requested as usize);
            expected.extend_from_slice(&first_chunk[first_chunk.len() - overlap..]);
            expected.extend_from_slice(&second_chunk[..overlap]);

            assert_eq!(stitched.len(), requested as usize);
            assert_eq!(stitched, expected);
            first_mock.assert_hits(1);
            second_mock.assert_hits(1);
        }

        #[test]
        fn invalidate_all_for_path_clears_file_and_parent_directory_cache() {
            let mut manager = CacheManager::new();
            manager.store_file_chunk("/root/file.txt", 0, vec![1, 2]);
            manager.store_directory_listing("/root", vec![file_entry("file.txt")]);

            manager.invalidate_all_for_path("/root/file.txt");

            assert!(!manager.file_cache.contains_key("/root/file.txt"));
            assert!(!manager.directory_cache.contains_key("/root"));
        }
    }
}
