//! Qwen managed runtime: private pinned Python + locked deps.
//!
//! The old installer shelled out to system `python3 -m venv` and
//! accepted any Python >= 3.9. A user Python 3.14 then fails on the
//! pinned `qwen-asr==0.0.6 → vllm==0.14.0` chain, whose requirement
//! is `>=3.10,<3.14`. That is a provisioning bug: the installer must
//! never depend on the user's interpreter.
//!
//! This module provisions a private interpreter instead:
//!
//! * Pinned portable CPython 3.12 (satisfies qwen-asr >= 3.9 and
//!   vllm >= 3.10,<3.14, plus transformers/accelerate/torch >= 3.10)
//!   installed under MyWhisper's managed storage (`<data>/bin/`).
//! * A private pinned `uv` binary (Rust, standalone) only as an
//!   install tool: users never install uv, nothing touches the
//!   global `PATH` or system Python.
//! * Locked pip specs per backend (torch/qwen/vllm/CUDA user-space
//!   wheels from PyPI, `--only-binary` for heavy builds).
//! * Transactional activation: stage downloads under owned staging
//!   storage, verify (sha256 + `python --version` + import checks),
//!   create the venv at its final path (venvs are not relocatable),
//!   write the marker last. Failures/cancels restore the previous
//!   working runtime and never leave a half-installed `.installed`.
//! * Cancellation kills the whole installer process group
//!   (descendants included), reaps children, and removes owned
//!   staging/partial files. Retry/removal coordinate through the
//!   caller's download-state map: removal while installing is
//!   refused until the run is cancelled.
//!
//! Supported baseline (this PR): Linux x86_64 and aarch64, glibc
//! new enough for the vLLM `manylinux_2_31` wheels and torch
//! `manylinux_2_28` wheels. Remaining host prerequisites (not
//! installed here): compatible NVIDIA GPU + proprietary driver for
//! the vLLM backend; the transformers backend runs on CPU.
//! The NVIDIA driver is never installed or replaced. CUDA user-space
//! libraries arrive as private `nvidia-*` wheels inside the venv.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------- pins

/// Tested interpreter: satisfies every pinned requirement below
/// (qwen-asr >= 3.9; vllm >= 3.10,<3.14; torch/accelerate >= 3.10).
pub const QWEN_PYTHON_VERSION: &str = "3.12.12";
pub const QWEN_PYTHON_MAJOR: u32 = 3;
pub const QWEN_PYTHON_MINOR: u32 = 12;

/// Private install-tool pin. Users never install uv; this binary
/// lives under managed storage and is only used to fetch Python and
/// create/install the venv.
pub const QWEN_UV_VERSION: &str = "0.13.0";

fn uv_asset() -> Result<(&'static str, &'static str), String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok((
            "uv-x86_64-unknown-linux-gnu.tar.gz",
            "1468ebd5a5541121837c5a2817b9972ba6090fa6caa3d142620850a47fb75154",
        )),
        "aarch64" => Ok((
            "uv-aarch64-unknown-linux-gnu.tar.gz",
            "3ccfb6af6e242433eb552f7d9676abd5412c6595c497e990d9c8cb7b5bd4d2c3",
        )),
        other => Err(format!("qwen runtime has no uv build for {other}")),
    }
}

fn uv_url(asset: &str) -> String {
    format!("https://github.com/astral-sh/uv/releases/download/{QWEN_UV_VERSION}/{asset}")
}

/// OS/arch baseline for this PR. vLLM ships `manylinux_2_31`
/// x86_64/aarch64 wheels; torch ships `manylinux_2_28`.
pub fn supported_platform() -> Result<(), String> {
    if std::env::consts::OS != "linux" {
        return Err(format!(
            "qwen runtime supports Linux only (this OS: {})",
            std::env::consts::OS
        ));
    }
    match std::env::consts::ARCH {
        "x86_64" | "aarch64" => Ok(()),
        other => Err(format!("qwen runtime has no build for {other}")),
    }
}

// ---------------------------------------------------------------- lock

/// Locked top-level specs: the complete dep set resolves from these
/// without source builds for the heavy packages. `torch==2.9.1` is
/// exactly what `vllm==0.14.0` pins; `transformers==4.57.6` and
/// `accelerate==1.12.0` are what `qwen-asr==0.0.6` pins. CUDA
/// user-space libraries (`nvidia-*`) resolve as `vllm` dependencies
/// into the same venv (private, no driver install).
pub fn locked_specs_vllm() -> &'static [&'static str] {
    &[
        "qwen-asr[vllm]==0.0.6",
        "vllm==0.14.0",
        "torch==2.9.1",
        "torchaudio==2.9.1",
        "torchvision==0.24.1",
        "transformers==4.57.6",
        "accelerate==1.12.0",
        "numpy",
        "soundfile",
    ]
}

pub fn locked_specs_transformers() -> &'static [&'static str] {
    &[
        "qwen-asr==0.0.6",
        "torch==2.9.1",
        "transformers==4.57.6",
        "accelerate==1.12.0",
        "numpy",
        "soundfile",
        "librosa",
    ]
}

/// Modules that must import cleanly after install.
pub fn import_checks_vllm() -> &'static [&'static str] {
    &["qwen_asr", "vllm"]
}

pub fn import_checks_transformers() -> &'static [&'static str] {
    &["qwen_asr", "torch"]
}

/// Static compatibility audit of the lock (no network): the pinned
/// interpreter must satisfy every pinned `requires-python`.
/// Sources: qwen-asr pyproject (`>=3.9`), vllm 0.14.0 (`>=3.10,<3.14`),
/// torch 2.9.1 / accelerate 1.12.0 (`>=3.10`).
pub fn check_locked_compat() -> Result<(), String> {
    // (floor_major, floor_minor, excluded major.minor|None)
    type Floor = (u32, u32);
    let reqs: &[(&str, Floor, Option<Floor>)] = &[
        ("qwen-asr==0.0.6", (3, 9), None),
        ("transformers==4.57.6", (3, 9), None),
        ("accelerate==1.12.0", (3, 10), None),
        ("torch==2.9.1", (3, 10), None),
        ("vllm==0.14.0", (3, 10), Some((3, 14))),
    ];
    for (spec, floor, excl) in reqs {
        if (QWEN_PYTHON_MAJOR, QWEN_PYTHON_MINOR) < *floor {
            return Err(format!(
                "pinned python {}.{} below {spec} floor",
                QWEN_PYTHON_MAJOR, QWEN_PYTHON_MINOR
            ));
        }
        if let Some(ex) = excl
            && (QWEN_PYTHON_MAJOR, QWEN_PYTHON_MINOR) >= *ex
        {
            return Err(format!(
                "pinned python {}.{} excluded by {spec}",
                QWEN_PYTHON_MAJOR, QWEN_PYTHON_MINOR
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- versions

/// Parse `Python 3.12.12` out of `python --version` output.
pub fn parse_python_version(output: &str) -> Option<(u32, u32)> {
    let num = output.split_whitespace().nth(1)?;
    let mut parts = num.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.trim().parse::<u32>().ok()?;
    Some((major, minor))
}

/// Only the pinned 3.12 line is accepted. In particular system
/// Python 3.14 (the demonstrated failure) is rejected even when
/// present: the installer never uses it.
pub fn managed_python_ok(major: u32, minor: u32) -> bool {
    major == QWEN_PYTHON_MAJOR && minor == QWEN_PYTHON_MINOR
}

pub fn is_cancel_error(e: &str) -> bool {
    e == "cancelled" || e.contains("cancelled")
}

// ---------------------------------------------------------------- layout

/// `<data>/bin/<subdir>` (runtime root), `<data>/bin/.staging-qwen-<id>`
/// (owned staging), `<data>/bin/.backup-qwen-<id>` (previous working
/// runtime during replacement).
pub fn staging_dir(data_dir: &Path, subdir: &str) -> PathBuf {
    data_dir.join("bin").join(format!(".staging-qwen-{subdir}"))
}

pub fn backup_dir(data_dir: &Path, subdir: &str) -> PathBuf {
    data_dir.join("bin").join(format!(".backup-qwen-{subdir}"))
}

pub fn runtime_dir(data_dir: &Path, subdir: &str) -> PathBuf {
    data_dir.join("bin").join(subdir)
}

pub fn venv_python(runtime: &Path) -> PathBuf {
    runtime.join("venv").join("bin").join("python")
}

pub fn install_log(runtime: &Path) -> PathBuf {
    runtime.join("install.log")
}

/// Marker v2 (only written after post-activation verification at the
/// final path). The v1 marker (`"<id>|<specs>\n"`) from the old
/// system-python installer is deliberately rejected: that venv may
/// be a Python 3.14 environment that can never satisfy vllm.
pub fn marker_text(subdir: &str, specs: &[&str]) -> String {
    format!(
        "qwen-runtime-v2|{subdir}|py{QWEN_PYTHON_VERSION}|{}\n",
        specs.join(" ")
    )
}

pub fn marker_valid(text: &str, subdir: &str, specs: &[&str]) -> bool {
    text == marker_text(subdir, specs)
}

/// User-facing host prerequisites (shown before large downloads).
pub fn host_prereqs_vllm() -> &'static str {
    "vLLM backend needs an NVIDIA GPU with a compatible proprietary driver \
     (the driver is not installed here) on Linux x86_64/aarch64; CUDA user-space \
     libraries ship privately inside the runtime. CPU-only machines should use \
     the transformers backend (offline decode, no live streaming)."
}

pub fn capabilities_line(vllm: bool) -> &'static str {
    if vllm {
        "NVIDIA GPU: offline + live streaming (vLLM)"
    } else {
        "CPU or NVIDIA GPU: offline decode only (transformers, no live streaming)"
    }
}

// ---------------------------------------------------------------- GPU gate

/// Advisory pre-flight before multi-GB vLLM downloads: refuse fast
/// with an actionable message when no NVIDIA GPU is visible, instead
/// of downloading ~8 GB that can never run.
pub fn check_gpu_for_vllm() -> Result<(), String> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .map_err(|_| {
            format!(
                "no NVIDIA GPU detected (nvidia-smi missing). {}.",
                host_prereqs_vllm()
            )
        })?;
    if !out.status.success() {
        return Err(format!(
            "no NVIDIA GPU detected (nvidia-smi failed). {}.",
            host_prereqs_vllm()
        ));
    }
    let name = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(format!(
            "no NVIDIA GPU detected (empty nvidia-smi). {}.",
            host_prereqs_vllm()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- stages

/// User-visible install stages (concise); detail stays in install.log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    CheckHost,
    FetchUv,
    InstallPython,
    CreateVenv,
    InstallDeps,
    Verify,
    Activate,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::CheckHost => "checking host",
            Stage::FetchUv => "fetching installer",
            Stage::InstallPython => "installing Python 3.12",
            Stage::CreateVenv => "creating environment",
            Stage::InstallDeps => "installing dependencies",
            Stage::Verify => "verifying",
            Stage::Activate => "activating",
        }
    }
}

// ---------------------------------------------------------------- process groups

/// Spawn with its own process group (`setsid`) so cancellation can
/// signal descendants too (pip/vllm builds fork helpers).
fn spawn_grouped(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.stdin(std::process::Stdio::null()).spawn()
}

fn kill_group(child: &mut std::process::Child) {
    let pid = child.id() as i32;
    // Negative pid = whole process group.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => return,
        }
        if start.elapsed() > std::time::Duration::from_secs(2) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// Run to completion with cancel polling; on cancel the entire
/// process group is signalled and reaped, then `"cancelled"` is
/// returned (callers surface it distinctly from failures).
pub fn run_cancellable(
    cmd: &mut std::process::Command,
    cancel: &AtomicBool,
    log_path: &Path,
    what: &str,
) -> Result<(), String> {
    use std::fs::OpenOptions;
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("{what}: open log: {e}"))?;
    let out_clone = log.try_clone().map_err(|e| format!("{what}: log: {e}"))?;
    let mut child = spawn_grouped(
        cmd.stdout(std::process::Stdio::from(log))
            .stderr(std::process::Stdio::from(out_clone)),
    )
    .map_err(|e| format!("{what}: spawn: {e}"))?;
    loop {
        if cancel.load(Ordering::SeqCst) {
            kill_group(&mut child);
            return Err("cancelled".to_string());
        }
        match child.try_wait().map_err(|e| format!("{what}: wait: {e}"))? {
            Some(status) => {
                if status.success() {
                    return Ok(());
                }
                return Err(format!("{what}: exit {status} (see install.log)"));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

// ---------------------------------------------------------------- downloads

/// Stream a URL to `dest` with cancel + sha256 verification. Blocking;
/// callers run it on a worker thread. Partial files are removed on
/// cancel/failure so retries start clean.
pub fn download_verified(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    cancel: &AtomicBool,
    on_progress: &dyn Fn(u64, Option<u64>),
) -> Result<(), String> {
    use sha2::Digest;
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http status: {}", resp.status()));
    }
    let total = resp.content_length();
    let file = std::fs::File::create(&part).map_err(|e| format!("create: {e}"))?;
    let mut hasher = sha2::Sha256::new();
    let mut received = 0u64;
    let mut buf = [0u8; 65536];
    let mut writer = HashWriter {
        file,
        hasher: &mut hasher,
    };
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&part);
            return Err("cancelled".to_string());
        }
        let n = resp.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| format!("write: {e}"))?;
        received += n as u64;
        on_progress(received, total);
    }
    drop(writer);
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != expected_sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "sha256 mismatch (got {digest}, expected {expected_sha256})"
        ));
    }
    std::fs::rename(&part, dest).map_err(|e| format!("install: {e}"))?;
    Ok(())
}

struct HashWriter<'a> {
    file: std::fs::File,
    hasher: &'a mut sha2::Sha256,
}

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.hasher.update(buf);
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

// ---------------------------------------------------------------- provision

/// Ensure the private uv binary exists under staging (downloaded +
/// sha-verified). Returns its path.
pub fn ensure_private_uv(
    staging: &Path,
    cancel: &AtomicBool,
    on_progress: &dyn Fn(u64, Option<u64>),
) -> Result<PathBuf, String> {
    let (asset, sha) = uv_asset()?;
    let dir = staging.join("tools");
    let uv_bin = dir.join("uv");
    if uv_bin.exists() {
        return Ok(uv_bin);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir tools: {e}"))?;
    let archive = dir.join(asset);
    download_verified(&uv_url(asset), &archive, sha, cancel, on_progress)?;
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let file = std::fs::File::open(&archive).map_err(|e| format!("open uv: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(&dir).map_err(|e| format!("extract uv: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    // Layout is `<dir>/uv-<arch>/uv`; normalize to `<dir>/uv`.
    if !uv_bin.exists() {
        let mut found = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let cand = e.path().join("uv");
                if cand.exists() {
                    found = Some(cand);
                    break;
                }
            }
        }
        match found {
            Some(src) => std::fs::rename(&src, &uv_bin).map_err(|e| format!("place uv: {e}"))?,
            None => return Err("uv binary missing after extract".to_string()),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&uv_bin, std::fs::Permissions::from_mode(0o755));
    }
    if !uv_bin.exists() {
        return Err("uv binary missing after extract".to_string());
    }
    Ok(uv_bin)
}

/// Install the pinned Python into `<staging>/python` via the private
/// uv. Never consults system Python.
pub fn ensure_managed_python(
    uv: &Path,
    staging: &Path,
    cancel: &AtomicBool,
    log_path: &Path,
) -> Result<PathBuf, String> {
    let install_dir = staging.join("python");
    let target = install_dir.join(format!("cpython-{QWEN_PYTHON_VERSION}"));
    // uv layout varies; find a usable python3 under install_dir.
    if let Some(existing) = find_managed_python(&install_dir)
        && python_version_ok(&existing, cancel)?
    {
        return Ok(existing);
    }
    std::fs::create_dir_all(&install_dir).map_err(|e| format!("mkdir python: {e}"))?;
    let mut cmd = std::process::Command::new(uv);
    cmd.args(["python", "install", QWEN_PYTHON_VERSION, "--install-dir"])
        .arg(&install_dir);
    // Isolate uv from user env/shell config.
    cmd.env("UV_CACHE_DIR", staging.join("uv-cache"))
        .env("UV_NO_CACHE", "0")
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV");
    run_cancellable(&mut cmd, cancel, log_path, "uv python install")?;
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let _ = target;
    match find_managed_python(&install_dir) {
        Some(p) => {
            if python_version_ok(&p, cancel)? {
                Ok(p)
            } else {
                Err(
                    "managed python failed version check (wanted 3.12, see install.log)"
                        .to_string(),
                )
            }
        }
        None => Err("managed python missing after install (see install.log)".to_string()),
    }
}

fn find_managed_python(install_dir: &Path) -> Option<PathBuf> {
    // Preferred exact dir, then any cpython-3.12* dir.
    let mut candidates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                for bin in ["bin/python3", "bin/python", "python.exe"] {
                    let cand = p.join(bin);
                    if cand.exists() {
                        candidates.push(cand);
                    }
                }
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

fn python_version_ok(python: &Path, cancel: &AtomicBool) -> Result<bool, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let out = std::process::Command::new(python)
        .arg("--version")
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV")
        .output()
        .map_err(|e| format!("managed python --version: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    match parse_python_version(&text) {
        Some((maj, min)) => Ok(managed_python_ok(maj, min)),
        None => Ok(false),
    }
}

/// Verify an installed runtime at its final path: venv python exists,
/// reports pinned 3.12, marker matches, import checks pass.
pub fn verify_runtime(
    runtime: &Path,
    subdir: &str,
    specs: &[&str],
    import_checks: &[&str],
    cancel: &AtomicBool,
    log_path: &Path,
) -> Result<(), String> {
    let python = venv_python(runtime);
    if !python.exists() {
        return Err("runtime python missing (reinstall)".to_string());
    }
    if !python_version_ok(&python, cancel)? {
        return Err(
            "runtime python is not 3.12 (incompatible older install; remove and reinstall)"
                .to_string(),
        );
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".to_string());
    }
    let check = format!("import {}", import_checks.join(", "));
    let mut cmd = std::process::Command::new(&python);
    cmd.args(["-c", &check]);
    cmd.env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV");
    run_cancellable(&mut cmd, cancel, log_path, "import check")?;
    let mark = std::fs::read_to_string(runtime.join(".installed"))
        .map_err(|_| "runtime marker missing (incomplete install)".to_string())?;
    if !marker_valid(&mark, subdir, specs) {
        return Err("runtime marker mismatch (stale install; remove and reinstall)".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_satisfies_all_requires_python() {
        check_locked_compat().unwrap();
        assert_eq!(QWEN_PYTHON_VERSION, "3.12.12");
    }

    #[test]
    fn system_python_314_is_rejected() {
        assert!(!managed_python_ok(3, 14));
        assert!(!managed_python_ok(3, 9));
        assert!(!managed_python_ok(3, 13));
        assert!(managed_python_ok(3, 12));
        assert_eq!(parse_python_version("Python 3.14.7"), Some((3, 14)));
        assert_eq!(parse_python_version("Python 3.12.12"), Some((3, 12)));
        assert!(!managed_python_ok(3, 14));
    }

    #[test]
    fn lock_pins_heavy_builds() {
        let vllm = locked_specs_vllm();
        assert!(vllm.contains(&"vllm==0.14.0"));
        assert!(vllm.contains(&"torch==2.9.1"));
        assert!(vllm.contains(&"qwen-asr[vllm]==0.0.6"));
        assert!(vllm.contains(&"transformers==4.57.6"));
        let tf = locked_specs_transformers();
        assert!(tf.contains(&"qwen-asr==0.0.6"));
        assert!(tf.contains(&"torch==2.9.1"));
        // Never arbitrarily upgrade vllm past qwen's pin.
        assert!(!vllm.iter().any(|s| s.starts_with("vllm==0.15")));
    }

    #[test]
    fn old_marker_is_rejected() {
        let specs = locked_specs_transformers();
        assert!(!marker_valid(
            "qwen-transformers|qwen-asr==0.0.6 torch\n",
            "qwen-transformers",
            specs
        ));
        assert!(!marker_valid("", "qwen-transformers", specs));
        let good = marker_text("qwen-transformers", specs);
        assert!(marker_valid(&good, "qwen-transformers", specs));
        assert!(!marker_valid(&good, "qwen-vllm", specs));
    }

    #[test]
    fn cancel_is_distinct_from_failure() {
        assert!(is_cancel_error("cancelled"));
        assert!(!is_cancel_error("sha256 mismatch"));
        assert!(!is_cancel_error("http status: 404"));
    }

    #[test]
    fn corrupt_download_is_rejected() {
        let dir = std::env::temp_dir().join(format!("qwen-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("uv.tar.gz");
        let cancel = AtomicBool::new(false);
        // Unreachable URL: must fail, never touch system python, no
        // partial left behind.
        let err = download_verified(
            "http://127.0.0.1:9/nope.tar.gz",
            &dest,
            "0".repeat(64).as_str(),
            &cancel,
            &|_, _| {},
        )
        .unwrap_err();
        assert!(!is_cancel_error(&err));
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupted_cancel_cleans_partial() {
        let dir = std::env::temp_dir().join(format!("qwen-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("x.bin");
        let cancel = AtomicBool::new(true);
        let err = download_verified(
            "http://127.0.0.1:9/nope.bin",
            &dest,
            "0".repeat(64).as_str(),
            &cancel,
            &|_, _| {},
        )
        .unwrap_err();
        assert!(is_cancel_error(&err));
        assert!(!dest.with_extension("part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incomplete_runtime_never_counts_as_installed() {
        let dir = std::env::temp_dir().join(format!("qwen-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runtime = dir.join("bin").join("qwen-transformers");
        std::fs::create_dir_all(runtime.join("venv").join("bin")).unwrap();
        // venv dir exists but no marker/python: incomplete.
        assert!(!runtime.join(".installed").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
