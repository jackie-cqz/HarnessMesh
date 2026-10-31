//! Observed descendants are bound to PID + kernel start time, never process names.
//! Fast unobserved double-forks and daemon hard-crash containment remain OS limits.
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::process::{Child, Command};
#[derive(Clone, Copy, Debug)]
struct Identity {
    pid: i32,
    parent: i32,
    start: u128,
    alive: bool,
}
fn same(a: Identity, b: Identity) -> bool {
    a.pid == b.pid && a.start == b.start
}
#[cfg(target_os = "macos")]
fn identity(pid: i32) -> Result<Option<Identity>> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let len = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut _,
            std::mem::size_of_val(&info) as i32,
        )
    };
    if len == 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) || e.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(e.into());
    }
    if len as usize != std::mem::size_of_val(&info) {
        bail!("incomplete owned process identity");
    }
    Ok(Some(Identity {
        pid,
        parent: info.pbi_ppid as i32,
        start: ((info.pbi_start_tvsec as u128) << 64) | info.pbi_start_tvusec as u128,
        alive: info.pbi_status != libc::SZOMB,
    }))
}
#[cfg(target_os = "macos")]
fn children(pid: i32) -> Result<Vec<i32>> {
    let mut pids = vec![0i32; 4096];
    let count = unsafe {
        libc::proc_listchildpids(
            pid,
            pids.as_mut_ptr() as *mut _,
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if count < 0 || count as usize >= pids.len() {
        bail!("owned child enumeration unavailable or truncated");
    }
    pids.truncate(count as usize);
    Ok(pids)
}
#[cfg(target_os = "linux")]
fn identity(pid: i32) -> Result<Option<Identity>> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let fields: Vec<_> = text
        .rsplit_once(") ")
        .context("invalid owned process identity")?
        .1
        .split_whitespace()
        .collect();
    if fields.len() < 20 {
        bail!("incomplete owned process identity");
    }
    Ok(Some(Identity {
        pid,
        parent: fields[1].parse()?,
        start: fields[19].parse()?,
        alive: fields[0] != "Z" && fields[0] != "X",
    }))
}
#[cfg(target_os = "linux")]
fn children(pid: i32) -> Result<Vec<i32>> {
    let threads = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut pids = vec![];
    for thread in threads {
        let file = thread?.path().join("children");
        match std::fs::read_to_string(file) {
            Ok(s) => {
                for id in s.split_whitespace() {
                    pids.push(id.parse()?);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    pids.sort_unstable();
    pids.dedup();
    if pids.len() > 4096 {
        bail!("too many owned descendants");
    }
    Ok(pids)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn identity(_pid: i32) -> Result<Option<Identity>> {
    bail!("process identity tracking unsupported on this Unix platform")
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn children(_pid: i32) -> Result<Vec<i32>> {
    bail!("child tracking unsupported on this Unix platform")
}
fn signal_owned(expected: Identity) -> Result<()> {
    if !identity(expected.pid)?.is_some_and(|current| same(current, expected) && current.alive) {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        // A pidfd pins this process even if the numeric PID is recycled afterwards.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) } as i32;
        if fd >= 0 {
            let matched = identity(expected.pid)?.is_some_and(|current| same(current, expected));
            let result = if matched {
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        fd,
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                }
            } else {
                0
            };
            unsafe { libc::close(fd) };
            if result < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                bail!("owned pidfd termination failed");
            }
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        // Do not fall back to an unpinned kill on old/restricted kernels.
        Err(error.into())
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Darwin has no pidfd equivalent; recheck the kernel birth time immediately.
        if identity(expected.pid)?.is_some_and(|current| same(current, expected) && current.alive)
            && unsafe { libc::kill(expected.pid, libc::SIGKILL) } != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            bail!("owned descendant termination failed");
        }
        Ok(())
    }
}
pub struct ProcessTree {
    group: i32,
    known: Mutex<BTreeMap<i32, Identity>>,
    finished: AtomicBool,
}
impl ProcessTree {
    pub fn configure(command: &mut Command) {
        command.process_group(0);
    }
    pub fn attach(child: &Child) -> Result<Self> {
        let pid = i32::try_from(child.id().context("child has no pid")?)?;
        if pid <= 1 {
            bail!("invalid managed process group");
        }
        let root = identity(pid)?.context("managed root exited before identity tracking")?;
        Ok(Self {
            group: pid,
            known: Mutex::new(BTreeMap::from([(pid, root)])),
            finished: AtomicBool::new(false),
        })
    }
    pub fn refresh(&self) -> Result<()> {
        if self.finished.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut known = self
            .known
            .lock()
            .map_err(|_| anyhow::anyhow!("process tracking poisoned"))?;
        let mut pending: Vec<_> = known.values().copied().collect();
        let mut visited = std::collections::BTreeSet::new();
        while let Some(parent) = pending.pop() {
            if !visited.insert(parent.pid) {
                continue;
            }
            if !identity(parent.pid)?.is_some_and(|p| same(p, parent) && p.alive) {
                continue;
            }
            for pid in children(parent.pid)? {
                if let Some(child) = identity(pid)?
                    && child.parent == parent.pid
                {
                    if !identity(parent.pid)?.is_some_and(|p| same(p, parent)) {
                        bail!("owned parent identity changed during enumeration");
                    }
                    known
                        .entry(pid)
                        .and_modify(|old| {
                            if !same(*old, child) {
                                *old = child;
                            }
                        })
                        .or_insert(child);
                    pending.push(child);
                    if known.len() > 4096 {
                        bail!("owned descendant budget exceeded");
                    }
                }
            }
        }
        Ok(())
    }
    pub fn terminate(&self) -> Result<()> {
        if self.finished.load(Ordering::SeqCst) {
            return Ok(());
        }
        let refreshed = self.refresh();
        let known = self
            .known
            .lock()
            .map_err(|_| anyhow::anyhow!("process tracking poisoned"))?;
        // Only signal identity-matched observed processes. Recycled group IDs are never killed.
        for expected in known.values().rev() {
            signal_owned(*expected)?;
        }
        refreshed
    }
    pub fn stopped(&self) -> Result<bool> {
        let known = self
            .known
            .lock()
            .map_err(|_| anyhow::anyhow!("process tracking poisoned"))?;
        for expected in known.values() {
            if identity(expected.pid)?.is_some_and(|p| same(p, *expected) && p.alive) {
                return Ok(false);
            }
        }
        if unsafe { libc::kill(-self.group, 0) } == 0 {
            return Ok(false);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            bail!("owned group status unconfirmed");
        }
        self.finished.store(true, Ordering::SeqCst);
        Ok(true)
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
