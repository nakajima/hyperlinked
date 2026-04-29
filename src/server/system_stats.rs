use std::process::Command;

#[derive(Clone, Debug)]
pub(crate) struct SystemStats {
    pub(crate) pid: u32,
    pub(crate) cpu_percent: Option<f64>,
    pub(crate) resident_memory_bytes: Option<u64>,
    pub(crate) error: Option<String>,
}

impl SystemStats {
    fn unavailable(pid: u32, error: impl Into<String>) -> Self {
        Self {
            pid,
            cpu_percent: None,
            resident_memory_bytes: None,
            error: Some(error.into()),
        }
    }
}

pub(crate) fn current() -> SystemStats {
    let pid = std::process::id();
    match read_ps_process_stats(pid) {
        Ok(stats) => stats,
        Err(error) => SystemStats::unavailable(pid, error),
    }
}

fn read_ps_process_stats(pid: u32) -> Result<SystemStats, String> {
    let pid_arg = pid.to_string();
    let output = Command::new("ps")
        .env("LC_ALL", "C")
        .args([
            "-p",
            pid_arg.as_str(),
            "-o",
            "pid=",
            "-o",
            "pcpu=",
            "-o",
            "rss=",
        ])
        .output()
        .map_err(|err| format!("failed to run `ps`: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!(
                "`ps` exited with non-zero status {}",
                output.status
            ));
        }
        return Err(format!(
            "`ps` exited with non-zero status {}: {stderr}",
            output.status
        ));
    }

    parse_ps_process_stats(pid, String::from_utf8_lossy(&output.stdout).as_ref())
}

fn parse_ps_process_stats(expected_pid: u32, output: &str) -> Result<SystemStats, String> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "`ps` output was empty".to_string())?;
    let columns = line.split_whitespace().collect::<Vec<_>>();
    if columns.len() < 3 {
        return Err(format!(
            "`ps` output had {} column(s), expected at least 3",
            columns.len()
        ));
    }

    let pid = columns[0]
        .parse::<u32>()
        .map_err(|err| format!("failed to parse `ps` pid `{}`: {err}", columns[0]))?;
    if pid != expected_pid {
        return Err(format!(
            "`ps` returned stats for pid {pid}, expected {expected_pid}"
        ));
    }

    let cpu_percent = columns[1]
        .parse::<f64>()
        .map_err(|err| format!("failed to parse `ps` cpu `{}`: {err}", columns[1]))?;
    if !cpu_percent.is_finite() {
        return Err(format!("`ps` cpu value was not finite: {}", columns[1]));
    }

    let resident_memory_kib = columns[2]
        .parse::<u64>()
        .map_err(|err| format!("failed to parse `ps` rss `{}`: {err}", columns[2]))?;

    Ok(SystemStats {
        pid,
        cpu_percent: Some(cpu_percent.max(0.0)),
        resident_memory_bytes: Some(resident_memory_kib.saturating_mul(1024)),
        error: None,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/server_system_stats.rs"]
mod tests;
