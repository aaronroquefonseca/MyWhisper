//! Bounded resolver subprocesses and explicit scoped IPv6 socket addresses.
//! NSS/DNS cannot pin a service thread forever: an owned getent group has one
//! deadline and capped output, and is cancelled/reaped on every exit.
use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6, TcpStream};
use std::time::{Duration, Instant};

/// Nonblocking connect with cancellation during the SYN/backlog wait. A
/// short poll bounds cancellation latency without changing the dial budget.
pub(crate) fn connect(
    address: SocketAddr,
    deadline: Instant,
    mut permitted: impl FnMut() -> bool,
) -> Result<TcpStream, String> {
    use std::os::fd::AsRawFd;
    if !permitted() {
        return Err("sync dial cancelled".into());
    }
    if Instant::now() >= deadline {
        return Err("sync connection deadline exceeded".into());
    }
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .map_err(|e| format!("dial socket: {e}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| format!("dial mode: {e}"))?;
    match socket.connect(&address.into()) {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == Some(libc::EINPROGRESS) =>
        {
            loop {
                if !permitted() {
                    return Err("sync dial cancelled".into());
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("sync connection deadline exceeded".into());
                }
                let mut descriptor = libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let timeout = remaining.min(Duration::from_millis(50)).as_millis().max(1) as i32;
                // SAFETY: the one descriptor is live for this stack-local
                // poll and the owned socket cannot close concurrently.
                let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
                if ready < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(format!("dial poll: {error}"));
                }
                if ready == 0 {
                    continue;
                }
                if let Some(error) = socket
                    .take_error()
                    .map_err(|e| format!("dial status: {e}"))?
                {
                    return Err(format!("dial: {error}"));
                }
                socket.peer_addr().map_err(|e| format!("dial peer: {e}"))?;
                break;
            }
        }
        Err(error) => return Err(format!("dial: {error}")),
    }
    if !permitted() {
        return Err("sync dial cancelled".into());
    }
    socket
        .set_nonblocking(false)
        .map_err(|e| format!("dial mode: {e}"))?;
    Ok(socket.into())
}

pub(crate) fn resolve(
    address: &str,
    deadline: Instant,
    mut permitted: impl FnMut() -> bool,
) -> Result<Vec<SocketAddr>, String> {
    if !permitted() {
        return Err("address resolution cancelled".into());
    }
    if Instant::now() >= deadline {
        return Err("address resolution deadline exceeded".into());
    }
    if address.len() > 256 {
        return Err("peer address exceeds limit".into());
    }
    if let Ok(address) = address.parse::<SocketAddr>() {
        return Ok(vec![address]);
    }
    let (host, port) = address
        .rsplit_once(':')
        .ok_or("peer address requires host:port")?;
    let port = port.parse::<u16>().map_err(|_| "invalid peer port")?;
    if port == 0 {
        return Err("peer port must be nonzero".into());
    }
    if let Some(literal) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        let (ip, zone) = literal
            .rsplit_once('%')
            .ok_or("invalid bracketed IPv6 address")?;
        let ip = ip.parse::<Ipv6Addr>().map_err(|_| "invalid IPv6 address")?;
        let zone_id = match zone.parse::<u32>() {
            Ok(id) if id != 0 => id,
            _ => {
                if zone.is_empty()
                    || zone.len() >= libc::IF_NAMESIZE
                    || !zone
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                {
                    return Err("invalid IPv6 interface scope".into());
                }
                let zone = std::ffi::CString::new(zone).map_err(|_| "invalid IPv6 scope")?;
                // SAFETY: CString is terminated; this read-only interface lookup
                // never sends traffic or changes network configuration.
                let id = unsafe { libc::if_nametoindex(zone.as_ptr()) };
                if id == 0 {
                    return Err("IPv6 interface is unavailable".into());
                }
                id
            }
        };
        return Ok(vec![SocketAddrV6::new(ip, port, 0, zone_id).into()]);
    }
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
    {
        return Err("invalid peer hostname".into());
    }
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(3));
    if remaining.is_zero() {
        return Err("peer resolution deadline exceeded".into());
    }
    let bytes = crate::child::run_guarded(
        std::process::Command::new("getent").args(["ahosts", host]),
        &[],
        remaining,
        64 * 1024,
        permitted,
    )?;
    parse_answers(&bytes, port)
}
fn parse_answers(bytes: &[u8], port: u16) -> Result<Vec<SocketAddr>, String> {
    let output = std::str::from_utf8(bytes).map_err(|_| "resolver output is not UTF-8")?;
    let mut addresses = Vec::new();
    for line in output.lines() {
        let Some(ip) = line
            .split_ascii_whitespace()
            .next()
            .and_then(|address| address.parse::<IpAddr>().ok())
        else {
            continue;
        };
        let address = SocketAddr::new(ip, port);
        if !addresses.contains(&address) {
            addresses.push(address);
        }
        if addresses.len() == 16 {
            break;
        }
    }
    if addresses.is_empty() {
        return Err("peer hostname has no usable address".into());
    }
    Ok(addresses)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_connect_is_cancelled_without_waiting_for_its_dial_deadline() {
        // A private loopback listener with a one-entry backlog supplies an
        // actual pending TCP connect without depending on external routing.
        let listener = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .unwrap();
        listener
            .bind(&"127.0.0.1:0".parse::<SocketAddr>().unwrap().into())
            .unwrap();
        listener.listen(1).unwrap();
        let address = listener.local_addr().unwrap().as_socket().unwrap();
        let mut queued = Vec::new();
        for _ in 0..8 {
            match TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
                Ok(stream) => queued.push(stream),
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => break,
                Err(error) => panic!("private backlog setup failed: {error}"),
            }
        }
        assert!(
            !queued.is_empty() && queued.len() < 8,
            "private backlog was not saturated"
        );
        let mut polls = 0;
        let start = Instant::now();
        let error = connect(address, start + Duration::from_secs(10), || {
            polls += 1;
            polls < 4
        })
        .unwrap_err();
        assert_eq!(error, "sync dial cancelled");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(
            connect(address, Instant::now() + Duration::from_secs(1), || false)
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(
            connect(address, Instant::now(), || true)
                .unwrap_err()
                .contains("deadline")
        );
    }

    #[test]
    fn ip_literals_and_numeric_or_named_ipv6_scopes_are_preserved() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            resolve("127.0.0.1:80", deadline, || true).unwrap(),
            ["127.0.0.1:80".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(
            resolve("[fe80::1234%7]:53249", deadline, || true).unwrap(),
            [SocketAddrV6::new("fe80::1234".parse().unwrap(), 53249, 0, 7).into()]
        );
        let named = resolve("[fe80::1234%lo]:53249", deadline, || true).unwrap();
        assert!(matches!(named[0], SocketAddr::V6(address) if address.scope_id() != 0));
        for address in [
            "bad;hostname:8",
            "[fe80::1%../../lo]:8",
            "host:not-a-port",
            "host:0",
            "[invalid]:8",
            "host:65536",
        ] {
            assert!(resolve(address, deadline, || true).is_err());
        }
    }
    #[test]
    fn dns_answers_are_typed_deduplicated_and_bounded() {
        let answers = parse_answers(b"127.0.0.1 STREAM localhost\n127.0.0.1 DGRAM localhost\n::1 STREAM localhost\ninvalid garbage\n", 9).unwrap();
        assert_eq!(
            answers,
            [
                "127.0.0.1:9".parse::<SocketAddr>().unwrap(),
                "[::1]:9".parse().unwrap()
            ]
        );
        let many = (1..100)
            .map(|index| format!("192.0.2.{index} STREAM\n"))
            .collect::<String>();
        assert_eq!(parse_answers(many.as_bytes(), 9).unwrap().len(), 16);
        assert!(parse_answers(&[255], 9).is_err());
    }
}
