use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(350);
const DEV_PROFILE: &str = "dev-hot";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingChangeAction {
    RestartOnly,
    RebuildAndRestart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PathAction {
    Ignore,
    RestartOnly,
    RebuildAndRestart,
}

pub async fn run_dev(host: String, port: String) -> Result<(), String> {
    println!("starting dev watcher");
    ensure_server_binary_exists()?;

    let mut child = spawn_server_process(&host, &port)?;
    println!("server started (pid {})", child.id());

    let (watch_event_tx, watch_event_rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = watch_event_tx.send(event);
    })
    .map_err(|err| format!("failed to create file watcher: {err}"))?;
    configure_watches(&mut watcher)?;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();
    let bridge = std::thread::spawn(move || {
        while let Ok(event) = watch_event_rx.recv() {
            if event_tx.send(event).is_err() {
                break;
            }
        }
    });

    let mut changed_paths = BTreeSet::new();
    let mut debounce_deadline: Option<tokio::time::Instant> = None;
    let mut pending_action: Option<PendingChangeAction> = None;

    loop {
        if let Some(deadline) = debounce_deadline {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("received Ctrl+C, stopping dev server");
                    stop_server_process(&mut child)?;
                    drop(watcher);
                    let _ = bridge.join();
                    return Ok(());
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            absorb_event(
                                event,
                                &mut changed_paths,
                                &mut debounce_deadline,
                                &mut pending_action,
                            );
                        }
                        Some(Err(err)) => {
                            eprintln!("watch error: {err}");
                        }
                        None => {
                            return Err("watch channel closed unexpectedly".to_string());
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    debounce_deadline = None;
                    if changed_paths.is_empty() {
                        pending_action = None;
                        continue;
                    }

                    let summary = summarize_paths(&changed_paths);
                    changed_paths.clear();
                    let action = pending_action
                        .take()
                        .unwrap_or(PendingChangeAction::RestartOnly);

                    match action {
                        PendingChangeAction::RebuildAndRestart => {
                            println!("change detected ({summary}), rebuilding");
                            match build_server_binary() {
                                Ok(()) => {
                                    stop_server_process(&mut child)?;
                                    child = spawn_server_process(&host, &port)?;
                                    println!("server restarted (pid {})", child.id());
                                }
                                Err(err) => {
                                    eprintln!("{err}");
                                    eprintln!("build failed; keeping the current server process");
                                }
                            }
                        }
                        PendingChangeAction::RestartOnly => {
                            println!("change detected ({summary}), restarting");
                            stop_server_process(&mut child)?;
                            child = spawn_server_process(&host, &port)?;
                            println!("server restarted (pid {})", child.id());
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("received Ctrl+C, stopping dev server");
                    stop_server_process(&mut child)?;
                    drop(watcher);
                    let _ = bridge.join();
                    return Ok(());
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            absorb_event(
                                event,
                                &mut changed_paths,
                                &mut debounce_deadline,
                                &mut pending_action,
                            );
                        }
                        Some(Err(err)) => {
                            eprintln!("watch error: {err}");
                        }
                        None => {
                            return Err("watch channel closed unexpectedly".to_string());
                        }
                    }
                }
            }
        }
    }
}

fn absorb_event(
    event: Event,
    changed_paths: &mut BTreeSet<PathBuf>,
    debounce_deadline: &mut Option<tokio::time::Instant>,
    pending_action: &mut Option<PendingChangeAction>,
) {
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }

    let mut saw_path = false;
    for path in event.paths {
        match action_for_path(&path) {
            PathAction::Ignore => {}
            PathAction::RestartOnly => {
                changed_paths.insert(path);
                saw_path = true;
                if pending_action.is_none() {
                    *pending_action = Some(PendingChangeAction::RestartOnly);
                }
            }
            PathAction::RebuildAndRestart => {
                changed_paths.insert(path);
                saw_path = true;
                *pending_action = Some(PendingChangeAction::RebuildAndRestart);
            }
        }
    }

    if saw_path {
        *debounce_deadline = Some(tokio::time::Instant::now() + DEBOUNCE_WINDOW);
    }
}

fn configure_watches(watcher: &mut RecommendedWatcher) -> Result<(), String> {
    let root = manifest_dir();
    let src = root.join("src");
    if src.exists() {
        watcher
            .watch(&src, RecursiveMode::Recursive)
            .map_err(|err| format!("failed to watch {}: {err}", src.display()))?;
    }

    let templates = root.join("templates");
    if templates.exists() {
        watcher
            .watch(&templates, RecursiveMode::Recursive)
            .map_err(|err| format!("failed to watch {}: {err}", templates.display()))?;
    }

    let cargo_toml = root.join("Cargo.toml");
    if cargo_toml.exists() {
        watcher
            .watch(&cargo_toml, RecursiveMode::NonRecursive)
            .map_err(|err| format!("failed to watch {}: {err}", cargo_toml.display()))?;
    }

    let cargo_lock = root.join("Cargo.lock");
    if cargo_lock.exists() {
        watcher
            .watch(&cargo_lock, RecursiveMode::NonRecursive)
            .map_err(|err| format!("failed to watch {}: {err}", cargo_lock.display()))?;
    }

    Ok(())
}

fn ensure_server_binary_exists() -> Result<(), String> {
    let executable = server_executable_path();
    if executable.exists() {
        println!("building server binary");
    } else {
        println!("server binary is missing, building");
    }
    build_server_binary()
}

fn build_server_binary() -> Result<(), String> {
    let status = Command::new("cargo")
        .arg("build")
        .arg("--bin")
        .arg("main")
        .arg("--profile")
        .arg(DEV_PROFILE)
        .current_dir(manifest_dir())
        .status()
        .map_err(|err| format!("failed to run cargo build: {err}"))?;

    if !status.success() {
        return Err(format!("cargo build failed with status {status}"));
    }

    Ok(())
}

fn spawn_server_process(host: &str, port: &str) -> Result<Child, String> {
    let executable = server_executable_path();
    Command::new(&executable)
        .arg("serve")
        .arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port)
        .current_dir(manifest_dir())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| {
            format!(
                "failed to start server process {}: {err}",
                executable.display()
            )
        })
}

fn stop_server_process(child: &mut Child) -> Result<(), String> {
    if let Some(_status) = child
        .try_wait()
        .map_err(|err| format!("failed to inspect server process state: {err}"))?
    {
        return Ok(());
    }

    child
        .kill()
        .map_err(|err| format!("failed to stop server process: {err}"))?;
    child
        .wait()
        .map_err(|err| format!("failed while waiting for server shutdown: {err}"))?;
    Ok(())
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn server_executable_path() -> PathBuf {
    let mut executable = manifest_dir().join("target").join(DEV_PROFILE).join("main");
    if cfg!(windows) {
        executable.set_extension("exe");
    }
    executable
}

fn action_for_path(path: &Path) -> PathAction {
    if !is_path_watchable(path) {
        return PathAction::Ignore;
    }

    let relative = path
        .strip_prefix(manifest_dir())
        .ok()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf());

    if relative == Path::new("Cargo.toml") || relative == Path::new("Cargo.lock") {
        return PathAction::RebuildAndRestart;
    }

    if relative.starts_with(Path::new("src/server/assets")) {
        return PathAction::RestartOnly;
    }

    if relative.starts_with(Path::new("templates")) {
        return PathAction::RebuildAndRestart;
    }

    if relative.starts_with(Path::new("src")) {
        return PathAction::RebuildAndRestart;
    }

    PathAction::Ignore
}

fn is_path_watchable(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }

    if path.components().any(|component| {
        matches!(
            component,
            Component::Normal(name) if name == "target" || name == ".git"
        )
    }) {
        return false;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };

    if name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.ends_with(".tmp")
        || name.starts_with(".#")
    {
        return false;
    }

    true
}

fn summarize_paths(paths: &BTreeSet<PathBuf>) -> String {
    let root = manifest_dir();
    let mut rendered = Vec::new();
    for path in paths.iter().take(3) {
        let display_path = path
            .strip_prefix(&root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| path.clone());
        rendered.push(display_path.display().to_string());
    }

    if paths.len() > 3 {
        rendered.push(format!("+{} more", paths.len() - 3));
    }

    rendered.join(", ")
}

#[cfg(test)]
mod tests {
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
}
