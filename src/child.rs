//! Owned subprocesses with bounded pipe IO and cleanup of their process group.
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn nonblocking(fd: &impl AsRawFd) -> std::io::Result<()> {
    let fd = fd.as_raw_fd();
    // SAFETY: fcntl only changes flags on this live, owned descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) struct OwnedChild(pub Child);
impl OwnedChild {
    pub fn spawn(cmd: &mut Command) -> Result<Self, String> {
        if crate::runtime::closing() {
            return Err("process launch cancelled by shutdown".into());
        }
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: pre_exec uses only async-signal-safe syscalls. The parent check
        // closes the race where it exits before the death signal is installed.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setpgid(0, 0) < 0 || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::other("parent exited during spawn"));
                }
                Ok(())
            });
        }
        cmd.spawn()
            .map(Self)
            .map_err(|e| format!("spawn process: {e}"))
    }
    /// Observe completion without reaping the group leader. Its PID remains
    /// reserved until Drop has killed any descendants and waited for it.
    pub fn try_status(&self) -> std::io::Result<Option<ExitStatus>> {
        // SAFETY: waitid initializes siginfo, and WNOWAIT leaves the child owned.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.0.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        let raw = if info.si_code == libc::CLD_EXITED {
            status << 8
        } else {
            status
                | if info.si_code == libc::CLD_DUMPED {
                    0x80
                } else {
                    0
                }
        };
        Ok(Some(ExitStatus::from_raw(raw)))
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // SAFETY: the group leader has not been reaped, so its numeric PID
        // cannot be reused before group cleanup completes.
        unsafe {
            libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Communicate without a writer/reader thread. Both pipes make progress in the
/// same deadline loop, with a hard limit on output from an untrusted child.
#[cfg(test)]
pub(crate) fn run(
    cmd: &mut Command,
    input: &[u8],
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>, String> {
    run_guarded(cmd, input, timeout, limit, || true)
}

pub(crate) fn run_guarded(
    cmd: &mut Command,
    input: &[u8],
    timeout: Duration,
    limit: usize,
    mut permitted: impl FnMut() -> bool,
) -> Result<Vec<u8>, String> {
    if crate::runtime::closing() || !permitted() {
        return Err("process cancelled".into());
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = OwnedChild::spawn(cmd)?;
    let mut stdin = child.0.stdin.take();
    let mut stdout = child.0.stdout.take().ok_or("stdout unavailable")?;
    if let Some(pipe) = stdin.as_ref() {
        nonblocking(pipe).map_err(|e| e.to_string())?;
    }
    nonblocking(&stdout).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let mut written = 0;
    let mut output = Vec::new();
    let mut eof = false;
    let mut status = None;
    loop {
        if crate::runtime::closing() || !permitted() {
            return Err("process cancelled".into());
        }
        if written == input.len() {
            stdin.take();
        }
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(&input[written..]) {
                Ok(0) => return Err("process input closed".into()),
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("process input: {e}")),
            }
        }
        let mut buffer = [0; 8192];
        match stdout.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(n) => {
                if n > limit.saturating_sub(output.len()) {
                    return Err("process output limit exceeded".into());
                }
                output.extend_from_slice(&buffer[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("process output: {e}")),
        }
        if status.is_none() {
            status = child
                .try_status()
                .map_err(|e| format!("process wait: {e}"))?;
        }
        if let Some(status) = status
            && eof
        {
            if !status.success() {
                return Err(format!("process failed: {status}"));
            }
            if written < input.len() {
                return Err("process did not consume input".into());
            }
            return Ok(output);
        }
        if Instant::now() >= deadline {
            return Err("process deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn communicates_large_input_without_pipe_deadlock() {
        let input = vec![b'x'; 300_000];
        assert_eq!(
            run(
                Command::new("cat").arg("-"),
                &input,
                Duration::from_secs(2),
                input.len()
            )
            .unwrap(),
            input
        );
    }
    #[test]
    fn bounds_stalled_input_output_and_nonzero_exit() {
        let t = Instant::now();
        assert!(
            run(
                Command::new("sleep").arg("30"),
                &vec![0; 300_000],
                Duration::from_millis(80),
                1024
            )
            .unwrap_err()
            .contains("deadline")
        );
        assert!(t.elapsed() < Duration::from_secs(1));
        assert!(
            run(&mut Command::new("yes"), &[], Duration::from_secs(1), 1024)
                .unwrap_err()
                .contains("limit")
        );
        assert!(
            run(
                Command::new("sh").args(["-c", "printf partial; exit 3"]),
                &[],
                Duration::from_secs(1),
                1024
            )
            .unwrap_err()
            .contains("failed")
        );
    }
}
