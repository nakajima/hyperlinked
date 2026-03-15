use super::resolve_command_path;

#[test]
fn resolve_command_path_returns_none_for_blank_value() {
    assert!(resolve_command_path("   ").is_none());
}

#[cfg(unix)]
#[test]
fn resolve_command_path_accepts_executable_absolute_path() {
    use std::{fs, os::unix::fs::PermissionsExt, time::SystemTime};

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock should be monotonic")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("chromium-check-{unique}"));
    fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("temp executable should write");

    let mut permissions = fs::metadata(&path)
        .expect("temp executable should exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("permissions should update");

    let resolved = resolve_command_path(path.to_str().expect("path should be utf-8"));
    assert_eq!(resolved.as_deref(), Some(path.as_path()));

    fs::remove_file(path).expect("temp executable should clean up");
}
