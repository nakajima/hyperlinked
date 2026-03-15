use super::*;

#[test]
fn restart_filter_ignores_temporary_files() {
    assert_eq!(
        action_for_path(Path::new("src/main.rs.swp")),
        PathAction::Ignore
    );
    assert_eq!(
        action_for_path(Path::new("src/main.rs~")),
        PathAction::Ignore
    );
    assert_eq!(
        action_for_path(Path::new("src/.#main.rs")),
        PathAction::Ignore
    );
}

#[test]
fn restart_filter_ignores_target_and_git_paths() {
    assert_eq!(
        action_for_path(Path::new("target/debug/main")),
        PathAction::Ignore
    );
    assert_eq!(action_for_path(Path::new(".git/index")), PathAction::Ignore);
}

#[test]
fn restart_filter_classifies_paths() {
    assert_eq!(
        action_for_path(Path::new("src/lib.rs")),
        PathAction::RebuildAndRestart
    );
    assert_eq!(
        action_for_path(Path::new("src/server/assets/app.css")),
        PathAction::RestartOnly
    );
    assert_eq!(
        action_for_path(Path::new("src/server/assets/pointer.svg")),
        PathAction::RestartOnly
    );
    assert_eq!(
        action_for_path(Path::new("Cargo.toml")),
        PathAction::RebuildAndRestart
    );
    assert_eq!(
        action_for_path(Path::new("src/data/runtime.txt")),
        PathAction::RebuildAndRestart
    );
    assert_eq!(
        action_for_path(Path::new("templates/admin/index.stpl")),
        PathAction::RebuildAndRestart
    );
}
