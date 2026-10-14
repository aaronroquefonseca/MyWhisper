//! Real ENOSPC through the public downloader, restricted to an owned tiny tmpfs.
use mywhisper::stt::{DownloadCallback, DownloadEvent, ModelEntry, ModelKind};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(std::env::var_os("MYWHISPER_AUDIT_ROOT").ok_or("disposable root required")?);
    let metadata = std::fs::symlink_metadata(&root)?;
    if !root.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with("mywhisper-installfs-audit.")
    }) || !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
        || !root.join(".disposable").is_file()
    {
        return Err("refusing a non-disposable root".into());
    }
    let data = root.join("fs");
    let data_metadata = std::fs::symlink_metadata(&data)?;
    if !data_metadata.is_dir() || data_metadata.file_type().is_symlink() {
        return Err("private tmpfs directory required".into());
    }
    let path = std::ffi::CString::new(data.as_os_str().as_encoded_bytes())?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: NUL-terminated path and properly sized output; inspect only on success.
    if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    if filesystem.f_type != 0x0102_1994
        || filesystem.f_blocks * filesystem.f_bsize as u64 > 1024 * 1024
    {
        return Err("refusing disk-pressure testing outside a tmpfs of at most 1 MiB".into());
    }

    let destination = data.join("models/whisper/fault-probe");
    std::fs::create_dir_all(&destination)?;
    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700))?;
    let model_path = destination.join("ggml-fault-probe.bin");
    let mut previous = File::create(&model_path)?;
    previous.write_all(b"synthetic complete previous model")?;
    previous.sync_all()?;
    let original_inode = previous.metadata()?.ino();
    drop(previous);
    File::open(&destination)?.sync_all()?;

    const CHUNK: usize = 16 * 1024;
    let body = vec![71u8; CHUNK * 8];
    let digest = Sha256::digest(&body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("http://{}/synthetic-model", listener.local_addr()?);
    let (continue_tx, continue_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for attempt in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "probe listener deadline");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("probe accept: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut send = |bytes: &[u8]| -> std::io::Result<()> {
                write!(socket, "{:x}\r\n", bytes.len())?;
                socket.write_all(bytes)?;
                socket.write_all(b"\r\n")
            };
            send(&body[..CHUNK]).unwrap();
            if attempt == 0 {
                continue_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            // The failed client's connection may close during these writes.
            if send(&body[CHUNK..]).is_ok() {
                let _ = socket.write_all(b"0\r\n\r\n");
            }
        }
    });
    let entry = ModelEntry {
        id: "fault-probe",
        kind: ModelKind::Whisper,
        name: "Synthetic disk-pressure probe",
        description: "Synthetic bytes; not a speech model",
        size_label: "128 KiB",
        size_mb: 1,
        expected_bytes: Some((CHUNK * 8) as u64),
        sha256: Box::leak(digest.into_boxed_str()),
        download_url: Box::leak(url.into_boxed_str()),
        extract_dir: None,
        required_files: &[],
        runtime: None,
        recommended: false,
    };
    let pressure = data.join("owned-pressure.bin");
    let pressure_callback = pressure.clone();
    let models = data.join("models/whisper");
    let filled = Arc::new(AtomicBool::new(false));
    let callback_filled = filled.clone();
    let callback: DownloadCallback = Arc::new(move |event| {
        let DownloadEvent::Progress { received, .. } = event else {
            return;
        };
        if received > 0 && !callback_filled.swap(true, Ordering::AcqRel) {
            let partial = std::fs::read_dir(&models)
                .unwrap()
                .map(Result::unwrap)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".mywhisper-install-")
                })
                .unwrap();
            assert_eq!(
                partial
                    .path()
                    .join("download.part")
                    .metadata()
                    .unwrap()
                    .len(),
                received
            );
            assert!(received <= CHUNK as u64);
            println!("Filling private tmpfs after {received} actual downloader bytes.");
            let mut file = File::create(&pressure_callback).unwrap();
            let error = loop {
                if let Err(error) = file.write_all(&[17; 4096]) {
                    break error;
                }
            };
            assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
            continue_tx.send(()).unwrap();
        }
    });
    let result = mywhisper::stt::download_model(&data, &entry, &AtomicBool::new(false), &callback);
    assert!(filled.load(Ordering::Acquire));
    let error = result.expect_err("a partially written body cannot install on a full filesystem");
    assert!(
        error.contains("download write") && error.contains("os error 28"),
        "unexpected failure: {error}"
    );
    assert_eq!(
        std::fs::read(&model_path)?,
        b"synthetic complete previous model"
    );
    assert_eq!(model_path.metadata()?.ino(), original_inode);
    assert_eq!(std::fs::read_dir(data.join("models/whisper"))?.count(), 1);
    assert_eq!(std::fs::read_dir(data.join(".install/staging"))?.count(), 0);
    std::fs::remove_file(&pressure)?;
    let retry: DownloadCallback = Arc::new(|_| {});
    mywhisper::stt::download_model(&data, &entry, &AtomicBool::new(false), &retry)?;
    server.join().expect("probe HTTP server panicked");
    assert_eq!(std::fs::read(&model_path)?, vec![71; CHUNK * 8]);
    assert_eq!(std::fs::read_dir(data.join("models/whisper"))?.count(), 1);
    assert_eq!(std::fs::read_dir(data.join(".install/staging"))?.count(), 0);
    println!(
        "Actual ENOSPC after partial downloader output: previous bytes/inode preserved, private staging removed, authenticated synthetic retry promoted successfully."
    );
    Ok(())
}
