//! macOS LaunchAgent control for the local Jarvis desktop runtime.

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BACKGROUND_LABELS: &[&str] = &["ai.zeroclaw.jarvis-daemon", "ai.zeroclaw.local-whisper"];
const DESKTOP_LABEL: &str = "ai.zeroclaw.desktop-dev";
const LOCAL_TTS_PROCESS_PATTERN: &str = "yuni-gpt-sovits/api.py";

/// Bring up the local background services when the user launches the desktop app.
pub fn start_background_services() {
    let Some(uid) = current_uid() else {
        return;
    };
    let Some(home) = home_dir() else {
        return;
    };

    for label in BACKGROUND_LABELS {
        start_launch_agent(&uid, &home, label);
    }
}

/// Stop every local process that keeps Jarvis listening in the background.
pub fn quit_completely() {
    let Some(uid) = current_uid() else {
        kill_local_tts();
        return;
    };

    for label in BACKGROUND_LABELS {
        stop_launch_agent(&uid, label);
    }
    kill_local_tts();
    schedule_desktop_launch_agent_bootout(&uid);
}

fn start_launch_agent(uid: &str, home: &Path, label: &str) {
    if is_launch_agent_running(label) {
        return;
    }

    let plist = launch_agent_plist_path(home, label);
    if !plist.is_file() {
        return;
    }

    let domain = launchctl_domain(uid);
    let _ = run_launchctl(&[
        "bootstrap",
        domain.as_str(),
        plist.to_string_lossy().as_ref(),
    ]);
    if !is_launch_agent_running(label) {
        let target = launchctl_target(uid, label);
        let _ = run_launchctl(&["kickstart", target.as_str()]);
    }
}

fn stop_launch_agent(uid: &str, label: &str) {
    let target = launchctl_target(uid, label);
    let _ = run_launchctl(&["bootout", target.as_str()]);
}

fn kill_local_tts() {
    let _ = Command::new("/usr/bin/pkill")
        .args(["-f", LOCAL_TTS_PROCESS_PATTERN])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn schedule_desktop_launch_agent_bootout(uid: &str) {
    let target = launchctl_target(uid, DESKTOP_LABEL);
    let script = format!("sleep 0.35; /bin/launchctl bootout {target} >/dev/null 2>&1");
    let _ = Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn is_launch_agent_running(label: &str) -> bool {
    let Ok(output) = Command::new("/bin/launchctl").arg("list").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    parse_launchctl_running(&String::from_utf8_lossy(&output.stdout), label)
}

fn run_launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn current_uid() -> Option<String> {
    let output = Command::new("/usr/bin/id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!uid.is_empty()).then_some(uid)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn launchctl_domain(uid: &str) -> String {
    format!("gui/{uid}")
}

fn launchctl_target(uid: &str, label: &str) -> String {
    format!("{}/{}", launchctl_domain(uid), label)
}

fn launch_agent_plist_path(home: &Path, label: &str) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

fn parse_launchctl_running(raw: &str, label: &str) -> bool {
    raw.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next() else {
            return false;
        };
        let _status = parts.next();
        let Some(candidate) = parts.next() else {
            return false;
        };
        candidate == label && pid.parse::<i32>().is_ok_and(|value| value > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        launch_agent_plist_path, launchctl_domain, launchctl_target, parse_launchctl_running,
    };
    use std::path::Path;

    #[test]
    fn builds_launchctl_domain_and_target() {
        assert_eq!(launchctl_domain("501"), "gui/501");
        assert_eq!(
            launchctl_target("501", "ai.zeroclaw.jarvis-daemon"),
            "gui/501/ai.zeroclaw.jarvis-daemon"
        );
    }

    #[test]
    fn builds_launch_agent_plist_path() {
        assert_eq!(
            launch_agent_plist_path(Path::new("/Users/test"), "ai.zeroclaw.local-whisper"),
            Path::new("/Users/test/Library/LaunchAgents/ai.zeroclaw.local-whisper.plist")
        );
    }

    #[test]
    fn parses_running_launch_agent() {
        let raw = "50597\t0\tai.zeroclaw.jarvis-daemon\n\
                   -\t-15\tai.zeroclaw.desktop-dev\n";
        assert!(parse_launchctl_running(raw, "ai.zeroclaw.jarvis-daemon"));
        assert!(!parse_launchctl_running(raw, "ai.zeroclaw.desktop-dev"));
        assert!(!parse_launchctl_running(raw, "ai.zeroclaw.local-whisper"));
    }
}
