//! Persistent host list (`hosts.json` in the app config dir).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const LOCAL_ID: &str = "local";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostConfig {
    pub id: String,
    pub name: String,
    /// `None` runs Kimi on this computer; otherwise an `ssh` destination
    /// (usually an alias from `~/.ssh/config`).
    #[serde(default)]
    pub ssh_host: Option<String>,
    #[serde(default = "default_command")]
    pub command: String,
    /// Hide GPUs from the agent so GPU work goes through the scheduler.
    #[serde(default)]
    pub hide_gpus: bool,
    /// Local port the host is served on. Kept stable across reconnects so the
    /// web UI keeps its origin (and therefore its storage and live socket).
    #[serde(default)]
    pub local_port: Option<u16>,
}

fn default_command() -> String {
    "kimi".into()
}

impl HostConfig {
    pub fn is_local(&self) -> bool {
        self.ssh_host.is_none()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name cannot be empty.".into());
        }
        if let Some(host) = &self.ssh_host {
            let ok = !host.is_empty()
                && !host.starts_with('-')
                && host.chars().all(|c| c.is_ascii_alphanumeric() || "_.@:-".contains(c));
            if !ok {
                return Err("SSH host contains unsupported characters. Use an alias from ~/.ssh/config for advanced options.".into());
            }
        }
        let ok = !self.command.is_empty()
            && !self.command.starts_with('-')
            && self.command.chars().all(|c| c.is_ascii_alphanumeric() || "_./~-".contains(c));
        if !ok {
            return Err("Kimi command must be an executable name or path without spaces.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Use Kimi's opt-in desktop presentation (`?kimi_desktop=1`) and its
    /// optional `window.kimiDesktop` hooks, with a seamless title bar.
    /// Turning it off falls back to the plain web UI under a native title
    /// bar, should a Kimi update ever change those hooks.
    #[serde(default = "enabled")]
    pub desktop_integration: bool,
    /// Native frosted-glass material behind Kimi's sidebar (macOS).
    #[serde(default = "enabled")]
    pub vibrancy: bool,
    /// Kimi's first-run onboarding was completed once; every host (each
    /// its own origin) is told so instead of asking again.
    #[serde(default)]
    pub onboarded: bool,
}

fn enabled() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self { desktop_integration: true, vibrancy: true, onboarded: false }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
    /// Host windows open at quit, reopened on launch.
    #[serde(default)]
    pub open_hosts: Vec<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("hosts.json");
        let mut store: Store = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        store.path = path;
        if !store.hosts.iter().any(|h| h.id == LOCAL_ID) {
            store.hosts.insert(
                0,
                HostConfig {
                    id: LOCAL_ID.into(),
                    name: local_name(),
                    ssh_host: None,
                    command: default_command(),
                    hide_gpus: false,
                    local_port: None,
                },
            );
        }
        store
    }

    pub fn save(&self) {
        let Some(dir) = self.path.parent() else { return };
        let _ = std::fs::create_dir_all(dir);
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&HostConfig> {
        self.hosts.iter().find(|h| h.id == id)
    }

    pub fn upsert(&mut self, mut host: HostConfig) -> Result<HostConfig, String> {
        host.name = host.name.trim().to_string();
        host.command = host.command.trim().to_string();
        host.ssh_host = host.ssh_host.map(|h| h.trim().to_string()).filter(|h| !h.is_empty());
        if host.id == LOCAL_ID {
            host.ssh_host = None;
        } else if host.ssh_host.is_none() {
            return Err("Enter an SSH host.".into());
        }
        host.validate()?;
        match self.hosts.iter_mut().find(|h| h.id == host.id) {
            Some(existing) => {
                // The stable port belongs to the app, not the form.
                host.local_port = existing.local_port;
                *existing = host.clone();
            }
            None => {
                if host.id.is_empty() {
                    host.id = new_id();
                }
                self.hosts.push(host.clone());
            }
        }
        self.save();
        Ok(host)
    }

    pub fn remove(&mut self, id: &str) {
        if id == LOCAL_ID {
            return;
        }
        self.hosts.retain(|h| h.id != id);
        self.open_hosts.retain(|h| h != id);
        self.save();
    }

    pub fn set_local_port(&mut self, id: &str, port: u16) {
        if let Some(host) = self.hosts.iter_mut().find(|h| h.id == id) {
            if host.local_port != Some(port) {
                host.local_port = Some(port);
                self.save();
            }
        }
    }

    pub fn reorder(&mut self, ids: &[String]) {
        self.hosts.sort_by_key(|h| ids.iter().position(|id| id == &h.id).unwrap_or(usize::MAX));
        self.save();
    }

    pub fn set_open(&mut self, id: &str, open: bool) {
        let present = self.open_hosts.iter().any(|h| h == id);
        if open && !present {
            self.open_hosts.push(id.to_string());
        } else if !open && present {
            self.open_hosts.retain(|h| h != id);
        } else {
            return;
        }
        self.save();
    }
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("h{:x}", nanos ^ (std::process::id() as u128) << 64)
}

fn local_name() -> String {
    if cfg!(target_os = "macos") {
        "This Mac".into()
    } else {
        "This Computer".into()
    }
}
