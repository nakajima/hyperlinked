use super::*;

fn unique_marker_path() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();
    std::env::temp_dir().join(format!("hyperlinked-dev-restart-marker-{timestamp}.tmp"))
}

#[test]
fn pending_marker_does_not_emit_banner_message() {
    let path = unique_marker_path();
    write_pending(&path, RestartPhase::Restart).expect("pending marker should write");

    let message =
        read_failed_message_from_path(&path).expect("pending marker should parse cleanly");
    assert_eq!(message, None);

    clear_marker(&path).expect("marker cleanup should succeed");
}

#[test]
fn failed_marker_emits_detailed_banner_message() {
    let path = unique_marker_path();
    write_failed(
        &path,
        RestartPhase::Rebuild,
        "cargo build failed with status exit status: 101",
    )
    .expect("failed marker should write");

    let message = read_failed_message_from_path(&path)
        .expect("failed marker should parse")
        .expect("failed marker should produce banner text");
    assert!(message.contains("rebuild"));
    assert!(message.contains("exit status: 101"));

    clear_marker(&path).expect("marker cleanup should succeed");
}

#[test]
fn clear_marker_removes_existing_file() {
    let path = unique_marker_path();
    write_pending(&path, RestartPhase::Startup).expect("pending marker should write");

    clear_marker(&path).expect("marker cleanup should succeed");
    let message =
        read_failed_message_from_path(&path).expect("missing marker should be handled cleanly");
    assert_eq!(message, None);
}
