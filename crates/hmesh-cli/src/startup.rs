//! Explicit owner-managed login registration. Plan/status never install or start anything.
use crate::StartupCommand;
use anyhow::{Context, Result, bail};
use hmesh_security::{Instance, atomic_write, file_sha256};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn safe(value: &str) -> Result<()> {
    if value.contains(['\r', '\n', '\0', '"']) {
        bail!("startup paths contain unsupported characters");
    }
    Ok(())
}
fn mac_config(label: &str, binary: &str, node: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>--data-dir</string><string>{}</string><string>daemon</string><string>start</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>",
        xml(label),
        xml(binary),
        xml(node)
    )
}
fn windows_config(sid: &str, binary: &str, node: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers><Principals><Principal id=\"Owner\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Enabled>true</Enabled></Settings><Actions Context=\"Owner\"><Exec><Command>{}</Command><Arguments>--data-dir &quot;{}&quot; daemon start</Arguments></Exec></Actions></Task>",
        xml(sid),
        xml(sid),
        xml(binary),
        xml(node)
    )
}
fn windows_bytes(configuration: &str) -> Vec<u8> {
    std::iter::once(0xfeffu16)
        .chain(configuration.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}
fn system32(name: &str) -> Result<std::path::PathBuf> {
    Ok(
        std::path::PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot unavailable")?)
            .join("System32")
            .join(name),
    )
}
fn task_command() -> Result<Command> {
    Ok(Command::new(system32("schtasks.exe")?))
}
// Use regular Win32 paths for scheduled-task XML and command arguments.
fn native_argument(path: &Path) -> Result<String> {
    let value = path.to_str().context("startup paths must be UTF-8")?;
    #[cfg(windows)]
    {
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return Ok(format!(r"\\{rest}"));
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return Ok(rest.to_owned());
        }
    }
    Ok(value.to_owned())
}
pub fn execute(node: &Path, command: &StartupCommand) -> Result<Value> {
    let instance = Instance::load(node)?;
    let executable = std::env::current_exe()?;
    let executable_argument = native_argument(&executable)?;
    let node_argument = native_argument(&instance.dir)?;
    let binary = executable_argument.as_str();
    let node = node_argument.as_str();
    safe(binary)?;
    safe(node)?;
    let name = format!("dev.harnessmesh.{}", instance.card.node_id);
    let marker = instance.dir.join("startup.json");
    let existing = if marker.exists() {
        Some(serde_json::from_slice::<Value>(&fs::read(&marker)?)?)
    } else {
        None
    };
    if matches!(command, StartupCommand::Status) {
        let installed = if let Some(record) = &existing {
            if record["name"] != name || record["node_id"] != instance.card.node_id.to_string() {
                bail!("startup registration identity mismatch");
            }
            if cfg!(windows) {
                task_command()?
                    .args(["/Query", "/TN", &name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
            } else {
                record["path"].as_str().is_some_and(|p| {
                    file_sha256(Path::new(p), 65536).ok().as_deref() == record["sha256"].as_str()
                })
            }
        } else {
            false
        };
        return Ok(
            json!({"installed":installed,"record_exists":existing.is_some(),"registration":existing.map(|r|json!({"name":r["name"],"node_id":r["node_id"],"path":r["path"],"binary":r["binary"],"sha256":r["sha256"],"starts":r["starts"]})),"daemon_checked":false}),
        );
    }
    if matches!(command, StartupCommand::Uninstall) {
        let record = existing.context("this Node has no managed startup registration")?;
        if record["name"] != name || record["node_id"] != instance.card.node_id.to_string() {
            bail!("startup registration identity mismatch");
        }
        match std::env::consts::OS {
            "macos" => {
                let path = std::path::PathBuf::from(
                    record["path"]
                        .as_str()
                        .context("invalid registration path")?,
                );
                let expected =
                    std::path::PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?)
                        .join("Library/LaunchAgents")
                        .join(format!("{name}.plist"));
                if path != expected
                    || file_sha256(&path, 65536)?
                        != record["sha256"]
                            .as_str()
                            .context("missing registration digest")?
                {
                    bail!("startup file was changed; inspect it before removal");
                }
                let uid = Command::new("/usr/bin/id").arg("-u").output()?;
                if !uid.status.success() {
                    bail!("cannot determine login session");
                }
                let target = format!("gui/{}/{}", String::from_utf8(uid.stdout)?.trim(), name);
                if Command::new("/bin/launchctl")
                    .args(["print", &target])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
                    && !Command::new("/bin/launchctl")
                        .args(["bootout", &target])
                        .status()?
                        .success()
                {
                    bail!("could not unload owned login job");
                }
                fs::remove_file(path)?;
            }
            "windows" => {
                let path = instance.dir.join("login-task.xml");
                if record["path"] != path.to_string_lossy().as_ref()
                    || file_sha256(&path, 65536)?
                        != record["sha256"]
                            .as_str()
                            .context("missing registration digest")?
                {
                    bail!("owned login task configuration changed; inspect before removal");
                }
                if !task_command()?
                    .args(["/Delete", "/TN", &name, "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
                {
                    bail!("could not remove owned login task; registration retained");
                }
                fs::remove_file(path)?;
            }
            _ => bail!("login registration is supported on Mac and Windows"),
        }
        fs::remove_file(marker)?;
        return Ok(json!({"installed":false,"name":name}));
    }
    let (path, config) = match std::env::consts::OS {
        "macos" => (
            std::path::PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?)
                .join("Library/LaunchAgents")
                .join(format!("{name}.plist")),
            mac_config(&name, binary, node),
        ),
        "windows" => {
            let sid = Command::new(system32("whoami.exe")?)
                .args(["/user", "/fo", "csv", "/nh"])
                .output()?;
            if !sid.status.success() {
                bail!("cannot determine owner SID");
            }
            let value = String::from_utf8(sid.stdout)?;
            let sid = value
                .trim()
                .rsplit(',')
                .next()
                .context("missing SID")?
                .trim_matches('"');
            if !sid.starts_with("S-1-")
                || !sid
                    .bytes()
                    .all(|c| c.is_ascii_digit() || c == b'S' || c == b'-')
            {
                bail!("invalid owner SID");
            }
            (
                instance.dir.join("login-task.xml"),
                windows_config(sid, binary, node),
            )
        }
        _ => bail!("login registration is supported on Mac and Windows"),
    };
    if matches!(command, StartupCommand::Plan) {
        return Ok(
            json!({"name":name,"path":path,"configuration":config,"runs":"at next owner login","daemon_only":true,"model_calls":0}),
        );
    }
    if existing.is_some() || path.exists() {
        bail!(
            "startup registration already exists; uninstall its owned registration before changing binaries"
        );
    }
    fs::create_dir_all(path.parent().context("registration parent unavailable")?)?;
    // create_new prevents overwriting any existing login registration.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    use std::io::Write;
    if cfg!(windows) {
        file.write_all(&windows_bytes(&config))?;
    } else {
        file.write_all(config.as_bytes())?;
    }
    file.sync_all()?;
    drop(file);
    if cfg!(windows) {
        let output = task_command()?
            .args(["/Create", "/TN", &name, "/XML"])
            .arg(native_argument(&path)?)
            .output()?;
        if !output.status.success() {
            let _ = fs::remove_file(&path);
            let diagnostic = if output.stderr.is_empty() {
                &output.stdout
            } else {
                &output.stderr
            };
            let message: String = String::from_utf8_lossy(diagnostic)
                .chars()
                .take(1000)
                .collect();
            bail!(
                "login task registration failed ({}); no existing task was overwritten: {message}",
                output.status
            );
        }
    }
    let record = json!({"name":name,"node_id":instance.card.node_id,"path":path,"binary":executable,"sha256":file_sha256(&path,65536)?,"starts":"next owner login"});
    if let Err(error) = atomic_write(&marker, &serde_json::to_vec_pretty(&record)?) {
        if cfg!(windows) {
            let _ = task_command()?
                .args(["/Delete", "/TN", &name, "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(json!({"installed":true,"registration":record,"started_now":false}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_escapes_paths_and_uses_owner_login_without_a_shell() {
        let mac = mac_config("node", "/Applications/A & B/hmesh", "/Users/a/Node <one>");
        assert!(mac.contains("A &amp; B"));
        assert!(mac.contains("Node &lt;one&gt;"));
        assert!(!mac.contains("/bin/sh"));
        assert!(mac.contains("<key>KeepAlive</key><false/>"));
        let win = windows_config("S-1-5-21", "C:\\A & B\\hmesh.exe", "C:\\Node one");
        assert!(win.contains("InteractiveToken"));
        assert!(win.contains("LeastPrivilege"));
        assert!(win.contains("&quot;C:\\Node one&quot;"));
        assert!(!win.contains("Password"));
        let bytes = windows_bytes(&windows_config(
            "S-1-5-21",
            "C:\\fixture\\hmesh.exe",
            "C:\\项目",
        ));
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        let decoded = String::from_utf16(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(decoded.contains("encoding=\"UTF-16\""));
        assert!(decoded.contains("项目"));
        assert!(safe("bad\npath").is_err());
        #[cfg(windows)]
        {
            assert_eq!(
                native_argument(Path::new(r"\\?\C:\Program Files\hmesh.exe")).unwrap(),
                r"C:\Program Files\hmesh.exe"
            );
            assert_eq!(
                native_argument(Path::new(r"\\?\UNC\server\share\hmesh.exe")).unwrap(),
                r"\\server\share\hmesh.exe"
            );
        }
    }
}
