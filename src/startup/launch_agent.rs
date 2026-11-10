//! Login startup through a per-user LaunchAgent.
//!
//! launchd loads every plist in `~/Library/LaunchAgents` when the user logs in, and
//! `RunAtLoad` starts the GUI then. Like the Windows `Run` key, installing takes effect at
//! the next login rather than starting anything now.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use super::display_command;
use crate::{constants::LAUNCH_AGENT_LABEL, paths::home_dir};

pub(super) fn install(executable: &Path, arguments: &[&str]) -> Result<(), String> {
    if is_translocated(executable) {
        return Err(
            "macOS is running this copy from a temporary App Translocation path that \
             disappears after it quits. Move HyperX Mic Lite.app to /Applications, open it \
             from there, and install startup again."
                .to_string(),
        );
    }
    let program_arguments = std::iter::once(executable.display().to_string())
        .chain(arguments.iter().map(|argument| argument.to_string()))
        .collect::<Vec<_>>();
    write_agent(&agent_path()?, &program_arguments)
}

/// Remove the LaunchAgent; `false` means there was none.
pub(super) fn uninstall() -> Result<bool, String> {
    remove_agent(&agent_path()?)
}

pub(super) fn installed_command() -> Result<Option<String>, String> {
    let path = agent_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let arguments = read_program_arguments(&path)?;
    let (executable, rest) = arguments
        .split_first()
        .ok_or_else(|| format!("{} has an empty ProgramArguments list", path.display()))?;
    Ok(Some(display_command(executable, rest)))
}

/// Whether Gatekeeper is running a quarantined app from a randomized read-only mount,
/// which a login item must never point at.
fn is_translocated(executable: &Path) -> bool {
    executable
        .components()
        .any(|component| component.as_os_str() == "AppTranslocation")
}

fn agent_path() -> Result<PathBuf, String> {
    let home = home_dir().ok_or("HOME is not set, so ~/Library/LaunchAgents cannot be found")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist")))
}

fn write_agent(path: &Path, program_arguments: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    fs::write(
        path,
        render_agent_plist(LAUNCH_AGENT_LABEL, program_arguments),
    )
    .map_err(|error| format!("{}: {error}", path.display()))
}

fn remove_agent(path: &Path) -> Result<bool, String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Read `ProgramArguments` back with `plutil`, which ships with macOS and also reads the
/// binary plist form in case the file was converted after it was written.
fn read_program_arguments(path: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "ProgramArguments", "json", "-o", "-"])
        .arg(path)
        .output()
        .map_err(|error| format!("plutil: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{}: ProgramArguments is not a list of strings: {error}",
            path.display()
        )
    })
}

fn render_agent_plist(label: &str, program_arguments: &[String]) -> String {
    let arguments = program_arguments
        .iter()
        .map(|argument| format!("        <string>{}</string>\n", xml_escape(argument)))
        .collect::<String>();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#,
        label = xml_escape(label)
    )
}

/// Escape text for an XML element body. `&` goes first so the entities added for `<` and
/// `>` are not escaped a second time.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh directory under the system temp dir, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "hyperx-launch-agent-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn plist_runs_the_program_at_login() {
        let plist = render_agent_plist("local.test", &arguments(&["/bin/app", "gui"]));
        assert!(plist.contains("<key>Label</key>\n    <string>local.test</string>"));
        assert!(plist.contains("<string>/bin/app</string>\n        <string>gui</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    }

    #[test]
    fn translocated_copies_are_recognised() {
        let translocated = Path::new(
            "/private/var/folders/x/T/AppTranslocation/ABCD-1234/d/HyperX Mic Lite.app/\
             Contents/MacOS/hyperx-mic-lite",
        );
        let installed =
            Path::new("/Applications/HyperX Mic Lite.app/Contents/MacOS/hyperx-mic-lite");

        assert!(is_translocated(translocated));
        assert!(!is_translocated(installed));
    }

    #[test]
    fn markup_characters_in_arguments_are_escaped() {
        assert_eq!(xml_escape("a&b<c>d"), "a&amp;b&lt;c&gt;d");
        let plist = render_agent_plist("local.test", &arguments(&["/Apps/R&D <x>/app"]));
        assert!(plist.contains("<string>/Apps/R&amp;D &lt;x&gt;/app</string>"));
    }

    #[test]
    fn written_agent_reads_back_with_the_same_arguments() {
        let dir = TempDir::new();
        let path = dir.0.join("LaunchAgents").join("agent.plist");
        let written = arguments(&[
            "/Applications/HyperX Mic Lite.app/Contents/MacOS/hyperx-mic-lite",
            "gui",
            "--start-minimized",
        ]);

        write_agent(&path, &written).expect("write agent");

        assert_eq!(read_program_arguments(&path).expect("read agent"), written);
    }

    #[test]
    fn removing_reports_whether_an_agent_existed() {
        let dir = TempDir::new();
        let path = dir.0.join("agent.plist");
        write_agent(&path, &arguments(&["/bin/app"])).expect("write agent");

        assert_eq!(remove_agent(&path), Ok(true));
        assert_eq!(remove_agent(&path), Ok(false));
    }
}
