//! This computer's own folder dialog, shown for the panel. The panel listens
//! only on loopback, so the browser asking for it is on this computer too.
//! The daemon never shows one: it may run in the background and also serves
//! paired computers.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A dialog nobody answers is closed after this long.
pub const DIALOG_LIMIT: Duration = Duration::from_secs(600);
/// Replaces the system dialog: a program that prints the chosen folder (and
/// nothing when cancelled). For tests, or another dialog tool.
pub const OVERRIDE_VAR: &str = "HMESH_FOLDER_PICKER";
const PROMPT: &str = "选择项目文件夹";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mac,
    Windows,
    Zenity,
    Kdialog,
    Custom,
}

/// The command that shows the dialog, starting in `start` when given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub kind: Kind,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment the program reads (the start folder on Windows).
    pub env: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PickError {
    /// No dialog can be shown here (no tool, or no desktop session).
    Unavailable,
    TimedOut,
    Failed(String),
}

/// What this computer offers, if anything.
pub fn picker(start: Option<&Path>) -> Option<Picker> {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let on_path = |name: &str| find_on_path(name, env("PATH").as_deref());
    let custom = env(OVERRIDE_VAR).map(PathBuf::from);
    let desktop = env("DISPLAY").is_some() || env("WAYLAND_DISPLAY").is_some();
    choose(
        std::env::consts::OS,
        custom,
        desktop,
        &on_path,
        start.filter(|p| p.is_absolute() && p.is_dir()),
    )
}

fn choose(
    os: &str,
    custom: Option<PathBuf>,
    desktop: bool,
    on_path: &dyn Fn(&str) -> Option<PathBuf>,
    start: Option<&Path>,
) -> Option<Picker> {
    let start_text = start.map(|p| p.to_string_lossy().into_owned());
    if let Some(program) = custom.filter(|p| p.is_absolute()) {
        return Some(Picker {
            kind: Kind::Custom,
            program,
            args: start_text.into_iter().collect(),
            env: vec![],
        });
    }
    match os {
        "macos" => {
            let mut args = vec![];
            for line in [
                "on run argv",
                "activate",
                "if (count of argv) > 0 then",
                &format!(
                    "return POSIX path of (choose folder with prompt \"{PROMPT}\" default location (POSIX file (item 1 of argv)))"
                ),
                "end if",
                &format!("return POSIX path of (choose folder with prompt \"{PROMPT}\")"),
                "end run",
            ] {
                args.push("-e".to_owned());
                args.push(line.to_owned());
            }
            // The start folder travels as an argument, never inside the script.
            args.extend(start_text);
            Some(Picker {
                kind: Kind::Mac,
                program: PathBuf::from("/usr/bin/osascript"),
                args,
                env: vec![],
            })
        }
        "windows" => {
            let program = on_path("powershell.exe").or_else(|| {
                std::env::var_os("SystemRoot").map(|root| {
                    PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                })
            })?;
            let script = [
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8",
                "Add-Type -AssemblyName System.Windows.Forms",
                "$owner = New-Object System.Windows.Forms.Form",
                "$owner.TopMost = $true",
                "$d = New-Object System.Windows.Forms.FolderBrowserDialog",
                &format!("$d.Description = '{PROMPT}'"),
                "$d.ShowNewFolderButton = $true",
                "if ($env:HMESH_PICK_START) { $d.SelectedPath = $env:HMESH_PICK_START }",
                "if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.SelectedPath) }",
            ]
            .join("; ");
            Some(Picker {
                kind: Kind::Windows,
                program,
                args: ["-NoProfile", "-NonInteractive", "-STA", "-Command", &script]
                    .map(str::to_owned)
                    .to_vec(),
                env: start_text
                    .map(|s| vec![("HMESH_PICK_START".to_owned(), s)])
                    .unwrap_or_default(),
            })
        }
        _ if desktop => {
            if let Some(program) = on_path("zenity") {
                let mut args = vec![
                    "--file-selection".to_owned(),
                    "--directory".to_owned(),
                    format!("--title={PROMPT}"),
                ];
                if let Some(start) = start_text {
                    args.push(format!("--filename={}/", start.trim_end_matches('/')));
                }
                return Some(Picker {
                    kind: Kind::Zenity,
                    program,
                    args,
                    env: vec![],
                });
            }
            let program = on_path("kdialog")?;
            Some(Picker {
                kind: Kind::Kdialog,
                program,
                args: vec![
                    "--title".to_owned(),
                    PROMPT.to_owned(),
                    "--getexistingdirectory".to_owned(),
                    start_text.unwrap_or_else(|| "~".to_owned()),
                ],
                env: vec![],
            })
        }
        _ => None,
    }
}

fn find_on_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The folder chosen, `None` when the dialog was cancelled.
pub fn chosen(
    kind: Kind,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<Option<PathBuf>, PickError> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| PickError::Failed("the dialog returned a path that is not UTF-8".into()))?;
    let text = text.trim_end_matches(['\r', '\n']);
    let problem = || {
        let said = String::from_utf8_lossy(stderr);
        let said = said.trim();
        PickError::Failed(if said.is_empty() {
            match code {
                Some(code) => format!("the folder dialog exited with code {code}"),
                None => "the folder dialog was stopped".to_owned(),
            }
        } else {
            said.chars().take(300).collect()
        })
    };
    match code {
        Some(0) if text.is_empty() => Ok(None),
        Some(0) => {
            // macOS ends a folder with a slash; keep the root as it is.
            let trimmed = text.trim_end_matches(['/', '\\']);
            let path = PathBuf::from(if trimmed.is_empty() || trimmed.ends_with(':') {
                text
            } else {
                trimmed
            });
            if path.is_absolute() {
                Ok(Some(path))
            } else {
                Err(PickError::Failed(
                    "the dialog did not return a full path".into(),
                ))
            }
        }
        // osascript reports "User canceled. (-128)".
        Some(1) if kind == Kind::Mac => {
            if String::from_utf8_lossy(stderr).contains("-128") {
                Ok(None)
            } else {
                Err(problem())
            }
        }
        Some(1) if matches!(kind, Kind::Zenity | Kind::Kdialog) => Ok(None),
        _ => Err(problem()),
    }
}

/// The open dialog. However `pick` ends (answered, timed out, or dropped
/// because the browser stopped waiting), the dialog is killed and waited
/// for, so it neither keeps running nor lingers as a zombie of the panel.
struct Dialog(Option<tokio::process::Child>);
impl Drop for Dialog {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = child.start_kill();
        // Without a runtime to wait on, kill_on_drop and tokio's own reaping
        // are left to it.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

/// Everything the dialog writes to one of its outputs.
async fn read_all(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

/// Show the dialog and wait for an answer (at most `DIALOG_LIMIT`).
pub async fn pick(start: Option<&Path>) -> Result<Option<PathBuf>, PickError> {
    let picker = picker(start).ok_or(PickError::Unavailable)?;
    let mut command = tokio::process::Command::new(&picker.program);
    command
        .args(&picker.args)
        .envs(picker.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| PickError::Failed(format!("could not show the folder dialog: {e}")))?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let mut dialog = Dialog(Some(child));
    let child = dialog.0.as_mut().expect("the dialog was just started");
    let answer = async { tokio::try_join!(child.wait(), read_all(stdout), read_all(stderr)) };
    let (status, stdout, stderr) = match tokio::time::timeout(DIALOG_LIMIT, answer).await {
        Ok(answer) => answer.map_err(|e| PickError::Failed(e.to_string()))?,
        // Dropping the dialog kills it.
        Err(_) => return Err(PickError::TimedOut),
    };
    chosen(picker.kind, status.code(), &stdout, &stderr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on this platform, from a Unix-style one.
    fn abs(unix: &str) -> String {
        if cfg!(windows) {
            format!("C:{}", unix.replace('/', "\\"))
        } else {
            unix.to_owned()
        }
    }
    fn tools(names: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            names
                .contains(&name)
                .then(|| PathBuf::from(format!("/usr/bin/{name}")))
        }
    }

    #[test]
    fn each_system_shows_its_own_dialog() {
        let none = tools(&[]);
        let start = Path::new("/Users/me/code");
        let mac = choose("macos", None, false, &none, Some(start)).unwrap();
        assert_eq!(mac.kind, Kind::Mac);
        assert_eq!(mac.program, PathBuf::from("/usr/bin/osascript"));
        // The folder is an argument after the script, not part of it.
        assert_eq!(mac.args.last().unwrap(), "/Users/me/code");
        assert_eq!(
            mac.args.iter().filter(|a| a.contains("/Users/me")).count(),
            1
        );
        assert!(mac.args.iter().any(|a| a.contains("choose folder")));
        let plain = choose("macos", None, false, &none, None).unwrap();
        assert_eq!(plain.args.len() % 2, 0, "only -e pairs without a start");

        let win = choose(
            "windows",
            None,
            false,
            &tools(&["powershell.exe"]),
            Some(start),
        )
        .unwrap();
        assert_eq!(win.kind, Kind::Windows);
        assert!(win.args.contains(&"-STA".to_owned()));
        assert!(win.args.last().unwrap().contains("FolderBrowserDialog"));
        assert!(!win.args.last().unwrap().contains("/Users/me"));
        assert_eq!(
            win.env,
            [("HMESH_PICK_START".to_owned(), "/Users/me/code".to_owned())]
        );

        let zenity = choose(
            "linux",
            None,
            true,
            &tools(&["zenity", "kdialog"]),
            Some(start),
        )
        .unwrap();
        assert_eq!(zenity.kind, Kind::Zenity);
        assert!(zenity.args.contains(&"--directory".to_owned()));
        assert!(
            zenity
                .args
                .contains(&"--filename=/Users/me/code/".to_owned())
        );
        let kdialog = choose("linux", None, true, &tools(&["kdialog"]), None).unwrap();
        assert_eq!(kdialog.kind, Kind::Kdialog);
        assert_eq!(kdialog.args.last().unwrap(), "~");
        // No desktop session, or no tool: no dialog.
        assert_eq!(
            choose("linux", None, false, &tools(&["zenity"]), None),
            None
        );
        assert_eq!(choose("linux", None, true, &none, None), None);
        assert_eq!(choose("freebsd", None, false, &none, None), None);
    }

    #[test]
    fn a_configured_program_replaces_the_dialog_when_given_in_full() {
        let none = tools(&[]);
        let program = PathBuf::from(abs("/opt/pick"));
        let custom = choose(
            "linux",
            Some(program.clone()),
            false,
            &none,
            Some(Path::new("/srv")),
        )
        .unwrap();
        assert_eq!(custom.kind, Kind::Custom);
        assert_eq!(custom.program, program);
        assert_eq!(custom.args, ["/srv"]);
        // A relative program is ignored rather than looked up.
        let ignored = choose("macos", Some(PathBuf::from("pick")), false, &none, None).unwrap();
        assert_eq!(ignored.kind, Kind::Mac);
    }

    #[test]
    fn answers_are_read_as_a_folder_or_a_cancel() {
        let chose =
            |kind, code, out: &str, err: &str| chosen(kind, code, out.as_bytes(), err.as_bytes());
        let code = abs("/Users/me/code");
        assert_eq!(
            chose(Kind::Mac, Some(0), &format!("{code}/\n"), ""),
            Ok(Some(PathBuf::from(&code)))
        );
        assert_eq!(
            chose(Kind::Windows, Some(0), &format!("{code}\r\n"), ""),
            Ok(Some(PathBuf::from(&code)))
        );
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        assert_eq!(
            chose(Kind::Mac, Some(0), root, ""),
            Ok(Some(PathBuf::from(root)))
        );
        assert_eq!(
            chose(
                Kind::Mac,
                Some(1),
                "",
                "execution error: User canceled. (-128)"
            ),
            Ok(None)
        );
        assert!(matches!(
            chose(Kind::Mac, Some(1), "", "execution error: Not authorized (-1743)"),
            Err(PickError::Failed(m)) if m.contains("-1743")
        ));
        assert_eq!(chose(Kind::Zenity, Some(1), "", ""), Ok(None));
        assert_eq!(chose(Kind::Kdialog, Some(1), "", ""), Ok(None));
        assert_eq!(chose(Kind::Windows, Some(0), "", ""), Ok(None));
        let named = abs("/home/me/项目");
        assert_eq!(
            chose(Kind::Zenity, Some(0), &format!("{named}\n"), ""),
            Ok(Some(PathBuf::from(&named)))
        );
        assert!(matches!(
            chose(Kind::Custom, Some(0), "relative/dir\n", ""),
            Err(PickError::Failed(_))
        ));
        assert!(
            matches!(chose(Kind::Custom, Some(2), "", "boom"), Err(PickError::Failed(m)) if m == "boom")
        );
        assert!(matches!(
            chose(Kind::Zenity, None, "", ""),
            Err(PickError::Failed(_))
        ));
        assert!(matches!(
            chosen(Kind::Custom, Some(0), &[0xff, 0xfe], b""),
            Err(PickError::Failed(_))
        ));
    }
}
