use std::{collections::HashSet, env, process::Command};

use serde::Serialize;

const SCREENSHOT_REQUIRED_FONT_FAMILIES_ENV: &str = "SCREENSHOT_REQUIRED_FONT_FAMILIES";
const SCREENSHOT_FONT_CHECK_ENABLED_ENV: &str = "SCREENSHOT_FONT_CHECK_ENABLED";
const DEFAULT_SCREENSHOT_REQUIRED_FONT_FAMILIES: &str =
    "Noto Sans,Noto Serif,Noto Sans Mono,Noto Color Emoji";

const OS_RELEASE_PATH: &str = "/etc/os-release";

const APT_INSTALL_HINT: &str =
    "apt install -y fontconfig fonts-noto fonts-noto-cjk fonts-noto-color-emoji";
const DNF_INSTALL_HINT: &str = "dnf install -y fontconfig google-noto-sans-fonts google-noto-serif-fonts google-noto-sans-mono-fonts google-noto-color-emoji-fonts";
const APK_INSTALL_HINT: &str =
    "apk add --no-cache fontconfig ttf-dejavu font-noto font-noto-cjk font-noto-emoji";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FontMatchResult {
    pub family: String,
    pub matched_font: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ScreenshotFontDiagnostics {
    pub fontconfig_found: bool,
    pub required_families: Vec<String>,
    pub missing_families: Vec<String>,
    pub resolved_matches: Vec<FontMatchResult>,
}

#[derive(Clone, Debug)]
pub(crate) struct FontDiagnostics {
    pub checks_enabled: bool,
    pub applicable: bool,
    pub platform: String,
    pub fontconfig_found: bool,
    pub required_families: Vec<String>,
    pub missing_families: Vec<String>,
    pub resolved_matches: Vec<FontMatchResult>,
    pub install_hint: Option<String>,
    pub fontconfig_error: Option<String>,
}

impl Default for FontDiagnostics {
    fn default() -> Self {
        Self {
            checks_enabled: true,
            applicable: false,
            platform: env::consts::OS.to_string(),
            fontconfig_found: false,
            required_families: default_required_families(),
            missing_families: Vec::new(),
            resolved_matches: Vec::new(),
            install_hint: None,
            fontconfig_error: None,
        }
    }
}

impl FontDiagnostics {
    pub fn has_missing_fonts(&self) -> bool {
        self.checks_enabled
            && self.applicable
            && (!self.fontconfig_found || !self.missing_families.is_empty())
    }

    pub fn screenshot_artifact_context(&self) -> Option<ScreenshotFontDiagnostics> {
        if !self.checks_enabled || !self.applicable {
            return None;
        }

        Some(ScreenshotFontDiagnostics {
            fontconfig_found: self.fontconfig_found,
            required_families: self.required_families.clone(),
            missing_families: self.missing_families.clone(),
            resolved_matches: self.resolved_matches.clone(),
        })
    }
}

pub(crate) fn current() -> FontDiagnostics {
    let checks_enabled = env_bool(SCREENSHOT_FONT_CHECK_ENABLED_ENV, true);
    let required_families = required_families_from_env();
    let platform = env::consts::OS.to_string();

    if !checks_enabled {
        return FontDiagnostics {
            checks_enabled,
            applicable: cfg!(target_os = "linux"),
            platform,
            fontconfig_found: false,
            required_families,
            missing_families: Vec::new(),
            resolved_matches: Vec::new(),
            install_hint: linux_install_hint(),
            fontconfig_error: None,
        };
    }

    if !cfg!(target_os = "linux") {
        return FontDiagnostics {
            checks_enabled,
            applicable: false,
            platform,
            fontconfig_found: false,
            required_families,
            missing_families: Vec::new(),
            resolved_matches: Vec::new(),
            install_hint: None,
            fontconfig_error: None,
        };
    }

    let fontconfig_version = run_command("fc-list", &["--version"]);
    if let Err(error) = fontconfig_version {
        return FontDiagnostics {
            checks_enabled,
            applicable: true,
            platform,
            fontconfig_found: false,
            missing_families: required_families.clone(),
            required_families,
            resolved_matches: Vec::new(),
            install_hint: linux_install_hint(),
            fontconfig_error: Some(error),
        };
    }

    let mut missing_families = Vec::new();
    let mut resolved_matches = Vec::new();
    for family in &required_families {
        match run_command("fc-match", &[family.as_str()]) {
            Ok(output) => {
                let matched_font = parse_fc_match_family(output.as_str());
                let matched_value = matched_font.as_deref().unwrap_or(output.as_str());
                if !fc_match_satisfies_family(family.as_str(), matched_value) {
                    missing_families.push(family.clone());
                }
                resolved_matches.push(FontMatchResult {
                    family: family.clone(),
                    matched_font,
                });
            }
            Err(_) => {
                missing_families.push(family.clone());
                resolved_matches.push(FontMatchResult {
                    family: family.clone(),
                    matched_font: None,
                });
            }
        }
    }

    FontDiagnostics {
        checks_enabled,
        applicable: true,
        platform,
        fontconfig_found: true,
        required_families,
        missing_families,
        resolved_matches,
        install_hint: linux_install_hint(),
        fontconfig_error: None,
    }
}

fn run_command(command: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run `{command}`: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!(
                "`{command}` exited with non-zero status {}",
                output.status
            ));
        }
        return Err(format!(
            "`{command}` exited with non-zero status {}: {stderr}",
            output.status
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_fc_match_family(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }

    if let Some(start) = line.find('"') {
        let rest = &line[start + 1..];
        if let Some(end) = rest.find('"') {
            let quoted = rest[..end].trim();
            if !quoted.is_empty() {
                return Some(quoted.to_string());
            }
        }
    }

    if let Some((before_colon, _)) = line.split_once(':') {
        let candidate = before_colon.trim();
        if !candidate.is_empty() {
            return Some(candidate.to_string());
        }
    }

    Some(line.to_string())
}

fn fc_match_satisfies_family(required_family: &str, matched_value: &str) -> bool {
    let required = compact_family(required_family);
    let matched = compact_family(matched_value);
    if required.is_empty() || matched.is_empty() {
        return false;
    }

    matched == required || matched.contains(required.as_str())
}

fn compact_family(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn required_families_from_env() -> Vec<String> {
    let raw = env::var(SCREENSHOT_REQUIRED_FONT_FAMILIES_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SCREENSHOT_REQUIRED_FONT_FAMILIES.to_string());
    parse_required_families(raw.as_str())
}

fn default_required_families() -> Vec<String> {
    parse_required_families(DEFAULT_SCREENSHOT_REQUIRED_FONT_FAMILIES)
}

fn parse_required_families(raw: &str) -> Vec<String> {
    let mut seen = HashSet::<String>::new();
    let mut families = Vec::new();
    for candidate in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let normalized = candidate.to_ascii_lowercase();
        if seen.insert(normalized) {
            families.push(candidate.to_string());
        }
    }
    families
}

fn linux_install_hint() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }

    let os_release = std::fs::read_to_string(OS_RELEASE_PATH).ok();
    Some(match package_manager_hint(os_release.as_deref()) {
        LinuxPackageManager::Apt => APT_INSTALL_HINT.to_string(),
        LinuxPackageManager::Dnf => DNF_INSTALL_HINT.to_string(),
        LinuxPackageManager::Apk => APK_INSTALL_HINT.to_string(),
        LinuxPackageManager::Unknown => format!(
            "Install fontconfig + Noto fonts with your distro package manager.\nDebian/Ubuntu: {APT_INSTALL_HINT}\nFedora/RHEL: {DNF_INSTALL_HINT}\nAlpine: {APK_INSTALL_HINT}"
        ),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinuxPackageManager {
    Apt,
    Dnf,
    Apk,
    Unknown,
}

fn package_manager_hint(os_release: Option<&str>) -> LinuxPackageManager {
    let Some(os_release) = os_release else {
        return LinuxPackageManager::Unknown;
    };

    let mut tags = String::new();
    for line in os_release.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key != "ID" && key != "ID_LIKE" {
            continue;
        }
        let cleaned = value.trim().trim_matches('"').to_ascii_lowercase();
        if !tags.is_empty() {
            tags.push(' ');
        }
        tags.push_str(cleaned.as_str());
    }

    if tags.contains("alpine") {
        return LinuxPackageManager::Apk;
    }
    if tags.contains("debian")
        || tags.contains("ubuntu")
        || tags.contains("linuxmint")
        || tags.contains("pop")
    {
        return LinuxPackageManager::Apt;
    }
    if tags.contains("fedora")
        || tags.contains("rhel")
        || tags.contains("centos")
        || tags.contains("rocky")
        || tags.contains("almalinux")
        || tags.contains("suse")
    {
        return LinuxPackageManager::Dnf;
    }

    LinuxPackageManager::Unknown
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .and_then(|value| match value.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{
        LinuxPackageManager, fc_match_satisfies_family, package_manager_hint,
        parse_fc_match_family, parse_required_families,
    };

    #[test]
    fn parse_required_families_trims_and_deduplicates() {
        let families =
            parse_required_families(" Noto Sans, Noto Serif, noto sans, ,Noto Color Emoji ");
        assert_eq!(
            families,
            vec!["Noto Sans", "Noto Serif", "Noto Color Emoji"]
        );
    }

    #[test]
    fn parse_fc_match_family_extracts_quoted_family() {
        let parsed = parse_fc_match_family(r#"NotoSans-Regular.ttf: "Noto Sans" "Regular""#);
        assert_eq!(parsed.as_deref(), Some("Noto Sans"));
    }

    #[test]
    fn fc_match_satisfies_family_accepts_exact_and_extended_matches() {
        assert!(fc_match_satisfies_family("Noto Sans", "Noto Sans"));
        assert!(fc_match_satisfies_family("Noto Sans", "Noto Sans CJK JP"));
        assert!(!fc_match_satisfies_family("Noto Sans", "DejaVu Sans"));
    }

    #[test]
    fn package_manager_hint_detects_apt() {
        let os_release = r#"ID=ubuntu
ID_LIKE=debian
"#;
        assert_eq!(
            package_manager_hint(Some(os_release)),
            LinuxPackageManager::Apt
        );
    }

    #[test]
    fn package_manager_hint_detects_dnf() {
        let os_release = r#"ID=fedora
ID_LIKE="rhel fedora"
"#;
        assert_eq!(
            package_manager_hint(Some(os_release)),
            LinuxPackageManager::Dnf
        );
    }

    #[test]
    fn package_manager_hint_detects_apk() {
        let os_release = r#"ID=alpine"#;
        assert_eq!(
            package_manager_hint(Some(os_release)),
            LinuxPackageManager::Apk
        );
    }
}
