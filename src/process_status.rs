//! Read-only Linux process identity and listening-socket evidence.
//! These checks never authorize signalling or adopting a process.
use std::collections::HashSet;
use std::io::Read;

fn proc_text(path: &str, limit: u64) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= limit).then_some(text)
}

pub(crate) fn start_ticks(pid: u32) -> Option<u64> {
    let stat = proc_text(&format!("/proc/{pid}/stat"), 8192)?;
    // comm can contain spaces and parentheses; fields resume after its last ).
    stat.rsplit_once(") ")?
        .1
        .split_ascii_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

pub(crate) fn listening_on(pid: u32, started: u64, port: u16) -> bool {
    if start_ticks(pid) != Some(started) {
        return false;
    }
    let mut inodes = HashSet::new();
    for protocol in ["tcp", "tcp6"] {
        let Some(text) = proc_text(&format!("/proc/{pid}/net/{protocol}"), 1024 * 1024) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<_> = line.split_ascii_whitespace().take(10).collect();
            if fields.len() != 10 || fields[3] != "0A" {
                continue;
            }
            let Some((_, encoded)) = fields[1].rsplit_once(':') else {
                continue;
            };
            if u16::from_str_radix(encoded, 16).ok() == Some(port)
                && let Ok(inode) = fields[9].parse::<u64>()
            {
                inodes.insert(inode);
                if inodes.len() > 32 {
                    return false;
                }
            }
        }
    }
    if inodes.is_empty() {
        return false;
    }
    let Ok(files) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    for file in files.take(4096).flatten() {
        let Ok(target) = std::fs::read_link(file.path()) else {
            continue;
        };
        let Some(inode) = target
            .to_str()
            .and_then(|path| path.strip_prefix("socket:["))
            .and_then(|path| path.strip_suffix(']'))
            .and_then(|path| path.parse::<u64>().ok())
        else {
            continue;
        };
        if inodes.contains(&inode) {
            return start_ticks(pid) == Some(started);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_requires_the_same_process_and_its_actual_listening_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let pid = std::process::id();
        let started = start_ticks(pid).unwrap();
        assert!(listening_on(pid, started, port));
        assert!(!listening_on(pid, started + 1, port));
        drop(listener);
        assert!(!listening_on(pid, started, port));
    }
}
