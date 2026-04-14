#[allow(dead_code)]
mod daemon {
    include!("../src/daemon.rs");

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn unique_pid_path(tag: &str) -> PathBuf {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos();

            std::env::temp_dir().join(format!("remotefs-client-{tag}-{nanos}.pid"))
        }

        #[test]
        fn should_stop_daemon_detects_stop_flag() {
            assert!(should_stop_daemon_from_args(["client", "--stop"]));
            assert!(!should_stop_daemon_from_args(["client", "--daemon"]));
        }

        #[test]
        fn should_daemonize_requires_daemon_without_foreground() {
            assert!(should_daemonize_from_args(["client", "--daemon"]));
            assert!(!should_daemonize_from_args([
                "client",
                "--daemon",
                "--foreground",
            ]));
            assert!(!should_daemonize_from_args(["client"]));
        }

        #[test]
        fn pid_file_roundtrip_with_custom_path() {
            let path = unique_pid_path("roundtrip");
            remove_pid_file_at(&path);

            write_pid_file_to(&path, 123_456).expect("write_pid_file_to should succeed");
            let pid = read_pid_file_from(&path).expect("read_pid_file_from should succeed");
            assert_eq!(pid, 123_456);

            remove_pid_file_at(&path);
            assert!(read_pid_file_from(&path).is_err());
        }

        #[test]
        fn read_pid_file_from_reports_invalid_content() {
            let path = unique_pid_path("invalid");
            std::fs::write(&path, "not-a-pid").expect("failed to write invalid pid file");

            let err = read_pid_file_from(&path).expect_err("parsing invalid pid must fail");
            let message = format!("{err:#}");
            assert!(message.contains("Invalid pid file content"));

            remove_pid_file_at(&path);
        }

        #[test]
        fn read_pid_file_optional_from_returns_none_when_missing() {
            let path = unique_pid_path("missing");
            remove_pid_file_at(&path);

            let pid = read_pid_file_optional_from(&path).expect("optional read should not fail");
            assert!(pid.is_none());
        }

        #[test]
        fn running_daemon_pid_from_detects_current_process() {
            let path = unique_pid_path("running");
            remove_pid_file_at(&path);

            let current_pid = std::process::id();
            write_pid_file_to(&path, current_pid).expect("write_pid_file_to should succeed");

            let detected = running_daemon_pid_from(&path).expect("running pid check should succeed");
            assert_eq!(detected, Some(current_pid));

            remove_pid_file_at(&path);
        }

        #[test]
        fn running_daemon_pid_from_cleans_invalid_pid_file() {
            let path = unique_pid_path("stale-invalid");
            remove_pid_file_at(&path);
            std::fs::write(&path, "invalid-pid").expect("failed to write stale pid");

            let detected = running_daemon_pid_from(&path).expect("running pid check should succeed");
            assert!(detected.is_none());
            assert!(!path.exists());
        }

        #[test]
        fn process_running_helper_recognizes_current_pid_and_rejects_zero() {
            assert!(is_process_running(std::process::id()));
            assert!(!is_process_running(0));
        }
    }
}
