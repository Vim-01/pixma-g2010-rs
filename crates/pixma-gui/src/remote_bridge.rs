//! Automatic SSH stdio-bridge launcher for `pixma-studio`.
//!
//! When `pixma-studio` is started directly (e.g., from a desktop launcher or terminal
//! without the `pixma-remote` wrapper) and no local Canon PIXMA USB device or local
//! `pixmad` Unix socket is active, this module checks
//! `$XDG_CONFIG_HOME/pixma-g2010/remote.conf` (or `~/.config/pixma-g2010/remote.conf`)
//! and spawns an isolated `pixma-sock-bridge --listen ... -- ssh ...` tunnel for the
//! lifetime of the GUI process.

use pixma_core::models::{PrinterModel, CANON_USB_VID};
use std::env;
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Parsed remote SSH bridge configuration from `~/.config/pixma-g2010/remote.conf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBridgeConfig {
    /// SSH destination in `[user@]host` format (for example `pixmatunnel@192.0.2.18`).
    pub remote_target: String,
    /// Hostname or IP displayed in the UI.
    pub remote_host: String,
    /// Optional SSH private key path.
    pub ssh_key: Option<PathBuf>,
    /// Optional dedicated `known_hosts` file path.
    pub known_hosts: Option<PathBuf>,
    /// Optional explicit path to `pixma-sock-bridge`.
    pub bridge_bin: Option<PathBuf>,
}

/// Owned child `pixma-sock-bridge` process and private socket cleaned up on drop.
pub struct AutoRemoteBridge {
    child: Child,
    socket_path: PathBuf,
    remote_host: String,
}

impl AutoRemoteBridge {
    /// Returns the local Unix socket path exposed by the bridge.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the configured remote host label for UI telemetry.
    pub fn remote_host(&self) -> &str {
        &self.remote_host
    }
}

impl Drop for AutoRemoteBridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Validates and parses `key=value` entries from `remote.conf`.
pub fn parse_remote_bridge_config(contents: &str) -> Result<Option<RemoteBridgeConfig>, String> {
    let mut remote_target: Option<String> = None;
    let mut remote_host: Option<String> = None;
    let mut ssh_key: Option<PathBuf> = None;
    let mut known_hosts: Option<PathBuf> = None;
    let mut bridge_bin: Option<PathBuf> = None;

    for (line_idx, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_val)) = line.split_once('=') else {
            return Err(format!("line {}: expected key=value", line_idx + 1));
        };
        let key = key.trim();
        let val = unquote_config_value(raw_val.trim());
        if val.is_empty() {
            continue;
        }
        match key {
            "remote_target" => remote_target = Some(val),
            "remote_host" => remote_host = Some(val),
            "ssh_key" => ssh_key = Some(PathBuf::from(val)),
            "known_hosts" => known_hosts = Some(PathBuf::from(val)),
            "bridge_bin" => bridge_bin = Some(PathBuf::from(val)),
            _ => {}
        }
    }

    let Some(target) = remote_target else {
        return Ok(None);
    };
    if target.starts_with('-')
        || target
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("invalid remote_target in remote.conf".to_string());
    }

    let host = remote_host.unwrap_or_else(|| {
        target
            .rsplit_once('@')
            .map(|(_, h)| h.to_string())
            .unwrap_or_else(|| target.clone())
    });

    Ok(Some(RemoteBridgeConfig {
        remote_target: target,
        remote_host: host,
        ssh_key,
        known_hosts,
        bridge_bin,
    }))
}

fn unquote_config_value(val: &str) -> String {
    if val.len() >= 2
        && ((val.starts_with('"') && val.ends_with('"'))
            || (val.starts_with('\'') && val.ends_with('\'')))
    {
        val[1..val.len() - 1].to_string()
    } else {
        val.to_string()
    }
}

fn default_config_path() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("pixma-g2010").join("remote.conf"));
    }
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("pixma-g2010")
                .join("remote.conf")
        })
}

fn has_local_canon_usb() -> bool {
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return false;
    };
    for entry in entries.flatten() {
        let dev_path = entry.path();
        let Ok(vid) = fs::read_to_string(dev_path.join("idVendor")) else {
            continue;
        };
        if u16::from_str_radix(vid.trim(), 16) != Ok(CANON_USB_VID) {
            continue;
        }
        let Ok(pid) = fs::read_to_string(dev_path.join("idProduct")) else {
            continue;
        };
        if u16::from_str_radix(pid.trim(), 16)
            .ok()
            .and_then(PrinterModel::from_usb_pid)
            .is_some()
        {
            return true;
        }
    }
    false
}

fn has_live_local_daemon_socket() -> bool {
    pixma_daemon::default_socket_candidates()
        .into_iter()
        .any(|sock| sock.exists() && UnixStream::connect(&sock).is_ok())
}

fn resolve_bridge_binary(explicit: Option<&PathBuf>) -> PathBuf {
    if let Some(bin) = explicit {
        return bin.clone();
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("pixma-sock-bridge");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("pixma-sock-bridge")
}

fn build_listen_socket_path() -> Option<PathBuf> {
    let sock_name = format!("pixma-studio-bridge-{}.sock", std::process::id());
    if let Some(xdg) = env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
    {
        let runtime_dir = xdg.join("pixma");
        if pixma_daemon::ensure_verified_private_dir(&runtime_dir).is_ok() {
            return Some(runtime_dir.join(sock_name));
        }
    }

    let fallback_dir = env::temp_dir().join(format!("pixma-studio-{}", pixma_daemon::current_uid()));
    if let Err(err) = pixma_daemon::ensure_verified_private_dir(&fallback_dir) {
        tracing::warn!(
            "Failed to prepare verified private socket directory {}: {err}",
            fallback_dir.display()
        );
        return None;
    }
    Some(fallback_dir.join(sock_name))
}

/// Starts `pixma-sock-bridge` automatically when no explicit socket, local USB printer,
/// or local daemon socket is present and `remote.conf` exists.
pub fn maybe_start_auto_bridge(
    is_mock_mode: bool,
    configured_socket: Option<&str>,
) -> Option<AutoRemoteBridge> {
    if is_mock_mode
        || configured_socket.is_some()
        || env::var_os("PIXMA_DISABLE_AUTO_REMOTE").is_some()
        || has_local_canon_usb()
        || has_live_local_daemon_socket()
    {
        return None;
    }

    let cfg_path = default_config_path()?;
    let contents = fs::read_to_string(&cfg_path).ok()?;
    let cfg = match parse_remote_bridge_config(&contents) {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(err) => {
            tracing::warn!(
                "Ignoring malformed remote config {}: {err}",
                cfg_path.display()
            );
            return None;
        }
    };

    if let Some(ref key) = cfg.ssh_key {
        if !key.is_file() {
            tracing::warn!(
                "Remote bridge SSH key {} does not exist; skipping auto bridge",
                key.display()
            );
            return None;
        }
    }

    let socket_path = build_listen_socket_path()?;
    let _ = fs::remove_file(&socket_path);

    let bridge_bin = resolve_bridge_binary(cfg.bridge_bin.as_ref());
    let mut cmd = Command::new(&bridge_bin);
    cmd.arg("--listen")
        .arg(&socket_path)
        .arg("--")
        .arg("ssh")
        .arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=3");

    if let Some(ref key) = cfg.ssh_key {
        cmd.arg("-i").arg(key);
    }
    if let Some(ref kh) = cfg.known_hosts {
        cmd.arg("-o")
            .arg(format!("UserKnownHostsFile={}", kh.display()));
    }
    cmd.arg(&cfg.remote_target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(
                "Failed to spawn auto remote bridge `{}`: {err}",
                bridge_bin.display()
            );
            return None;
        }
    };

    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut socket_ready = false;
    while Instant::now() < deadline {
        if socket_path.exists() {
            socket_ready = true;
            break;
        }
        if let Ok(Some(_)) = child.try_wait() {
            let _ = fs::remove_file(&socket_path);
            return None;
        }
        thread::sleep(Duration::from_millis(40));
    }

    if !socket_ready && !socket_path.exists() {
        tracing::warn!(
            "Auto remote bridge did not expose socket {} within 1.5s; terminating child",
            socket_path.display()
        );
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&socket_path);
        return None;
    }

    Some(AutoRemoteBridge {
        child,
        socket_path,
        remote_host: cfg.remote_host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_remote_conf_with_comments_and_quotes() {
        let raw = r#"
# Remote Canon G2010 usb-server bridge
remote_target = "pixmatunnel@192.0.2.18"
ssh_key = '/etc/pixma-test/pixmatunnel_ed25519'
known_hosts = /etc/pixma-test/pixmatunnel_known_hosts
"#;
        let cfg = parse_remote_bridge_config(raw)
            .expect("parse should succeed")
            .expect("config should be present");
        assert_eq!(cfg.remote_target, "pixmatunnel@192.0.2.18");
        assert_eq!(cfg.remote_host, "192.0.2.18");
        assert_eq!(
            cfg.ssh_key.as_deref(),
            Some(Path::new("/etc/pixma-test/pixmatunnel_ed25519"))
        );
        assert_eq!(
            cfg.known_hosts.as_deref(),
            Some(Path::new("/etc/pixma-test/pixmatunnel_known_hosts"))
        );
    }

    #[test]
    fn rejects_option_like_or_whitespace_remote_target() {
        assert!(parse_remote_bridge_config("remote_target=-oProxyCommand=bad").is_err());
        assert!(parse_remote_bridge_config("remote_target=user@host extra").is_err());
        assert_eq!(parse_remote_bridge_config("# only comment\n").unwrap(), None);
    }

    #[test]
    fn listen_socket_path_uses_verified_private_parent_directory() {
        let sock = build_listen_socket_path().expect("verified socket path");
        let parent = sock.parent().expect("socket parent dir");
        assert!(pixma_daemon::is_verified_private_dir(
            parent,
            pixma_daemon::current_uid()
        ));
    }
}
