use chrono::Duration as ChronoDuration;

pub(crate) const ADMIN_TAB_OVERVIEW_PATH: &str = "/admin/overview";
pub(crate) const ADMIN_TAB_ARTIFACTS_PATH: &str = "/admin/artifacts";
pub(crate) const ADMIN_TAB_LLM_INTERACTIONS_PATH: &str = "/admin/llm-interactions";
pub(crate) const ADMIN_TAB_QUEUE_PATH: &str = "/admin/queue";
pub(crate) const ADMIN_TAB_FEEDS_PATH: &str = "/admin/feeds";
pub(crate) const ADMIN_TAB_IMPORT_EXPORT_PATH: &str = "/admin/import-export";
pub(crate) const ADMIN_TAB_STORAGE_PATH: &str = "/admin/storage";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminTab {
    Overview,
    Artifacts,
    LlmInteractions,
    Queue,
    ImportExport,
    Storage,
}

impl AdminTab {
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Overview => ADMIN_TAB_OVERVIEW_PATH,
            Self::Artifacts => ADMIN_TAB_ARTIFACTS_PATH,
            Self::LlmInteractions => ADMIN_TAB_LLM_INTERACTIONS_PATH,
            Self::Queue => ADMIN_TAB_QUEUE_PATH,
            Self::ImportExport => ADMIN_TAB_IMPORT_EXPORT_PATH,
            Self::Storage => ADMIN_TAB_STORAGE_PATH,
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Artifacts => "Artifacts",
            Self::LlmInteractions => "LLM",
            Self::Queue => "Queue",
            Self::ImportExport => "Import / Export",
            Self::Storage => "Storage",
        }
    }

    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Overview => "Status and diagnostics for this server.",
            Self::Artifacts => "Configure collection pipelines and run missing-artifact backfills.",
            Self::LlmInteractions => {
                "Configure LLM connectivity and inspect raw request and response payloads."
            }
            Self::Queue => "Queue lifecycle controls for worker processing.",
            Self::ImportExport => "Create backup archives and restore from ZIP exports.",
            Self::Storage => "Disk and artifact storage utilization by dataset and kind.",
        }
    }
}

pub(crate) fn llm_interactions_href(page: u64) -> String {
    format!("{ADMIN_TAB_LLM_INTERACTIONS_PATH}?page={}", page.max(1))
}

pub(crate) fn format_relative_duration(delta: ChronoDuration) -> String {
    let future = delta < ChronoDuration::zero();
    let seconds = delta.num_seconds().unsigned_abs();

    let label = if seconds < 10 {
        "just now".to_string()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / (60 * 60))
    };

    if label == "just now" {
        if future {
            "in a few seconds".to_string()
        } else {
            label
        }
    } else if future {
        format!("in {label}")
    } else {
        format!("{label} ago")
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    format_bytes_f64(bytes as f64)
}

pub(crate) fn format_bytes_f64(bytes_f64: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    if bytes_f64 < KB {
        return format!("{}B", bytes_f64 as u64);
    }
    if bytes_f64 < MB {
        return format!("{:.1}KB", bytes_f64 / KB);
    }
    if bytes_f64 < GB {
        return format!("{:.1}MB", bytes_f64 / MB);
    }
    if bytes_f64 < TB {
        return format!("{:.1}GB", bytes_f64 / GB);
    }
    format!("{:.1}TB", bytes_f64 / TB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_duration_formats_past_and_future_values() {
        assert_eq!(
            format_relative_duration(ChronoDuration::seconds(5)),
            "just now"
        );
        assert_eq!(
            format_relative_duration(ChronoDuration::minutes(2)),
            "2m ago"
        );
        assert_eq!(
            format_relative_duration(-ChronoDuration::minutes(3)),
            "in 3m"
        );
    }

    #[test]
    fn byte_formatting_and_paths_use_admin_conventions() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1536), "1.5KB");
        assert_eq!(format_bytes_f64(5.0 * 1024.0 * 1024.0), "5.0MB");
        assert_eq!(llm_interactions_href(0), "/admin/llm-interactions?page=1");
        assert_eq!(AdminTab::ImportExport.path(), ADMIN_TAB_IMPORT_EXPORT_PATH);
    }
}
