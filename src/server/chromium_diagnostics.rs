use std::{
    env,
    path::{Path, PathBuf},
};

const CHROMIUM_PATH_ENV: &str = "CHROMIUM_PATH";
const DEFAULT_CHROMIUM_PATH: &str = "chromium";

#[derive(Clone, Debug)]
pub(crate) struct ChromiumDiagnostics {
    pub chromium_path: String,
    pub chromium_resolved_path: Option<String>,
    pub chromium_found: bool,
}

impl ChromiumDiagnostics {
    pub fn has_missing_binary(&self) -> bool {
        !self.chromium_found
    }
}

pub(crate) fn current() -> ChromiumDiagnostics {
    let chromium_path = chromium_path();
    let chromium_resolved_path =
        resolve_command_path(&chromium_path).map(|path| path.to_string_lossy().to_string());

    ChromiumDiagnostics {
        chromium_path,
        chromium_found: chromium_resolved_path.is_some(),
        chromium_resolved_path,
    }
}

fn chromium_path() -> String {
    env::var(CHROMIUM_PATH_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CHROMIUM_PATH.to_string())
}

fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() || has_path_separator(trimmed) {
        return is_executable_file(candidate).then(|| candidate.to_path_buf());
    }

    let search_path = env::var_os("PATH")?;
    for dir in env::split_paths(&search_path) {
        for candidate in candidate_command_paths(&dir, trimmed) {
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

fn has_path_separator(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn candidate_command_paths(dir: &Path, command: &str) -> Vec<PathBuf> {
    vec![dir.join(command)]
}

#[cfg(windows)]
fn candidate_command_paths(dir: &Path, command: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let base = dir.join(command);
    candidates.push(base.clone());
    if base.extension().is_some() {
        return candidates;
    }

    let pathext = env::var_os("PATHEXT")
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());

    for ext in pathext.split(';') {
        let normalized = ext.trim();
        if normalized.is_empty() {
            continue;
        }

        let suffix = if normalized.starts_with('.') {
            normalized.to_string()
        } else {
            format!(".{normalized}")
        };
        candidates.push(dir.join(format!("{command}{suffix}")));
    }

    candidates
}

#[cfg(test)]
mod tests {
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
}
