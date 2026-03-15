use std::{
    collections::BTreeMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use chrono::{Datelike, NaiveDate, Utc};
use serde_json::Value;

pub const MATHPIX_API_TOKEN_ENV: &str = "MATHPIX_API_TOKEN";
pub const MATHPIX_APP_ID_ENV: &str = "MATHPIX_APP_ID";

const MATHPIX_TIMEOUT_SECS_ENV: &str = "MATHPIX_TIMEOUT_SECS";
const MATHPIX_POLL_INTERVAL_MS_ENV: &str = "MATHPIX_POLL_INTERVAL_MS";
const MATHPIX_POLL_TIMEOUT_SECS_ENV: &str = "MATHPIX_POLL_TIMEOUT_SECS";

const DEFAULT_MATHPIX_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MATHPIX_POLL_INTERVAL_MS: u64 = 1_000;
const DEFAULT_MATHPIX_POLL_TIMEOUT_SECS: u64 = 90;
const MATHPIX_USAGE_BASE_URL: &str = "https://api.mathpix.com";
const MATHPIX_USAGE_CACHE_TTL: Duration = Duration::from_secs(60 * 5);
const IMAGE_TIER1_LIMIT: u64 = 1_000;
const IMAGE_TIER2_LIMIT: u64 = 5_000;
const IMAGE_TIER1_RATE: f64 = 0.002;
const IMAGE_TIER2_RATE: f64 = 0.0015;
const IMAGE_TIER3_RATE: f64 = 0.001;
const PDF_TIER1_LIMIT: u64 = 1_000;
const PDF_TIER2_LIMIT: u64 = 5_000;
const PDF_TIER1_RATE: f64 = 0.005;
const PDF_TIER2_RATE: f64 = 0.004;
const PDF_TIER3_RATE: f64 = 0.003;

static USAGE_CACHE: LazyLock<Mutex<Option<MathpixUsageCache>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathpixStatus {
    pub enabled: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MathpixUsageWindow {
    pub total_requests: u64,
    pub estimated_cost_usd: f64,
    pub breakdown: Vec<MathpixUsageBreakdown>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MathpixUsageBreakdown {
    pub usage_type: String,
    pub count: u64,
    pub cost_class: MathpixUsageCostClass,
    pub estimated_cost_usd: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MathpixUsageCostClass {
    ImageRequest,
    PdfRequest,
    #[default]
    Unknown,
}

impl MathpixUsageCostClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageRequest => "image",
            Self::PdfRequest => "pdf",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MathpixUsageSummary {
    pub month: MathpixUsageWindow,
    pub all_time: MathpixUsageWindow,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MathpixConfig {
    pub app_id: String,
    pub app_key: String,
    pub request_timeout: Duration,
    pub poll_interval: Duration,
    pub poll_timeout: Duration,
}

#[derive(Clone, Debug)]
pub enum MathpixMode {
    Enabled(MathpixConfig),
    Disabled(MathpixStatus),
}

impl MathpixMode {
    pub fn status(&self) -> MathpixStatus {
        match self {
            Self::Enabled(_) => MathpixStatus {
                enabled: true,
                reason: "enabled".to_string(),
            },
            Self::Disabled(status) => status.clone(),
        }
    }

    pub fn disabled_missing_app_id(&self) -> bool {
        matches!(
            self,
            Self::Disabled(MathpixStatus {
                enabled: false,
                reason
            }) if reason.contains(MATHPIX_APP_ID_ENV)
        )
    }
}

pub fn load_mode_from_env() -> MathpixMode {
    parse_mode(MathpixEnvValues::from_env())
}

pub fn current_status() -> MathpixStatus {
    load_mode_from_env().status()
}

pub async fn current_usage_summary() -> MathpixUsageSummary {
    let mode = load_mode_from_env();
    let MathpixMode::Enabled(config) = mode else {
        return MathpixUsageSummary {
            warning: Some(
                "Mathpix usage unavailable because PDF Mathpix parsing is disabled.".to_string(),
            ),
            ..Default::default()
        };
    };

    if let Some(summary) = usage_cache_get(config.app_id.as_str()) {
        return summary;
    }

    let summary = match fetch_usage_summary(&config).await {
        Ok(summary) => summary,
        Err(error) => MathpixUsageSummary {
            warning: Some(format!("Mathpix usage unavailable: {error}")),
            ..Default::default()
        },
    };
    usage_cache_put(config.app_id.as_str(), summary.clone());
    summary
}

#[derive(Clone, Debug, Default)]
struct MathpixEnvValues {
    api_token: Option<String>,
    app_id: Option<String>,
    timeout_secs: Option<String>,
    poll_interval_ms: Option<String>,
    poll_timeout_secs: Option<String>,
}

impl MathpixEnvValues {
    fn from_env() -> Self {
        Self {
            api_token: std::env::var(MATHPIX_API_TOKEN_ENV).ok(),
            app_id: std::env::var(MATHPIX_APP_ID_ENV).ok(),
            timeout_secs: std::env::var(MATHPIX_TIMEOUT_SECS_ENV).ok(),
            poll_interval_ms: std::env::var(MATHPIX_POLL_INTERVAL_MS_ENV).ok(),
            poll_timeout_secs: std::env::var(MATHPIX_POLL_TIMEOUT_SECS_ENV).ok(),
        }
    }
}

fn parse_mode(values: MathpixEnvValues) -> MathpixMode {
    let api_token = parse_non_empty(values.api_token);
    let app_id = parse_non_empty(values.app_id);

    if api_token.is_none() {
        return MathpixMode::Disabled(MathpixStatus {
            enabled: false,
            reason: format!("disabled: {MATHPIX_API_TOKEN_ENV} not set"),
        });
    }

    let Some(app_id) = app_id else {
        return MathpixMode::Disabled(MathpixStatus {
            enabled: false,
            reason: format!("disabled: {MATHPIX_APP_ID_ENV} not set"),
        });
    };

    let timeout_secs = parse_u64(values.timeout_secs, DEFAULT_MATHPIX_TIMEOUT_SECS, 5, 120);
    let poll_interval_ms = parse_u64(
        values.poll_interval_ms,
        DEFAULT_MATHPIX_POLL_INTERVAL_MS,
        250,
        5_000,
    );
    let poll_timeout_secs = parse_u64(
        values.poll_timeout_secs,
        DEFAULT_MATHPIX_POLL_TIMEOUT_SECS,
        10,
        300,
    );

    MathpixMode::Enabled(MathpixConfig {
        app_id,
        app_key: api_token.expect("api token presence was checked"),
        request_timeout: Duration::from_secs(timeout_secs),
        poll_interval: Duration::from_millis(poll_interval_ms),
        poll_timeout: Duration::from_secs(poll_timeout_secs),
    })
}

fn parse_non_empty(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_u64(raw: Option<String>, default: u64, min: u64, max: u64) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(min, max))
        .unwrap_or(default.clamp(min, max))
}

#[derive(Clone, Debug)]
struct MathpixUsageCache {
    app_id: String,
    fetched_at: Instant,
    summary: MathpixUsageSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsageRecord {
    usage_type: String,
    count: u64,
}

fn usage_cache_get(app_id: &str) -> Option<MathpixUsageSummary> {
    let Ok(guard) = USAGE_CACHE.lock() else {
        return None;
    };
    let entry = guard.as_ref()?;
    if entry.app_id != app_id || entry.fetched_at.elapsed() > MATHPIX_USAGE_CACHE_TTL {
        return None;
    }
    Some(entry.summary.clone())
}

fn usage_cache_put(app_id: &str, summary: MathpixUsageSummary) {
    let Ok(mut guard) = USAGE_CACHE.lock() else {
        return;
    };
    *guard = Some(MathpixUsageCache {
        app_id: app_id.to_string(),
        fetched_at: Instant::now(),
        summary,
    });
}

async fn fetch_usage_summary(config: &MathpixConfig) -> Result<MathpixUsageSummary, String> {
    let usage_timeout = config.request_timeout.min(Duration::from_secs(8));
    let client = reqwest::Client::builder()
        .timeout(usage_timeout)
        .build()
        .map_err(|error| format!("failed to build mathpix usage client: {error}"))?;

    let today = Utc::now().date_naive();
    let month_start = today.with_day(1).unwrap_or(today);
    let all_time_start = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or(month_start);

    let month_records = fetch_usage_records(&client, config, month_start, today).await?;
    let all_time_records = fetch_usage_records(&client, config, all_time_start, today).await?;

    let month = summarize_usage_window(&month_records);
    let all_time = summarize_usage_window(&all_time_records);

    let mut warning_parts = Vec::new();
    append_unknown_types_warning(
        &mut warning_parts,
        "current month",
        unknown_usage_types(&month),
    );
    append_unknown_types_warning(
        &mut warning_parts,
        "all-time",
        unknown_usage_types(&all_time),
    );

    Ok(MathpixUsageSummary {
        month,
        all_time,
        warning: if warning_parts.is_empty() {
            None
        } else {
            Some(format!(
                "Estimated cost excludes unsupported usage types: {}.",
                warning_parts.join("; ")
            ))
        },
    })
}

fn append_unknown_types_warning(
    warnings: &mut Vec<String>,
    label: &str,
    unknown_usage_types: Vec<String>,
) {
    if unknown_usage_types.is_empty() {
        return;
    }
    warnings.push(format!("{label} [{}]", unknown_usage_types.join(", ")));
}

async fn fetch_usage_records(
    client: &reqwest::Client,
    config: &MathpixConfig,
    from_date: NaiveDate,
    to_date: NaiveDate,
) -> Result<Vec<UsageRecord>, String> {
    let url = format!("{MATHPIX_USAGE_BASE_URL}/v3/ocr-usage");
    let from_date_text = from_date.format("%Y-%m-%d").to_string();
    let to_date_text = to_date.format("%Y-%m-%d").to_string();
    let response = client
        .get(url)
        .header("app_id", config.app_id.as_str())
        .header("app_key", config.app_key.as_str())
        .query(&[
            ("group_by", "usage_type"),
            ("from_date", from_date_text.as_str()),
            ("to_date", to_date_text.as_str()),
        ])
        .send()
        .await
        .map_err(|error| {
            format!("failed to call /v3/ocr-usage for {from_date_text}..{to_date_text}: {error}")
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "/v3/ocr-usage returned {status} for {from_date_text}..{to_date_text}: {}",
            summarize_error_body(body.as_str())
        ));
    }

    let body = response
        .text()
        .await
        .map_err(|error| format!("failed to decode /v3/ocr-usage response body: {error}"))?;
    parse_usage_records(body.as_str())
}

fn summarize_usage_window(records: &[UsageRecord]) -> MathpixUsageWindow {
    let total_requests = records
        .iter()
        .fold(0u64, |acc, record| acc.saturating_add(record.count));

    let mut by_usage_type = BTreeMap::<String, u64>::new();
    for record in records {
        let normalized = normalize_usage_type(record.usage_type.as_str());
        if normalized.is_empty() {
            continue;
        }
        let entry = by_usage_type.entry(normalized).or_insert(0);
        *entry = entry.saturating_add(record.count);
    }

    let mut breakdown = by_usage_type
        .into_iter()
        .map(|(usage_type, count)| MathpixUsageBreakdown {
            cost_class: classify_usage_type(usage_type.as_str()),
            usage_type,
            count,
            estimated_cost_usd: 0.0,
        })
        .collect::<Vec<_>>();

    let image_requests = breakdown
        .iter()
        .filter(|item| item.cost_class == MathpixUsageCostClass::ImageRequest)
        .fold(0u64, |acc, item| acc.saturating_add(item.count));
    let pdf_requests = breakdown
        .iter()
        .filter(|item| item.cost_class == MathpixUsageCostClass::PdfRequest)
        .fold(0u64, |acc, item| acc.saturating_add(item.count));

    let image_cost = tiered_cost(
        image_requests,
        IMAGE_TIER1_LIMIT,
        IMAGE_TIER2_LIMIT,
        IMAGE_TIER1_RATE,
        IMAGE_TIER2_RATE,
        IMAGE_TIER3_RATE,
    );
    let pdf_cost = tiered_cost(
        pdf_requests,
        PDF_TIER1_LIMIT,
        PDF_TIER2_LIMIT,
        PDF_TIER1_RATE,
        PDF_TIER2_RATE,
        PDF_TIER3_RATE,
    );

    for item in &mut breakdown {
        let (class_count, class_cost) = match item.cost_class {
            MathpixUsageCostClass::ImageRequest => (image_requests, image_cost),
            MathpixUsageCostClass::PdfRequest => (pdf_requests, pdf_cost),
            MathpixUsageCostClass::Unknown => (0, 0.0),
        };
        if class_count == 0 || class_cost <= 0.0 {
            continue;
        }
        item.estimated_cost_usd = class_cost * (item.count as f64 / class_count as f64);
    }

    let estimated_cost_usd = image_cost + pdf_cost;

    MathpixUsageWindow {
        total_requests,
        estimated_cost_usd,
        breakdown,
    }
}

fn tiered_cost(
    count: u64,
    tier1_limit: u64,
    tier2_limit: u64,
    tier1_rate: f64,
    tier2_rate: f64,
    tier3_rate: f64,
) -> f64 {
    if count == 0 {
        return 0.0;
    }

    let tier1_count = count.min(tier1_limit);
    let tier2_count = count
        .saturating_sub(tier1_limit)
        .min(tier2_limit.saturating_sub(tier1_limit));
    let tier3_count = count.saturating_sub(tier2_limit);

    tier1_count as f64 * tier1_rate
        + tier2_count as f64 * tier2_rate
        + tier3_count as f64 * tier3_rate
}

fn unknown_usage_types(window: &MathpixUsageWindow) -> Vec<String> {
    let mut unknown = window
        .breakdown
        .iter()
        .filter(|item| item.cost_class == MathpixUsageCostClass::Unknown)
        .map(|item| item.usage_type.clone())
        .collect::<Vec<_>>();
    unknown.sort_unstable();
    unknown.dedup();
    unknown
}

fn normalize_usage_type(usage_type: &str) -> String {
    let mut normalized = String::with_capacity(usage_type.len());
    let mut prev_sep = false;
    for ch in usage_type.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep {
            normalized.push('-');
            prev_sep = true;
        }
    }
    normalized.trim_matches('-').to_string()
}

fn classify_usage_type(usage_type: &str) -> MathpixUsageCostClass {
    let normalized = normalize_usage_type(usage_type);
    if normalized.is_empty() {
        return MathpixUsageCostClass::Unknown;
    }

    match normalized.as_str() {
        "image" | "image-async" | "text" | "batch" | "ocr-results" | "latex" | "strokes" => {
            MathpixUsageCostClass::ImageRequest
        }
        "pdf" | "pdf-async" | "pdf-page" | "pdf-pages" | "pdfpages" => {
            MathpixUsageCostClass::PdfRequest
        }
        _ if normalized.starts_with("image-") => MathpixUsageCostClass::ImageRequest,
        _ if normalized.starts_with("pdf-") => MathpixUsageCostClass::PdfRequest,
        _ => MathpixUsageCostClass::Unknown,
    }
}

fn parse_usage_records(body: &str) -> Result<Vec<UsageRecord>, String> {
    let payload: Value = serde_json::from_str(body)
        .map_err(|error| format!("failed to parse /v3/ocr-usage response: {error}"))?;
    let rows = payload
        .get("ocr_usage")
        .and_then(Value::as_array)
        .ok_or_else(|| "/v3/ocr-usage response missing `ocr_usage` array".to_string())?;

    let mut records = Vec::new();
    for row in rows {
        let Some(usage_type) = row.get("usage_type").and_then(Value::as_str) else {
            continue;
        };
        let usage_type = usage_type.trim();
        if usage_type.is_empty() {
            continue;
        }
        let Some(count) = parse_usage_count(row) else {
            continue;
        };
        records.push(UsageRecord {
            usage_type: usage_type.to_string(),
            count,
        });
    }
    Ok(records)
}

fn parse_usage_count(row: &Value) -> Option<u64> {
    let value = row.get("count")?;
    if let Some(count) = value.as_u64() {
        return Some(count);
    }
    if let Some(count) = value.as_i64() {
        return u64::try_from(count.max(0)).ok();
    }
    if let Some(count) = value.as_f64() {
        if !count.is_finite() || count.is_sign_negative() {
            return None;
        }
        return Some(count.round() as u64);
    }
    if let Some(count) = value.as_str() {
        let trimmed = count.trim();
        if let Ok(parsed) = trimmed.parse::<u64>() {
            return Some(parsed);
        }
        if let Ok(parsed) = trimmed.parse::<f64>() {
            if !parsed.is_finite() || parsed.is_sign_negative() {
                return None;
            }
            return Some(parsed.round() as u64);
        }
    }
    None
}

fn summarize_error_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "empty response body".to_string();
    }
    const MAX: usize = 240;
    if compact.chars().count() <= MAX {
        compact
    } else {
        format!("{}...", compact.chars().take(MAX).collect::<String>())
    }
}
#[cfg(test)]
#[path = "../../tests/unit/integrations_mathpix.rs"]
mod tests;
