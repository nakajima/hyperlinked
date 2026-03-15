use std::process::{Child, Command, Stdio};

const DEFAULT_SERVICE_TYPE: &str = "_hyperlinked._tcp.local.";

#[derive(Clone, Debug)]
pub struct MdnsOptions {
    pub enabled: bool,
    pub service_name: String,
    pub service_type: String,
}

impl Default for MdnsOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            service_name: default_service_name(),
            service_type: DEFAULT_SERVICE_TYPE.to_string(),
        }
    }
}

impl MdnsOptions {
    pub fn new(enabled: bool, service_name: String, service_type: String) -> Self {
        let service_name = service_name.trim().to_string();
        let service_name = if service_name.is_empty() {
            default_service_name()
        } else {
            service_name
        };

        let service_type = service_type.trim().to_string();
        let service_type = if service_type.is_empty() {
            DEFAULT_SERVICE_TYPE.to_string()
        } else {
            service_type
        };

        Self {
            enabled,
            service_name,
            service_type,
        }
    }

    pub fn default_service_name() -> String {
        default_service_name()
    }

    pub fn default_service_type() -> &'static str {
        DEFAULT_SERVICE_TYPE
    }
}

pub struct MdnsAdvertisement {
    child: Option<Child>,
}

impl MdnsAdvertisement {
    pub fn start(options: &MdnsOptions, port: u16) -> Result<Option<Self>, String> {
        if !options.enabled {
            return Ok(None);
        }

        let child = start_mdns_process(options, port)?;
        Ok(Some(Self { child: Some(child) }))
    }
}

impl Drop for MdnsAdvertisement {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };

        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => return,
        }

        let _ = child.kill();
        let _ = child.wait();
    }
}

fn default_service_name() -> String {
    if let Some(from_env) = std::env::var("HOSTNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return from_env;
    }

    if let Ok(output) = Command::new("hostname")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        let hostname = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !hostname.is_empty() {
            return hostname;
        }
    }

    "hyperlinked".to_string()
}

fn split_service_type_and_domain(service_type: &str) -> Result<(String, String), String> {
    let trimmed = service_type.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return Err("mDNS service type cannot be empty".to_string());
    }

    let lowercase = trimmed.to_ascii_lowercase();
    for transport in ["._tcp.", "._udp."] {
        if let Some(index) = lowercase.find(transport) {
            let type_end = index + transport.len() - 1;
            let service_type = trimmed[..type_end].to_string();
            let domain_raw = trimmed[type_end + 1..].trim_matches('.');
            let domain = if domain_raw.is_empty() {
                "local.".to_string()
            } else {
                format!("{domain_raw}.")
            };

            if !service_type.starts_with('_') {
                return Err(format!(
                    "invalid mDNS service type `{service_type}`: service name must start with `_`"
                ));
            }

            return Ok((service_type, domain));
        }
    }

    if lowercase.ends_with("._tcp") || lowercase.ends_with("._udp") {
        if !trimmed.starts_with('_') {
            return Err(format!(
                "invalid mDNS service type `{trimmed}`: service name must start with `_`"
            ));
        }
        return Ok((trimmed.to_string(), "local.".to_string()));
    }

    Err(format!(
        "invalid mDNS service type `{trimmed}`: expected `_<name>._tcp` or `_<name>._tcp.<domain>`"
    ))
}

fn ensure_process_is_running(mut child: Child, label: &str) -> Result<Child, String> {
    if let Some(status) = child
        .try_wait()
        .map_err(|err| format!("failed to inspect {label} process status: {err}"))?
    {
        return Err(format!(
            "{label} advertisement process exited immediately with status {status}"
        ));
    }

    Ok(child)
}

#[cfg(target_os = "macos")]
fn start_mdns_process(options: &MdnsOptions, port: u16) -> Result<Child, String> {
    let (service_type, domain) = split_service_type_and_domain(&options.service_type)?;
    let child = Command::new("dns-sd")
        .arg("-R")
        .arg(options.service_name.trim())
        .arg(service_type.as_str())
        .arg(domain.as_str())
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            format!(
                "failed to start mDNS advertisement process `dns-sd`: {err}. \
                 Ensure `dns-sd` is available on this system"
            )
        })?;

    ensure_process_is_running(child, "mDNS")
}

#[cfg(target_os = "linux")]
fn start_mdns_process(options: &MdnsOptions, port: u16) -> Result<Child, String> {
    let (service_type, domain) = split_service_type_and_domain(&options.service_type)?;
    let domain = domain.trim_end_matches('.');

    let child = Command::new("avahi-publish-service")
        .arg("--no-fail")
        .arg(format!("--domain={domain}"))
        .arg(options.service_name.trim())
        .arg(service_type.as_str())
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            format!(
                "failed to start mDNS advertisement process `avahi-publish-service`: {err}. \
                 Ensure `avahi-publish-service` (avahi-utils) is installed and Avahi is running"
            )
        })?;

    ensure_process_is_running(child, "mDNS")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn start_mdns_process(_options: &MdnsOptions, _port: u16) -> Result<Child, String> {
    Err(
        "mDNS advertisement is currently supported on macOS (`dns-sd`) and Linux (`avahi-publish-service`)"
            .to_string(),
    )
}
#[cfg(test)]
#[path = "../../tests/unit/server_mdns.rs"]
mod tests;
