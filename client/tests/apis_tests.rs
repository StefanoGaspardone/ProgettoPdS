#[allow(dead_code)]
mod apis {
    include!("../src/apis.rs");

    #[cfg(test)]
    mod tests {
        use super::*;
        use httpmock::Method::{DELETE, GET, PATCH, POST, PUT};
        use httpmock::MockServer;
        use serde_json::json;
        use tokio::runtime::Runtime;

        fn make_client(base_url: String) -> (Runtime, ApiClient) {
            let rt = Runtime::new().expect("failed to create tokio runtime");
            let client = ApiClient::new(base_url, rt.handle().clone()).expect("failed to create api client");
            (rt, client)
        }

        fn sample_entry(name: &str) -> FileEntry {
            FileEntry {
                name: name.to_string(),
                is_dir: false,
                size: 7,
                mtime: 11.0,
                ctime: 5.0,
                mode: 0o644,
            }
        }

        #[test]
        fn validate_path_rejects_traversal() {
            let err = ApiClient::validate_path("/safe/../evil", "list_directory").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);
            assert!(err.message.contains("list_directory"));
        }

        #[test]
        fn list_directory_successfully_parses_entries() {
            let server = MockServer::start();
            let response_entry = sample_entry("hello.txt");

            let list_mock = server.mock(|when, then| {
                when.method(GET).path("/list/dir");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(json!({"entries": [response_entry]}));
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            let entries = client
                .list_directory("/dir")
                .expect("list_directory should return mocked entry");

            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "hello.txt");
            list_mock.assert_hits(1);
        }

        #[test]
        fn list_directory_returns_parse_error_for_invalid_json() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/list/dir");
                then.status(200)
                    .header("content-type", "application/json")
                    .body("not-json");
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            let err = client.list_directory("/dir").unwrap_err();
            assert_eq!(err.errno, libc::EIO);
            assert!(err.message.contains("Failed to parse response"));
        }

        #[test]
        fn read_file_chunk_zero_size_short_circuits_without_http_call() {
            let server = MockServer::start();
            let (_rt, client) = make_client(server.base_url());

            let bytes = client
                .read_file_chunk("/any", 0, 0)
                .expect("size=0 should return empty vec");

            assert!(bytes.is_empty());
        }

        #[test]
        fn read_file_chunk_truncates_server_payload_to_requested_size() {
            let server = MockServer::start();
            let read_mock = server.mock(|when, then| {
                when.method(GET)
                    .path("/files/blob")
                    .query_param("offset", "0")
                    .query_param("size", "4");
                then.status(200).body(vec![1u8, 2, 3, 4, 5, 6]);
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            let bytes = client
                .read_file_chunk("/blob", 0, 4)
                .expect("read_file_chunk should succeed");

            assert_eq!(bytes, vec![1u8, 2, 3, 4]);
            read_mock.assert_hits(1);
        }

        #[test]
        fn write_file_chunk_empty_payload_short_circuits() {
            let server = MockServer::start();
            let (_rt, client) = make_client(server.base_url());

            client
                .write_file_chunk("/blob", 10, &[])
                .expect("empty chunk write should be a no-op");
        }

        #[test]
        fn write_file_chunk_uses_patch_with_content_range() {
            let server = MockServer::start();
            let patch_mock = server.mock(|when, then| {
                when.method(PATCH)
                    .path("/files/blob")
                    .header("content-range", "bytes 5-7/*")
                    .body("xyz");
                then.status(200);
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            client
                .write_file_chunk("/blob", 5, b"xyz")
                .expect("patch write should succeed");

            patch_mock.assert_hits(1);
        }

        #[test]
        fn write_file_chunk_falls_back_to_read_modify_write_on_405() {
            let server = MockServer::start();

            let patch_mock = server.mock(|when, then| {
                when.method(PATCH).path("/files/file.txt");
                then.status(405);
            });

            let read_mock = server.mock(|when, then| {
                when.method(GET).path("/files/file.txt");
                then.status(200).body("ABCDE");
            });

            let write_mock = server.mock(|when, then| {
                when.method(PUT).path("/files/file.txt").body("ABxyz");
                then.status(200);
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            client
                .write_file_chunk("/file.txt", 2, b"xyz")
                .expect("fallback write should succeed");

            patch_mock.assert_hits(1);
            read_mock.assert_hits(1);
            write_mock.assert_hits(1);
        }

        #[test]
        fn write_file_chunk_fallback_creates_new_content_when_file_missing() {
            let server = MockServer::start();

            let patch_mock = server.mock(|when, then| {
                when.method(PATCH).path("/files/new.bin");
                then.status(405);
            });

            let read_mock = server.mock(|when, then| {
                when.method(GET).path("/files/new.bin");
                then.status(404);
            });

            let write_mock = server.mock(|when, then| {
                when.method(PUT)
                    .path("/files/new.bin")
                    .body("\0\0AB");
                then.status(200);
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            client
                .write_file_chunk("/new.bin", 2, b"AB")
                .expect("fallback should create missing content with zero-fill");

            patch_mock.assert_hits(1);
            read_mock.assert_hits(1);
            write_mock.assert_hits(1);
        }

        #[test]
        fn create_delete_rename_set_attrs_and_health_check_hit_expected_endpoints() {
            let server = MockServer::start();

            let mkdir_mock = server.mock(|when, then| {
                when.method(POST).path("/mkdir/docs");
                then.status(200);
            });

            let delete_mock = server.mock(|when, then| {
                when.method(DELETE).path("/files/docs/file.txt");
                then.status(200);
            });

            let rename_mock = server.mock(|when, then| {
                when.method(POST)
                    .path("/rename")
                    .json_body(json!({"from":"/a.txt","to":"/b.txt"}));
                then.status(200);
            });

            let attrs_mock = server.mock(|when, then| {
                when.method(PATCH)
                    .path("/attrs/file.txt")
                    .json_body(json!({"mode":420}));
                then.status(200);
            });

            let health_mock = server.mock(|when, then| {
                when.method(GET).path("/health");
                then.status(200);
            });

            let (_rt, client) = make_client(server.base_url());
            let _guard = client.enter_runtime();
            client.create_directory("/docs").expect("mkdir should succeed");
            client
                .delete("/docs/file.txt")
                .expect("delete should succeed");
            client
                .rename("/a.txt", "/b.txt")
                .expect("rename should succeed");
            client
                .set_attrs("/file.txt", Some(0o644))
                .expect("set_attrs should succeed");
            client.health_check().expect("health_check should succeed");

            mkdir_mock.assert_hits(1);
            delete_mock.assert_hits(1);
            rename_mock.assert_hits(1);
            attrs_mock.assert_hits(1);
            health_mock.assert_hits(1);
        }

        #[test]
        fn path_validation_happens_before_any_network_request() {
            let (_rt, client) = make_client("http://127.0.0.1:9".to_string());

            let err = client.list_directory("/a/../b").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.read_file("/a/../b").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.read_file_chunk("/a/../b", 0, 1).unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.write_file("/a/../b", b"x").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.write_file_chunk("/a/../b", 0, b"x").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.create_directory("/a/../b").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.delete("/a/../b").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.rename("/ok", "/a/../b").unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);

            let err = client.set_attrs("/a/../b", None).unwrap_err();
            assert_eq!(err.errno, libc::EINVAL);
        }

        #[test]
        fn api_error_status_mapping_covers_special_cases() {
            let not_found = ApiError::from_status(reqwest::StatusCode::NOT_FOUND, "read_file");
            assert_eq!(not_found.errno, libc::ENOENT);

            let delete_conflict = ApiError::from_status(reqwest::StatusCode::CONFLICT, "delete");
            assert_eq!(delete_conflict.errno, libc::ENOTEMPTY);

            let create_conflict = ApiError::from_status(reqwest::StatusCode::CONFLICT, "create_directory");
            assert_eq!(create_conflict.errno, libc::EEXIST);

            let unsupported = ApiError::from_status(reqwest::StatusCode::METHOD_NOT_ALLOWED, "write_file_chunk");
            assert_eq!(unsupported.errno, libc::ENOSYS);

            let storage_full = ApiError::from_status(reqwest::StatusCode::INSUFFICIENT_STORAGE, "write_file");
            assert_eq!(storage_full.errno, libc::ENOSPC);

            let custom = ApiError::from_status(reqwest::StatusCode::IM_A_TEAPOT, "op");
            assert_eq!(custom.errno, libc::EIO);
            assert!(custom.message.contains("418"));
        }

        #[test]
        fn api_error_display_includes_errno() {
            let err = ApiError {
                errno: libc::EIO,
                message: "broken".to_string(),
            };
            let displayed = format!("{err}");
            assert!(displayed.contains("broken"));
            assert!(displayed.contains("errno"));
        }
    }
}
