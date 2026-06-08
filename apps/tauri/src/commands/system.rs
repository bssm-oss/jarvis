//! Local system status commands for the desktop control surface.

use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LaunchAgentInfo {
    pub label: String,
    pub pid: Option<i32>,
    pub last_exit_status: Option<i32>,
    pub running: bool,
}

const JARVIS_LAUNCH_AGENTS: &[&str] = &[
    "ai.zeroclaw.jarvis-daemon",
    "ai.zeroclaw.local-whisper",
    "ai.zeroclaw.desktop-dev",
];
const DESKTOP_AGENT_LABEL: &str = "ai.zeroclaw.desktop-dev";
const DESKTOP_PROCESS_PATTERN: &str = "zeroclaw-desktop";

#[tauri::command]
pub fn get_output_volume() -> Result<Option<u8>, String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("osascript")
            .args(["-e", "output volume of (get volume settings)"])
            .output()
            .map_err(|e| format!("read output volume failed: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }

        return parse_output_volume(&String::from_utf8_lossy(&output.stdout));
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

#[tauri::command]
pub fn get_launch_agent_statuses() -> Result<Vec<LaunchAgentInfo>, String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("launchctl")
            .arg("list")
            .output()
            .map_err(|e| format!("launchctl list failed: {e}"))?;

        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }

        let mut statuses = parse_launchctl_list(&String::from_utf8_lossy(&output.stdout));
        apply_desktop_process_fallback(&mut statuses);
        return Ok(statuses);
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(Vec::new())
    }
}

#[cfg(target_os = "macos")]
fn parse_output_volume(raw: &str) -> Result<Option<u8>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed: u8 = trimmed
        .parse()
        .map_err(|_| format!("invalid output volume: {trimmed}"))?;
    Ok(Some(parsed.min(100)))
}

#[cfg(target_os = "macos")]
fn parse_launchctl_list(raw: &str) -> Vec<LaunchAgentInfo> {
    JARVIS_LAUNCH_AGENTS
        .iter()
        .map(|label| {
            let matched = raw
                .lines()
                .filter_map(parse_launchctl_line)
                .find(|info| info.label == *label);
            matched.unwrap_or_else(|| LaunchAgentInfo {
                label: (*label).to_string(),
                pid: None,
                last_exit_status: None,
                running: false,
            })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn parse_launchctl_line(line: &str) -> Option<LaunchAgentInfo> {
    let mut parts = line.split_whitespace();
    let pid_raw = parts.next()?;
    let status_raw = parts.next()?;
    let label = parts.next()?.to_string();

    if !JARVIS_LAUNCH_AGENTS
        .iter()
        .any(|candidate| *candidate == label)
    {
        return None;
    }

    let pid = pid_raw.parse::<i32>().ok();
    let last_exit_status = status_raw.parse::<i32>().ok();
    Some(LaunchAgentInfo {
        label,
        pid,
        last_exit_status,
        running: pid.is_some_and(|value| value > 0),
    })
}

#[cfg(target_os = "macos")]
fn apply_desktop_process_fallback(statuses: &mut [LaunchAgentInfo]) {
    let Some(pid) = find_desktop_process_pid() else {
        return;
    };
    if let Some(info) = statuses
        .iter_mut()
        .find(|info| info.label == DESKTOP_AGENT_LABEL && !info.running)
    {
        info.pid = Some(pid);
        info.last_exit_status = None;
        info.running = true;
    }
}

#[cfg(target_os = "macos")]
fn find_desktop_process_pid() -> Option<i32> {
    let output = Command::new("pgrep")
        .args(["-f", DESKTOP_PROCESS_PATTERN])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_first_pid(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn parse_first_pid(raw: &str) -> Option<i32> {
    raw.lines()
        .find_map(|line| line.trim().parse::<i32>().ok())
        .filter(|pid| *pid > 0)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{parse_first_pid, parse_launchctl_list, parse_output_volume};

    #[test]
    fn parses_output_volume() {
        assert_eq!(parse_output_volume("37\n").unwrap(), Some(37));
        assert_eq!(parse_output_volume("125\n").unwrap(), Some(100));
    }

    #[test]
    fn parses_launchctl_statuses() {
        let parsed = parse_launchctl_list(
            "80242\t0\tai.zeroclaw.jarvis-daemon\n\
             10115\t0\tai.zeroclaw.local-whisper\n\
             -\t-15\tai.zeroclaw.desktop-dev\n",
        );

        assert!(parsed[0].running);
        assert_eq!(parsed[0].pid, Some(80242));
        assert!(parsed[1].running);
        assert!(!parsed[2].running);
        assert_eq!(parsed[2].last_exit_status, Some(-15));
    }

    #[test]
    fn parses_first_process_pid() {
        assert_eq!(parse_first_pid("123\n456\n"), Some(123));
        assert_eq!(parse_first_pid("-\n456\n"), Some(456));
        assert_eq!(parse_first_pid(""), None);
    }
}
