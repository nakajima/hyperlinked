use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use crate::dev_reload_marker::{self, RestartPhase};

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

pub async fn run_dev(
    host: String,
    port: String,
    mdns_options: crate::server::MdnsOptions,
) -> Result<(), String> {
    println!("starting dev watcher");
    let restart_marker_path = dev_reload_marker::default_marker_path();
    clear_restart_marker(&restart_marker_path);

    if let Err(err) = ensure_server_binary_exists() {
        mark_restart_failed(&restart_marker_path, RestartPhase::Startup, &err);
        return Err(err);
    }

    let mut child = match spawn_server_process(&host, &port, &restart_marker_path, &mdns_options) {
        Ok(child) => child,
        Err(err) => {
            mark_restart_failed(&restart_marker_path, RestartPhase::Startup, &err);
            return Err(err);
        }
    };
    clear_restart_marker(&restart_marker_path);
    println!("server started (pid {})", child.id());

    let mut tailwind_child = match spawn_tailwind_watch_process() {
        Ok(child) => child,
        Err(err) => {
            let _ = stop_server_process(&mut child);
            return Err(err);
        }
    };
    println!("tailwind watcher started (pid {})", tailwind_child.id());

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
                    stop_dev_processes(&mut child, &mut tailwind_child)?;
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
                            mark_restart_pending(&restart_marker_path, RestartPhase::Rebuild);
                            match build_server_binary() {
                                Ok(()) => {
                                    if let Err(err) = stop_server_process(&mut child) {
                                        mark_restart_failed(
                                            &restart_marker_path,
                                            RestartPhase::Restart,
                                            &err,
                                        );
                                        return Err(err);
                                    }
                                    child =
                                        match spawn_server_process(
                                            &host,
                                            &port,
                                            &restart_marker_path,
                                            &mdns_options,
                                        ) {
                                            Ok(child) => child,
                                            Err(err) => {
                                                mark_restart_failed(
                                                    &restart_marker_path,
                                                    RestartPhase::Restart,
                                                    &err,
                                                );
                                                return Err(err);
                                            }
                                        };
                                    clear_restart_marker(&restart_marker_path);
                                    println!("server restarted (pid {})", child.id());
                                }
                                Err(err) => {
                                    mark_restart_failed(
                                        &restart_marker_path,
                                        RestartPhase::Rebuild,
                                        &err,
                                    );
                                    eprintln!("{err}");
                                    eprintln!("build failed; keeping the current server process");
                                }
                            }
                        }
                        PendingChangeAction::RestartOnly => {
                            println!("change detected ({summary}), restarting");
                            mark_restart_pending(&restart_marker_path, RestartPhase::Restart);
                            if let Err(err) = stop_server_process(&mut child) {
                                mark_restart_failed(&restart_marker_path, RestartPhase::Restart, &err);
                                return Err(err);
                            }
                            child = match spawn_server_process(
                                &host,
                                &port,
                                &restart_marker_path,
                                &mdns_options,
                            ) {
                                Ok(child) => child,
                                Err(err) => {
                                    mark_restart_failed(
                                        &restart_marker_path,
                                        RestartPhase::Restart,
                                        &err,
                                    );
                                    return Err(err);
                                }
                            };
                            clear_restart_marker(&restart_marker_path);
                            println!("server restarted (pid {})", child.id());
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("received Ctrl+C, stopping dev server");
                    stop_dev_processes(&mut child, &mut tailwind_child)?;
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

fn spawn_server_process(
    host: &str,
    port: &str,
    marker_path: &Path,
    mdns_options: &crate::server::MdnsOptions,
) -> Result<Child, String> {
    let executable = server_executable_path();
    let mdns_enabled = if mdns_options.enabled {
        "true"
    } else {
        "false"
    };
    Command::new(&executable)
        .arg("serve")
        .arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port)
        .arg("--mdns-enabled")
        .arg(mdns_enabled)
        .arg("--mdns-service-name")
        .arg(mdns_options.service_name.as_str())
        .arg("--mdns-service-type")
        .arg(mdns_options.service_type.as_str())
        .env(dev_reload_marker::DEV_MODE_ENV, "1")
        .env(dev_reload_marker::RESTART_MARKER_ENV, marker_path)
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

fn spawn_tailwind_watch_process() -> Result<Child, String> {
    let script = manifest_dir().join("scripts").join("tailwind-watch.sh");
    if !script.exists() {
        return Err(format!(
            "failed to start tailwind watcher: {} does not exist",
            script.display()
        ));
    }

    Command::new(&script)
        .current_dir(manifest_dir())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| {
            format!(
                "failed to start tailwind watcher {}: {err}",
                script.display()
            )
        })
}

fn stop_server_process(child: &mut Child) -> Result<(), String> {
    stop_child_process(child, "server process")
}

fn stop_tailwind_watch_process(child: &mut Child) -> Result<(), String> {
    stop_child_process(child, "tailwind watcher process")
}

fn stop_dev_processes(server_child: &mut Child, tailwind_child: &mut Child) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(err) = stop_tailwind_watch_process(tailwind_child) {
        errors.push(err);
    }
    if let Err(err) = stop_server_process(server_child) {
        errors.push(err);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn stop_child_process(child: &mut Child, label: &str) -> Result<(), String> {
    if let Some(_status) = child
        .try_wait()
        .map_err(|err| format!("failed to inspect {label} state: {err}"))?
    {
        return Ok(());
    }

    child
        .kill()
        .map_err(|err| format!("failed to stop {label}: {err}"))?;
    child
        .wait()
        .map_err(|err| format!("failed while waiting for {label} shutdown: {err}"))?;
    Ok(())
}

fn clear_restart_marker(path: &Path) {
    if let Err(err) = dev_reload_marker::clear_marker(path) {
        eprintln!("{err}");
    }
}

fn mark_restart_pending(path: &Path, phase: RestartPhase) {
    if let Err(err) = dev_reload_marker::write_pending(path, phase) {
        eprintln!("{err}");
    }
}

fn mark_restart_failed(path: &Path, phase: RestartPhase, error: &str) {
    if let Err(marker_err) = dev_reload_marker::write_failed(path, phase, error) {
        eprintln!("{marker_err}");
    }
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
