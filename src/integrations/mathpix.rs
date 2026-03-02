use std::time::Duration;

pub const MATHPIX_API_TOKEN_ENV: &str = "MATHPIX_API_TOKEN";
pub const MATHPIX_APP_ID_ENV: &str = "MATHPIX_APP_ID";

const MATHPIX_TIMEOUT_SECS_ENV: &str = "MATHPIX_TIMEOUT_SECS";
const MATHPIX_POLL_INTERVAL_MS_ENV: &str = "MATHPIX_POLL_INTERVAL_MS";
const MATHPIX_POLL_TIMEOUT_SECS_ENV: &str = "MATHPIX_POLL_TIMEOUT_SECS";

const DEFAULT_MATHPIX_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MATHPIX_POLL_INTERVAL_MS: u64 = 1_000;
const DEFAULT_MATHPIX_POLL_TIMEOUT_SECS: u64 = 90;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathpixStatus {
    pub enabled: bool,
    pub reason: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_is_disabled_when_api_token_missing() {
        let mode = parse_mode(MathpixEnvValues::default());
        let status = mode.status();
        assert!(!status.enabled);
        assert!(status.reason.contains(MATHPIX_API_TOKEN_ENV));
    }

    #[test]
    fn mode_is_disabled_when_app_id_missing() {
        let mode = parse_mode(MathpixEnvValues {
            api_token: Some("test-key".to_string()),
            app_id: None,
            ..Default::default()
        });
        let status = mode.status();
        assert!(!status.enabled);
        assert!(status.reason.contains(MATHPIX_APP_ID_ENV));
        assert!(mode.disabled_missing_app_id());
    }

    #[test]
    fn mode_is_enabled_when_required_env_is_present() {
        let mode = parse_mode(MathpixEnvValues {
            api_token: Some("  test-key  ".to_string()),
            app_id: Some("  app-id  ".to_string()),
            ..Default::default()
        });

        match mode {
            MathpixMode::Enabled(config) => {
                assert_eq!(config.app_key, "test-key");
                assert_eq!(config.app_id, "app-id");
                assert_eq!(config.request_timeout, Duration::from_secs(30));
                assert_eq!(config.poll_interval, Duration::from_millis(1_000));
                assert_eq!(config.poll_timeout, Duration::from_secs(90));
            }
            MathpixMode::Disabled(status) => {
                panic!("expected enabled mathpix mode, got {}", status.reason)
            }
        }
    }

    #[test]
    fn mode_clamps_mathpix_timing_values() {
        let mode = parse_mode(MathpixEnvValues {
            api_token: Some("test-key".to_string()),
            app_id: Some("app-id".to_string()),
            timeout_secs: Some("999".to_string()),
            poll_interval_ms: Some("1".to_string()),
            poll_timeout_secs: Some("9".to_string()),
        });

        match mode {
            MathpixMode::Enabled(config) => {
                assert_eq!(config.request_timeout, Duration::from_secs(120));
                assert_eq!(config.poll_interval, Duration::from_millis(250));
                assert_eq!(config.poll_timeout, Duration::from_secs(10));
            }
            MathpixMode::Disabled(status) => {
                panic!("expected enabled mathpix mode, got {}", status.reason)
            }
        }
    }
}
