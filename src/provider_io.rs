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
