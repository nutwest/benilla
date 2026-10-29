//! The server's processes: started hidden inside a job object that kills them if the game dies,
//! waited on by their ports, stopped in order, and found again after a crash through the run file.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use bevy::log::{info, warn};

use super::layout::Layout;
use super::setup::Secrets;
use super::{DB_PORT, REALMD_PORT, WORLD_PORT};

/// `CREATE_NO_WINDOW`: a console program started with no console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `BELOW_NORMAL_PRIORITY_CLASS`, for the background navmesh build.
pub const BELOW_NORMAL: u32 = 0x0000_4000;

/// A command that runs hidden, with `extra` creation flags.
pub fn hidden(cmd: &mut Command, extra: u32) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(CREATE_NO_WINDOW | extra);
    }
    #[cfg(not(windows))]
    let _ = (CREATE_NO_WINDOW, extra);
    cmd
}

/// A job object with kill-on-close: when the game's handle goes, so do the processes in it.
pub struct Job(#[cfg(windows)] Option<win::Handle>);

impl Job {
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            let job = win::kill_on_close_job();
            if job.is_none() {
                warn!(
                    "party: no job object; a crash may leave the server running until next launch"
                );
            }
            Self(job)
        }
        #[cfg(not(windows))]
        Self()
    }

    /// Puts `child` in the job.
    pub fn adopt(&self, child: &Child) {
        #[cfg(windows)]
        if let Some(job) = &self.0 {
            win::assign(job, child);
        }
        #[cfg(not(windows))]
        let _ = child;
    }
}

/// Spawns `cmd` in the job.
pub fn spawn(job: &Job, cmd: &mut Command) -> anyhow::Result<Child> {
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {:?}", cmd.get_program()))?;
    job.adopt(&child);
    Ok(child)
}

/// Runs a setup tool to completion, hidden with `flags` added, handing each line of its output
/// (split at `\r` too, which progress bars redraw with) to `on_line`.
pub fn run_tool(
    job: &Job,
    cmd: &mut Command,
    flags: u32,
    cancel: &AtomicBool,
    mut on_line: impl FnMut(&str),
) -> anyhow::Result<()> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    hidden(cmd, flags)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = spawn(job, cmd)?;
    let mut out = child.stdout.take().expect("piped stdout");
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            bail!("cancelled");
        }
        let n = out.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                if !line.is_empty() {
                    on_line(&String::from_utf8_lossy(&line));
                    line.clear();
                }
            } else {
                line.push(b);
            }
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("{name} failed ({status})");
    }
    Ok(())
}

/// Whether something listens on 127.0.0.1:`port`.
fn listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Refuses a port another program already holds.
fn ensure_free(port: u16, what: &str) -> anyhow::Result<()> {
    if listening(port) {
        bail!("port {port}, which the {what} uses, is taken by another program");
    }
    Ok(())
}

/// Waits until `port` answers, failing if `child` exits first or `limit` passes.
fn wait_port(
    port: u16,
    child: &mut Child,
    limit: Duration,
    cancel: &AtomicBool,
    what: &str,
) -> anyhow::Result<()> {
    let start = Instant::now();
    while !listening(port) {
        if let Some(status) = child.try_wait()? {
            bail!("the {what} exited during startup ({status}); see its log in logs/");
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if start.elapsed() > limit {
            bail!(
                "the {what} did not open port {port} within {}s",
                limit.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn log_file(layout: &Layout, name: &str) -> anyhow::Result<std::fs::File> {
    Ok(std::fs::File::create(layout.logs().join(name))?)
}

/// Starts MariaDB on its private port and waits for it.
pub fn start_mariadb(job: &Job, layout: &Layout, cancel: &AtomicBool) -> anyhow::Result<Child> {
    ensure_free(DB_PORT, "database")?;
    let mut cmd = Command::new(layout.mariadb_bin("mariadbd.exe"));
    cmd.arg(format!(
        "--defaults-file={}",
        layout.db().join("my.ini").display()
    ))
    .arg("--console")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(log_file(layout, "mariadb.log")?);
    let mut child = spawn(job, hidden(&mut cmd, 0))?;
    wait_port(
        DB_PORT,
        &mut child,
        Duration::from_secs(120),
        cancel,
        "database",
    )?;
    Ok(child)
}

/// Starts realmd and mangosd together and waits for both ports. mangosd's console is a pipe.
pub fn start_servers(
    job: &Job,
    layout: &Layout,
    cancel: &AtomicBool,
) -> anyhow::Result<(Child, (Child, ChildStdin))> {
    ensure_free(REALMD_PORT, "login server")?;
    ensure_free(WORLD_PORT, "world server")?;
    let mut realmd = Command::new(layout.vmangos_bin("realmd.exe"));
    realmd
        .arg("-c")
        .arg(layout.vmangos().join("realmd.conf"))
        .current_dir(layout.vmangos())
        .stdin(Stdio::null())
        .stdout(log_file(layout, "realmd.out")?)
        .stderr(Stdio::null());
    let mut realmd = spawn(job, hidden(&mut realmd, 0))?;

    let mut mangosd = Command::new(layout.vmangos_bin("mangosd.exe"));
    mangosd
        .arg("-c")
        .arg(layout.vmangos().join("mangosd.conf"))
        .current_dir(layout.vmangos())
        .stdin(Stdio::piped())
        .stdout(log_file(layout, "mangosd.out")?)
        .stderr(Stdio::null());
    let mut mangosd = spawn(job, hidden(&mut mangosd, 0))?;
    let console = mangosd.stdin.take().expect("piped stdin");

    wait_port(
        REALMD_PORT,
        &mut realmd,
        Duration::from_secs(60),
        cancel,
        "login server",
    )?;
    wait_port(
        WORLD_PORT,
        &mut mangosd,
        Duration::from_secs(300),
        cancel,
        "world server",
    )?;
    Ok((realmd, (mangosd, console)))
}

/// The processes of this launch.
pub struct Running {
    mariadb: Child,
    realmd: Option<Child>,
    mangosd: Option<(Child, ChildStdin)>,
}

impl Running {
    pub fn with_db(mariadb: Child) -> Self {
        Self {
            mariadb,
            realmd: None,
            mangosd: None,
        }
    }

    pub fn add_servers(&mut self, realmd: Child, mangosd: (Child, ChildStdin)) {
        self.realmd = Some(realmd);
        self.mangosd = Some(mangosd);
    }

    /// One command on mangosd's console, as typed at its `mangos>` prompt.
    pub fn console(&mut self, line: &str) -> anyhow::Result<()> {
        let (_, stdin) = self
            .mangosd
            .as_mut()
            .context("the world server is not running")?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    fn pids(&self) -> Vec<(&'static str, u32)> {
        let mut out = vec![("mariadbd", self.mariadb.id())];
        if let Some(r) = &self.realmd {
            out.push(("realmd", r.id()));
        }
        if let Some((m, _)) = &self.mangosd {
            out.push(("mangosd", m.id()));
        }
        out
    }

    /// mangosd saves and exits from its console, realmd stops, MariaDB shuts down; each is killed
    /// after its wait.
    pub fn stop(mut self, layout: &Layout, secrets: Option<&Secrets>) {
        if let Some((mut mangosd, mut stdin)) = self.mangosd.take() {
            let _ = writeln!(stdin, "saveall");
            let _ = writeln!(stdin, "server exit");
            let _ = stdin.flush();
            // End of input stops mangosd too (`CliRunnable`), should the command be lost.
            drop(stdin);
            finish(&mut mangosd, Duration::from_secs(30), "mangosd");
        }
        if let Some(mut realmd) = self.realmd.take() {
            let _ = realmd.kill();
            let _ = realmd.wait();
        }
        admin_shutdown(layout, secrets);
        finish(&mut self.mariadb, Duration::from_secs(30), "mariadbd");
    }
}

/// Waits for `child` to exit on its own, then kills it.
fn finish(child: &mut Child, limit: Duration, name: &str) {
    let start = Instant::now();
    while start.elapsed() < limit {
        if let Ok(Some(_)) = child.try_wait() {
            info!(
                "party: {name} stopped after {:.1}s",
                start.elapsed().as_secs_f32()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    warn!(
        "party: {name} did not stop within {}s; killing it",
        limit.as_secs()
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// `mariadb-admin shutdown`: MariaDB flushes and exits.
fn admin_shutdown(layout: &Layout, secrets: Option<&Secrets>) {
    let mut cmd = Command::new(layout.mariadb_bin("mariadb-admin.exe"));
    cmd.args(["--protocol=TCP", "-h127.0.0.1", "-uroot", "--skip-ssl"])
        .arg(format!("-P{DB_PORT}"))
        .arg("shutdown")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(s) = secrets {
        cmd.env("MYSQL_PWD", &s.root_pass);
    }
    let _ = hidden(&mut cmd, 0).status();
}

/// Records this launch's PIDs, so the next launch can find them after a crash.
pub fn write_run_file(layout: &Layout, running: &Running) -> anyhow::Result<()> {
    let body: String = running
        .pids()
        .iter()
        .map(|(name, pid)| format!("{name} {pid}\n"))
        .collect();
    std::fs::write(layout.run_file(), body)?;
    Ok(())
}

pub fn remove_run_file(layout: &Layout) {
    let _ = std::fs::remove_file(layout.run_file());
}

/// After an unclean exit: stops whatever of the last launch's processes still runs (a PID alive
/// and running our binary), then forgets them. Returns whether there was a run file.
pub fn recover_stale(layout: &Layout) -> anyhow::Result<bool> {
    let Ok(body) = std::fs::read_to_string(layout.run_file()) else {
        return Ok(false);
    };
    for line in body.lines() {
        let Some((name, pid)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid.trim().parse::<u32>() else {
            continue;
        };
        let exe = match name {
            "mariadbd" => layout.mariadb_bin("mariadbd.exe"),
            "realmd" => layout.vmangos_bin("realmd.exe"),
            "mangosd" => layout.vmangos_bin("mangosd.exe"),
            _ => continue,
        };
        if stop_if_ours(pid, &exe) {
            warn!("party: stopped a stale {name} (pid {pid}) left by the last session");
        }
    }
    remove_run_file(layout);
    Ok(true)
}

/// Terminates `pid` if it is alive and runs `exe`.
fn stop_if_ours(pid: u32, exe: &Path) -> bool {
    #[cfg(windows)]
    {
        win::terminate_if_image(pid, exe)
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, exe);
        false
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle as _;
    use std::path::Path;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    /// An owned kernel handle.
    pub struct Handle(HANDLE);

    // A job handle is a kernel object reference, usable from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn kill_on_close_job() -> Option<Handle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let job = Handle(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            (ok != 0).then_some(job)
        }
    }

    pub fn assign(job: &Handle, child: &Child) {
        unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
    }

    pub fn terminate_if_image(pid: u32, exe: &Path) -> bool {
        unsafe {
            let h = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if h.is_null() {
                return false;
            }
            let h = Handle(h);
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0
            {
                return false;
            }
            let image = String::from_utf16_lossy(&buf[..len as usize]);
            let same = std::fs::canonicalize(&image).ok() == std::fs::canonicalize(exe).ok();
            if !same {
                return false;
            }
            TerminateProcess(h.0, 1);
            WaitForSingleObject(h.0, 10_000);
            true
        }
    }
}
