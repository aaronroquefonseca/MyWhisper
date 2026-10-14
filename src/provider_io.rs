//! Owned asynchronous HTTP transfer: dropping the request cancels its IO.
//! Custom resolution uses a bounded, cancellable subprocess instead of
//! leaving blocking NSS jobs behind when the runtime or request is dropped.
use crate::operation::Operation;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Resolver(Operation);
impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let operation = self.0.clone();
        let address = format!("{}:1", name.as_str());
        Box::pin(async move {
            let addresses =
                crate::address::resolve(&address, Instant::now() + Duration::from_secs(3), || {
                    operation.permitted()
                })
                .map_err(std::io::Error::other)?;
            let addresses = addresses
                .into_iter()
                .map(|mut address| {
                    address.set_port(0);
                    address
                })
                .collect::<Vec<_>>();
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub(crate) fn post(
    url: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
    operation: &Operation,
    description: &str,
) -> Result<String, String> {
    post_with_timeout(
        url,
        headers,
        body,
        operation,
        description,
        crate::transcribe::INFERENCE_TIMEOUT,
    )
}

pub(crate) fn post_with_timeout(
    url: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
    operation: &Operation,
    description: &str,
    timeout: Duration,
) -> Result<String, String> {
    operation.check()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "provider runtime unavailable")?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(timeout)
            .dns_resolver(Arc::new(Resolver(operation.clone())))
            .build()
            .map_err(|error| crate::transcribe::http_error(description, &error))?;
        let mut request = client.post(url).body(body);
        for (name, value) in headers {
            request = if *name == "authorization-bearer" {
                request.bearer_auth(value)
            } else {
                request.header(*name, *value)
            };
        }
        operation.check()?;
        let transfer = Box::pin(async {
            let mut response = request
                .send()
                .await
                .map_err(|error| crate::transcribe::http_error(description, &error))?;
            if !response.status().is_success() {
                return Err(format!(
                    "{description}: HTTP {}",
                    response.status().as_u16()
                ));
            }
            const CAP: usize = 1024 * 1024;
            if response
                .content_length()
                .is_some_and(|length| length > CAP as u64)
            {
                return Err(format!("{description}: response size limit exceeded"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| crate::transcribe::http_error(description, &error))?
            {
                operation.check()?;
                if chunk.len() > CAP.saturating_sub(bytes.len()) {
                    return Err(format!("{description}: response size limit exceeded"));
                }
                bytes.extend_from_slice(&chunk);
            }
            operation.check()?;
            String::from_utf8(bytes).map_err(|_| format!("{description}: response is not UTF-8"))
        });
        let cancellation = Box::pin(async {
            loop {
                if let Err(error) = operation.check() {
                    break error;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        match futures_util::future::select(transfer, cancellation).await {
            futures_util::future::Either::Left((result, _)) => result,
            futures_util::future::Either::Right((error, _)) => Err(error),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn capture_operation(cancel: &Arc<AtomicBool>) -> (tempfile::TempDir, Operation) {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&root.path().join("state.db")).unwrap();
        let epoch = store.capture_epoch().unwrap();
        let operation = Operation::capture(store, epoch, cancel.clone()).unwrap();
        (root, operation)
    }

    /// Exact phase of a synthetic request read. EOF before the header
    /// terminator is a legitimate early-cancel outcome (client went away
    /// before sending a full request): callers observe it explicitly and
    /// never mistake it for stalled-body cancellation coverage.
    #[derive(Debug, PartialEq, Eq)]
    enum HeaderRead {
        Complete,
        ClientGone,
    }

    fn read_request_headers(stream: &mut std::net::TcpStream) -> HeaderRead {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8];
        loop {
            if head.ends_with(b"\r\n\r\n") {
                return HeaderRead::Complete;
            }
            assert!(head.len() < 64 * 1024);
            use std::io::Read as _;
            match stream.read_exact(&mut byte) {
                Ok(()) => head.push(byte[0]),
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return HeaderRead::ClientGone;
                }
                Err(error) => panic!("synthetic server IO failure: {error:?}"),
            }
        }
    }

    /// Accept one connection within `deadline`. A cancelled client may never
    /// connect; the server must exit instead of wedging the test join.
    fn accept_within(listener: &TcpListener, deadline: Instant) -> Option<std::net::TcpStream> {
        listener.set_nonblocking(true).unwrap();
        loop {
            match listener.accept() {
                Ok((stream, _)) => return Some(stream),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    #[test]
    fn stalled_response_body_is_abandoned_on_cancel_within_deadline() {
        // Server sends headers plus one partial chunk, then stalls forever.
        // Cancelling must abandon the transfer promptly (not after the
        // inference timeout) and close the connection. The cancel fires
        // only after the server observes the complete request: rendezvous,
        // not sleep, establishes the stalled-body phase.
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let (headers_seen, headers_signal) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let Some(mut stream) =
                accept_within(&listener, Instant::now() + Duration::from_secs(10))
            else {
                return HeaderRead::ClientGone;
            };
            // Full headers observed before the test may cancel: this is the
            // stalled-body phase under test, established by rendezvous.
            let phase = read_request_headers(&mut stream);
            let _ = headers_seen.send(());
            // The client may already be gone (cancel wins instantly):
            // a failed write is that legitimate outcome, not a stall.
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-length: 1000000\r\nconnection: close\r\n\r\npartial",
            );
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf);
            phase
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (_held_root, held_operation) = capture_operation(&cancel);
        let worker = std::thread::spawn(move || {
            post(
                &url,
                &[],
                b"synthetic".to_vec(),
                &held_operation,
                "synthetic stall",
            )
        });
        headers_signal
            .recv_timeout(Duration::from_secs(10))
            .expect("server never observed the request headers");
        let began = Instant::now();
        cancel.store(true, Ordering::Release);
        assert!(worker.join().unwrap().is_err());
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "cancel took {:?}",
            began.elapsed()
        );
        assert_eq!(server.join().unwrap(), HeaderRead::Complete);
    }

    #[test]
    fn repeated_cancelled_transfers_finish_bounded_without_stalling() {
        // Twenty stalled transfers, each cancelled mid-flight, must all
        // return promptly: no hang, no accumulated stall, bounded total.
        // Every iteration rendezvous on the server observing complete
        // headers before cancelling, so each one covers the intended
        // active-transfer phase (never an early exit mistaken for it).
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let (headers_seen, headers_signal) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let stop = Instant::now() + Duration::from_secs(30);
            let mut served = 0;
            let mut anomalies = 0;
            while served < 20 && Instant::now() < stop {
                let Some(mut stream) =
                    accept_within(&listener, Instant::now() + Duration::from_secs(2))
                else {
                    continue;
                };
                served += 1;
                if read_request_headers(&mut stream) != HeaderRead::Complete {
                    anomalies += 1;
                    continue;
                }
                let _ = headers_seen.send(());
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf);
            }
            (served, anomalies)
        });
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&root.path().join("state.db")).unwrap();
        let epoch = store.capture_epoch().unwrap();
        let began = Instant::now();
        for _ in 0..20 {
            let cancel = Arc::new(AtomicBool::new(false));
            let operation = Operation::capture(
                crate::store::Store::open_existing(&root.path().join("state.db")).unwrap(),
                epoch,
                cancel.clone(),
            )
            .unwrap();
            let worker = std::thread::spawn({
                let address = url.clone();
                move || {
                    post(
                        &address,
                        &[],
                        b"synthetic".to_vec(),
                        &operation,
                        "synthetic cancel stress",
                    )
                }
            });
            headers_signal
                .recv_timeout(Duration::from_secs(10))
                .expect("server never observed the request headers");
            cancel.store(true, Ordering::Release);
            assert!(worker.join().unwrap().is_err());
        }
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "stress took {:?}",
            began.elapsed()
        );
        let (served, anomalies) = server.join().unwrap();
        assert_eq!(served, 20);
        assert_eq!(anomalies, 0);
    }

    #[test]
    fn precancelled_post_fails_before_any_io() {
        // Separate early-cancel contract: an already-cancelled operation
        // cannot even be constructed for capture — the entry gate fails
        // closed before any runtime, client, DNS or socket exists.
        let cancel = Arc::new(AtomicBool::new(false));
        cancel.store(true, Ordering::Release);
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&root.path().join("state.db")).unwrap();
        let epoch = store.capture_epoch().unwrap();
        let began = Instant::now();
        let error = match Operation::capture(store, epoch, cancel) {
            Ok(_) => panic!("pre-cancelled capture must not construct"),
            Err(error) => error,
        };
        assert!(
            error.contains("cancelled"),
            "pre-cancel must fail closed, got: {error}"
        );
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "pre-cancel took {:?}",
            began.elapsed()
        );
    }
}
