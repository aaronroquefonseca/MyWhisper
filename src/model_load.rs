//! One page-owned Ollama load/unload operation. It never owns GTK widgets.
use crate::operation::Operation;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

pub(crate) struct ModelLoad {
    cancel: Arc<AtomicBool>,
    pub(crate) result: mpsc::Receiver<Result<(), String>>,
    _task: crate::runtime::SessionTask,
}
impl ModelLoad {
    pub(crate) fn start(
        url: String,
        model: String,
        token: String,
        keep: &'static str,
    ) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let operation = Operation::cancellable(cancel.clone());
        let (sender, result) = mpsc::sync_channel(1);
        let task = crate::runtime::spawn_session("ollama-model-load", move || {
            let result = apply(&url, &model, &token, keep, &operation);
            let _ = sender.try_send(result);
        })?;
        Ok(Self {
            cancel,
            result,
            _task: task,
        })
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Drop for ModelLoad {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn apply(
    url: &str,
    model: &str,
    token: &str,
    keep: &str,
    operation: &Operation,
) -> Result<(), String> {
    operation.check()?;
    let mut body = serde_json::json!({"model":model,"stream":false,"messages":[]});
    if !keep.is_empty() {
        body["keep_alive"] = match keep {
            "-1" => serde_json::Value::from(-1),
            "0" => serde_json::Value::from(0),
            _ => serde_json::Value::String(keep.to_string()),
        };
    }
    let mut headers = vec![("Content-Type", "application/json")];
    if !token.is_empty() {
        headers.push(("authorization-bearer", token));
    }
    let response = crate::provider_io::post_with_timeout(
        &format!("{}/api/chat", url.trim_end_matches('/')),
        &headers,
        body.to_string().into_bytes(),
        operation,
        "model load",
        Duration::from_secs(60),
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&response).map_err(|_| "model load: invalid JSON response")?;
    if value["done"].as_bool() != Some(true)
        || value.get("error").is_some_and(|error| !error.is_null())
    {
        return Err("model load: provider did not confirm completion".into());
    }
    operation.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn read_request(socket: &mut std::net::TcpStream) -> (String, serde_json::Value) {
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let n = socket.read(&mut buffer).unwrap();
            assert!(n != 0);
            bytes.extend_from_slice(&buffer[..n]);
            assert!(bytes.len() < 8192);
            if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|value| value.parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    return (headers, serde_json::from_slice(&bytes[end + 4..]).unwrap());
                }
            }
        }
    }

    #[test]
    fn load_and_unload_preserve_authentication_and_model_default_contracts() {
        for (keep, expected) in [
            ("", None),
            ("-1", Some(serde_json::json!(-1))),
            ("0", Some(serde_json::json!(0))),
            ("5m", Some(serde_json::json!("5m"))),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let (headers, body) = read_request(&mut socket);
                assert!(headers.starts_with("POST /api/chat HTTP/1.1"));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer synthetic-token\r\n")
                );
                assert_eq!(body["model"], "synthetic-model");
                assert_eq!(body["messages"], serde_json::json!([]));
                assert_eq!(body["stream"], false);
                assert_eq!(body.get("keep_alive").cloned(), expected);
                assert!(body.get("think").is_none());
                let response = br#"{"done":true,"done_reason":"load","error":null}"#;
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .unwrap();
                socket.write_all(response).unwrap();
            });
            apply(
                &url,
                "synthetic-model",
                "synthetic-token",
                keep,
                &Operation::unrestricted(),
            )
            .unwrap();
            peer.join().unwrap();
        }
    }

    #[test]
    fn header_success_cannot_mask_incomplete_or_malformed_completion() {
        for response in [
            "",
            "not JSON synthetic-provider-secret",
            "null",
            r#"{"done":false}"#,
            r#"{"done":true,"error":"synthetic-provider-secret"}"#,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let _ = read_request(&mut socket);
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .unwrap();
            });
            let error =
                apply(&url, "synthetic-model", "", "", &Operation::unrestricted()).unwrap_err();
            assert!(!error.contains("synthetic-provider-secret"));
            peer.join().unwrap();
        }
    }

    #[test]
    fn cancellation_closes_stalled_headers_and_partial_completion_bodies() {
        for partial_body in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let (ready, barrier) = mpsc::channel();
            let peer = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let _ = read_request(&mut socket);
                if partial_body {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 32\r\nConnection: close\r\n\r\n{\"done\":").unwrap();
                }
                socket
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                ready.send(()).unwrap();
                assert_eq!(
                    socket.read(&mut [0; 1]).unwrap(),
                    0,
                    "cancellation must close the provider socket"
                );
            });
            let cancel = Arc::new(AtomicBool::new(false));
            let operation = Operation::cancellable(cancel.clone());
            let (send, receive) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                send.send(apply(
                    &url,
                    "synthetic-model",
                    "synthetic-token",
                    "",
                    &operation,
                ))
                .unwrap();
            });
            barrier.recv_timeout(Duration::from_secs(2)).unwrap();
            cancel.store(true, Ordering::Release);
            assert!(
                receive
                    .recv_timeout(Duration::from_millis(500))
                    .unwrap()
                    .is_err()
            );
            worker.join().unwrap();
            peer.join().unwrap();
        }
    }
}
