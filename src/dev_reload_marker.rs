use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const DEV_MODE_ENV: &str = "HYPERLINKED_DEV_MODE";
pub(crate) const RESTART_MARKER_ENV: &str = "HYPERLINKED_DEV_RESTART_MARKER";
const PENDING_STATUS: &str = "pending";
const FAILED_STATUS: &str = "failed";
const MAX_ERROR_LEN: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestartPhase {
    Startup,
    Rebuild,
    Restart,
}

impl RestartPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Rebuild => "rebuild",
            Self::Restart => "restart",
        }
    }
}

#[derive(Default)]
struct MarkerRecord {
    status: Option<String>,
    phase: Option<String>,
    error: Option<String>,
}

pub(crate) fn default_marker_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("dev-hot")
        .join("restart-status")
}

pub(crate) fn clear_marker(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("failed to remove marker {}: {err}", path.display())),
    }
}

pub(crate) fn write_pending(path: &Path, phase: RestartPhase) -> Result<(), String> {
    write_marker(path, PENDING_STATUS, phase, None)
}

pub(crate) fn write_failed(path: &Path, phase: RestartPhase, error: &str) -> Result<(), String> {
    write_marker(path, FAILED_STATUS, phase, Some(error))
}

fn write_marker(
    path: &Path,
    status: &str,
    phase: RestartPhase,
    error: Option<&str>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create marker parent directory {}: {err}",
                parent.display()
            )
        })?;
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());

    let mut body = format!(
        "status={status}\nphase={}\nat={timestamp}\n",
        phase.as_str()
    );
    if let Some(error) = error {
        body.push_str("error=");
        body.push_str(&sanitize_error(error));
        body.push('\n');
    }

    fs::write(path, body).map_err(|err| format!("failed to write marker {}: {err}", path.display()))
}

pub(crate) fn read_failed_message() -> Option<String> {
    if std::env::var(DEV_MODE_ENV).ok().as_deref() != Some("1") {
        return None;
    }

    let path = marker_path_from_env().unwrap_or_else(default_marker_path);
    match read_failed_message_from_path(&path) {
        Ok(message) => message,
        Err(_err) => Some(
            "Hot reload restart status is unreadable. Check the dev watcher output.".to_string(),
        ),
    }
}

fn marker_path_from_env() -> Option<PathBuf> {
    let raw = std::env::var(RESTART_MARKER_ENV).ok()?;
    if raw.trim().is_empty() {
        return None;
    }

    Some(PathBuf::from(raw))
}

fn read_failed_message_from_path(path: &Path) -> Result<Option<String>, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!("failed to read marker {}: {err}", path.display()));
        }
    };

    let marker = parse_marker(&raw);
    let status = marker
        .status
        .as_deref()
        .ok_or_else(|| format!("marker {} is missing status field", path.display()))?;

    if status == PENDING_STATUS {
        return Ok(None);
    }

    if status != FAILED_STATUS {
        return Err(format!(
            "marker {} has unknown status {status}",
            path.display()
        ));
    }

    let phase = marker.phase.unwrap_or_else(|| "restart".to_string());
    let error = marker.error.unwrap_or_else(|| "unknown error".to_string());

    Ok(Some(format!(
        "Hot reload {phase} failed: {error}. Check `hyperlinked dev` logs."
    )))
}

fn parse_marker(raw: &str) -> MarkerRecord {
    let mut marker = MarkerRecord::default();

    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }

        match key {
            "status" => marker.status = Some(value),
            "phase" => marker.phase = Some(value),
            "error" => marker.error = Some(value),
            _ => {}
        }
    }

    marker
}

fn sanitize_error(error: &str) -> String {
    let mut compact = error.replace(['\n', '\r'], " ");
    compact.truncate(MAX_ERROR_LEN);
    compact.trim().to_string()
}

#[cfg(test)]
mod tests {
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
}
