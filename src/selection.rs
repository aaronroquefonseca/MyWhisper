//! An owned Wayland selection. Data stays in memory; pipe transfers and
//! compositor acknowledgements have deadlines. Only the current source can
//! restore a borrowed selection, and restoration waits for a completed read.
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ed, ext_data_control_manager_v1 as em,
    ext_data_control_offer_v1 as eo, ext_data_control_source_v1 as es,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wd, zwlr_data_control_manager_v1 as wm,
    zwlr_data_control_offer_v1 as wo, zwlr_data_control_source_v1 as ws,
};

pub(crate) const UNSUPPORTED: &str =
    "compositor lacks data-control support required for owned clipboard delivery";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TYPES: usize = 16;
pub(crate) type Guard = Arc<dyn Fn() -> bool + Send + Sync>;
pub(crate) type Payload = Vec<(String, Arc<Vec<u8>>)>;
#[derive(Clone)]
enum Offer {
    Ext(eo::ExtDataControlOfferV1),
    Wlr(wo::ZwlrDataControlOfferV1),
}
impl Offer {
    fn id(&self) -> wayland_client::backend::ObjectId {
        match self {
            Self::Ext(p) => p.id(),
            Self::Wlr(p) => p.id(),
        }
    }
    fn receive(&self, mime: &str, fd: &impl AsFd) {
        match self {
            Self::Ext(p) => p.receive(mime.into(), fd.as_fd()),
            Self::Wlr(p) => p.receive(mime.into(), fd.as_fd()),
        }
    }
    fn destroy(&self) {
        match self {
            Self::Ext(p) => p.destroy(),
            Self::Wlr(p) => p.destroy(),
        }
    }
}
enum Manager {
    Ext(em::ExtDataControlManagerV1),
    Wlr(wm::ZwlrDataControlManagerV1),
}
enum Device {
    Ext(ed::ExtDataControlDeviceV1),
    Wlr(wd::ZwlrDataControlDeviceV1),
}
enum Source {
    Ext(es::ExtDataControlSourceV1),
    Wlr(ws::ZwlrDataControlSourceV1),
}
impl Source {
    fn offer(&self, mime: &str) {
        match self {
            Self::Ext(p) => p.offer(mime.into()),
            Self::Wlr(p) => p.offer(mime.into()),
        }
    }
    fn destroy(&self) {
        match self {
            Self::Ext(p) => p.destroy(),
            Self::Wlr(p) => p.destroy(),
        }
    }
}
#[derive(Clone)]
struct SourceData {
    generation: u64,
    payload: Payload,
}
struct Transfer {
    pipe: File,
    bytes: Arc<Vec<u8>>,
    offset: usize,
    deadline: Instant,
    generation: u64,
    count: bool,
}
#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    offers: HashMap<wayland_client::backend::ObjectId, Vec<String>>,
    selection: Option<Offer>,
    acknowledgement: u64,
    generation: u64,
    nonce: [u8; 16],
    source_alive: bool,
    owned: bool,
    armed: bool,
    completed: u64,
    transfers: Vec<Transfer>,
    error: Option<String>,
    finished: bool,
}
impl State {
    fn changed(&mut self, offer: Option<Offer>) {
        if let Some(previous) = self.selection.take() {
            self.offers.remove(&previous.id());
            previous.destroy();
        }
        self.selection = offer;
        self.owned = self.source_alive
            && self
                .selection
                .as_ref()
                .and_then(|o| self.offers.get(&o.id()))
                .is_some_and(|types| types.contains(&marker(self.generation, &self.nonce)));
    }
    fn send(&mut self, mime: String, fd: std::os::fd::OwnedFd, data: &SourceData) {
        let bytes = if mime == "x-kde-passwordManagerHint" {
            Some(Arc::new(b"secret".to_vec()))
        } else {
            data.payload
                .iter()
                .find(|(m, _)| *m == mime)
                .map(|(_, b)| b.clone())
        };
        let Some(bytes) = bytes else { return };
        let pipe = File::from(fd);
        if self.transfers.len() >= 8 || crate::child::nonblocking(&pipe).is_err() {
            self.error = Some("clipboard transfer capacity exceeded".into());
            return;
        }
        self.transfers.push(Transfer {
            pipe,
            bytes,
            offset: 0,
            deadline: Instant::now() + IO_TIMEOUT,
            generation: data.generation,
            count: self.armed
                && data.generation == self.generation
                && !mime.starts_with("application/x-mywhisper-selection-")
                && mime != "x-kde-passwordManagerHint",
        });
    }
    fn progress(&mut self) {
        let mut index = 0;
        while index < self.transfers.len() {
            let transfer = &mut self.transfers[index];
            let mut failed = false;
            match transfer.pipe.write(&transfer.bytes[transfer.offset..]) {
                Ok(n) => transfer.offset += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => failed = true,
            }
            let complete = transfer.offset == transfer.bytes.len();
            if !complete && Instant::now() >= transfer.deadline {
                failed = true;
            }
            if complete || failed {
                let transfer = self.transfers.swap_remove(index);
                if complete && transfer.count && transfer.generation == self.generation {
                    self.completed += 1;
                }
                if failed && transfer.count && transfer.generation == self.generation {
                    self.error = Some("clipboard recipient closed or stalled".into());
                }
            } else {
                index += 1;
            }
        }
    }
}
fn marker(generation: u64, nonce: &[u8; 16]) -> String {
    let nonce = nonce
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("application/x-mywhisper-selection-{nonce}-{generation}")
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if state.globals.len() >= 128 || interface.len() > 256 {
                state.error = Some("compositor global limit exceeded".into());
            } else {
                state.globals.push((name, interface, version));
            }
        }
    }
}
impl Dispatch<wl_callback::WlCallback, u64> for State {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        tag: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.acknowledgement = *tag;
    }
}
wayland_client::delegate_noop!(State: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(State: em::ExtDataControlManagerV1);
wayland_client::delegate_noop!(State: wm::ZwlrDataControlManagerV1);
macro_rules! handlers {
    ($device:ident, $device_type:ident, $offer:ident, $offer_type:ident, $source:ident, $source_type:ident, $variant:ident) => {
        impl Dispatch<$device::$device_type, ()> for State {
            fn event(state: &mut Self, _: &$device::$device_type, event: $device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
                match event {
                    $device::Event::DataOffer { id } => { state.offers.insert(id.id(), Vec::new()); },
                    $device::Event::Selection { id } => state.changed(id.map(Offer::$variant)),
                    $device::Event::PrimarySelection { id: Some(id) } => { state.offers.remove(&id.id()); id.destroy(); },
                    $device::Event::Finished => state.finished = true,
                    _ => {},
                }
            }
            wayland_client::event_created_child!(State, $device::$device_type, [0 => ($offer::$offer_type, ())]);
        }
        impl Dispatch<$offer::$offer_type, ()> for State {
            fn event(state: &mut Self, proxy: &$offer::$offer_type, event: $offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
                if let $offer::Event::Offer { mime_type } = event {
                    let types = state.offers.entry(proxy.id()).or_default();
                    if types.len() >= 64 || mime_type.len() > 256 { state.error = Some("clipboard offer limit exceeded".into()); }
                    else { types.push(mime_type); }
                }
            }
        }
        impl Dispatch<$source::$source_type, SourceData> for State {
            fn event(state: &mut Self, _: &$source::$source_type, event: $source::Event, data: &SourceData, _: &Connection, _: &QueueHandle<Self>) {
                match event {
                    $source::Event::Send { mime_type, fd } => state.send(mime_type, fd, data),
                    $source::Event::Cancelled if data.generation == state.generation => { state.source_alive = false; state.owned = false; },
                    _ => {},
                }
            }
        }
    };
}
handlers!(
    ed,
    ExtDataControlDeviceV1,
    eo,
    ExtDataControlOfferV1,
    es,
    ExtDataControlSourceV1,
    Ext
);
handlers!(
    wd,
    ZwlrDataControlDeviceV1,
    wo,
    ZwlrDataControlOfferV1,
    ws,
    ZwlrDataControlSourceV1,
    Wlr
);

pub(crate) struct Selection {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    manager: Manager,
    device: Device,
    source: Option<Source>,
    next_ack: u64,
}
impl Selection {
    pub fn open(guard: &Guard) -> Result<Self, String> {
        let connection = connect_compositor(guard)?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let mut state = State::default();
        getrandom::fill(&mut state.nonce).map_err(|_| "clipboard ownership entropy unavailable")?;
        connection.display().sync(&qh, 1);
        pump_until(&connection, &mut queue, &mut state, guard, |s| {
            s.acknowledgement == 1
        })?;
        let seat = state
            .globals
            .iter()
            .find(|(_, name, _)| name == "wl_seat")
            .ok_or("compositor has no seat")?;
        let seat: wl_seat::WlSeat = registry.bind(seat.0, seat.2.min(2), &qh, ());
        let (manager, device) = if let Some((name, _, _)) = state
            .globals
            .iter()
            .find(|(_, interface, _)| interface == "ext_data_control_manager_v1")
        {
            let manager: em::ExtDataControlManagerV1 = registry.bind(*name, 1, &qh, ());
            let device = manager.get_data_device(&seat, &qh, ());
            (Manager::Ext(manager), Device::Ext(device))
        } else if let Some((name, _, version)) = state
            .globals
            .iter()
            .find(|(_, interface, _)| interface == "zwlr_data_control_manager_v1")
        {
            let manager: wm::ZwlrDataControlManagerV1 =
                registry.bind(*name, (*version).min(2), &qh, ());
            let device = manager.get_data_device(&seat, &qh, ());
            (Manager::Wlr(manager), Device::Wlr(device))
        } else {
            return Err(
                "compositor lacks data-control support required for owned clipboard delivery"
                    .into(),
            );
        };
        let mut result = Self {
            connection,
            queue,
            state,
            manager,
            device,
            source: None,
            next_ack: 1,
        };
        result.sync(guard)?;
        Ok(result)
    }
    fn sync(&mut self, guard: &Guard) -> Result<(), String> {
        self.next_ack += 1;
        let tag = self.next_ack;
        self.connection.display().sync(&self.queue.handle(), tag);
        pump_until(
            &self.connection,
            &mut self.queue,
            &mut self.state,
            guard,
            |s| s.acknowledgement == tag,
        )
    }
    pub fn tick(&mut self) -> Result<(), String> {
        pump(&self.connection, &mut self.queue, &mut self.state)
    }
    pub fn snapshot(&mut self, guard: &Guard) -> Result<Payload, String> {
        self.sync(guard)?;
        let Some(offer) = self.state.selection.clone() else {
            return Ok(Vec::new());
        };
        let types = self
            .state
            .offers
            .get(&offer.id())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|mime| {
                !mime.starts_with("application/x-mywhisper-selection-")
                    && mime != "x-kde-passwordManagerHint"
            })
            .collect::<Vec<_>>();
        if types.len() > MAX_TYPES {
            return Err("clipboard has too many formats to preserve safely".into());
        }
        let mut payload = Vec::new();
        let mut total: usize = 0;
        for mime in types {
            let (read, write) =
                std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
            read.set_nonblocking(true).map_err(|e| e.to_string())?;
            offer.receive(&mime, &write);
            drop(write);
            let mut read = read;
            let deadline = Instant::now() + IO_TIMEOUT;
            let mut bytes = Vec::new();
            loop {
                check_guard(guard)?;
                self.tick()?;
                if self.state.selection.as_ref().map(Offer::id) != Some(offer.id()) {
                    return Err("clipboard changed while preserving it; retry delivery".into());
                }
                let mut buffer = [0; 8192];
                match read.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        total += n;
                        if total > MAX_BYTES {
                            return Err("clipboard exceeds preservation limit".into());
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err("clipboard snapshot transfer failed".into()),
                }
                if Instant::now() >= deadline {
                    return Err("clipboard snapshot deadline exceeded".into());
                }
            }
            payload.push((mime, Arc::new(bytes)));
        }
        Ok(payload)
    }
    pub fn copy(
        &mut self,
        mut payload: Payload,
        sensitive: bool,
        guard: &Guard,
    ) -> Result<(), String> {
        check_guard(guard)?;
        let size = payload
            .iter()
            .try_fold(0usize, |total, (_, bytes)| total.checked_add(bytes.len()))
            .ok_or("clipboard size overflow")?;
        if size > MAX_BYTES || payload.len() > MAX_TYPES {
            return Err("clipboard payload limit exceeded".into());
        }
        self.state.generation += 1;
        self.state.owned = false;
        self.state.source_alive = true;
        self.state.armed = false;
        self.state.completed = 0;
        self.state.error = None;
        if let Some(source) = self.source.take() {
            source.destroy();
        }
        if payload.is_empty() {
            match &self.device {
                Device::Ext(p) => p.set_selection(None),
                Device::Wlr(p) => p.set_selection(None),
            }
            self.sync(guard)?;
            return Ok(());
        }
        payload.push((
            marker(self.state.generation, &self.state.nonce),
            Arc::new(Vec::new()),
        ));
        let data = SourceData {
            generation: self.state.generation,
            payload,
        };
        let source = match &self.manager {
            Manager::Ext(p) => {
                Source::Ext(p.create_data_source(&self.queue.handle(), data.clone()))
            }
            Manager::Wlr(p) => {
                Source::Wlr(p.create_data_source(&self.queue.handle(), data.clone()))
            }
        };
        for (mime, _) in &data.payload {
            source.offer(mime);
        }
        if sensitive {
            source.offer("x-kde-passwordManagerHint");
        }
        match (&self.device, &source) {
            (Device::Ext(device), Source::Ext(source)) => device.set_selection(Some(source)),
            (Device::Wlr(device), Source::Wlr(source)) => device.set_selection(Some(source)),
            _ => return Err("clipboard protocol mismatch".into()),
        }
        self.source = Some(source);
        self.sync(guard)?;
        if !self.state.owned {
            return Err("clipboard selection was replaced before delivery".into());
        }
        Ok(())
    }
    pub fn arm(&mut self, guard: &Guard) -> Result<(), String> {
        self.sync(guard)?;
        if !self.state.owned {
            return Err("clipboard changed before paste".into());
        }
        self.state.completed = 0;
        self.state.armed = true;
        Ok(())
    }
    pub fn consumed(&mut self, guard: &Guard) -> Result<(), String> {
        pump_until(
            &self.connection,
            &mut self.queue,
            &mut self.state,
            guard,
            |s| !s.owned || s.completed > 0,
        )?;
        if self.state.completed == 0 {
            return Err("clipboard changed before the transcript was consumed".into());
        }
        Ok(())
    }
    pub fn restore_if_owned(&mut self, payload: Payload, guard: &Guard) -> Result<(), String> {
        // A fresh compositor acknowledgement processes cancellation before a
        // restoration request. A user's replacement is never our authority.
        self.sync(guard)?;
        if self.state.owned {
            self.copy(payload, false, guard)?;
        }
        Ok(())
    }
    pub fn clear_if_owned(&mut self) {
        // Cancelled delivery must drop its sensitive offer without replacing
        // another client's selection. Destroying the source achieves this.
        if let Some(source) = self.source.take() {
            source.destroy();
        }
        self.state.transfers.clear();
        self.state.owned = false;
        self.state.source_alive = false;
        let _ = self.connection.flush();
    }
}
fn connect_compositor(guard: &Guard) -> Result<Connection, String> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::{ffi::OsStrExt, net::UnixStream};
    let name = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or("clipboard worker needs a WAYLAND_DISPLAY socket path")?;
    let name = std::path::PathBuf::from(name);
    let path = if name.is_absolute() {
        name
    } else {
        let directory = std::path::PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").ok_or("Wayland runtime directory unavailable")?,
        );
        if !directory.is_absolute() {
            return Err("Wayland runtime directory must be absolute".into());
        }
        directory.join(name)
    };
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is initialized before use; the owned socket closes
    // on every error. Nonblocking connect is retried within one deadline.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err("invalid compositor socket path".into());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, byte) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *byte as libc::c_char;
    }
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if descriptor < 0 {
        return Err("cannot create compositor socket".into());
    }
    let socket = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        check_guard(guard)?;
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
                    as libc::socklen_t,
            )
        };
        let error = std::io::Error::last_os_error();
        if result == 0 || error.raw_os_error() == Some(libc::EISCONN) {
            break;
        }
        if !matches!(
            error.raw_os_error(),
            Some(libc::EAGAIN | libc::EINPROGRESS | libc::EALREADY | libc::EINTR)
        ) {
            return Err("cannot connect to the Wayland compositor".into());
        }
        if Instant::now() >= deadline {
            return Err("compositor connection deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Connection::from_socket(UnixStream::from(socket))
        .map_err(|_| "cannot initialize compositor connection".into())
}

fn check_guard(guard: &Guard) -> Result<(), String> {
    if guard() {
        Ok(())
    } else {
        Err("clipboard delivery cancelled".into())
    }
}
fn pump_until(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    guard: &Guard,
    done: impl Fn(&State) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        check_guard(guard)?;
        pump(connection, queue, state)?;
        if done(state) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("clipboard compositor or recipient deadline exceeded".into());
        }
    }
}
fn pump(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
) -> Result<(), String> {
    queue
        .dispatch_pending(state)
        .map_err(|_| "clipboard compositor disconnected")?;
    state.progress();
    if state.finished {
        return Err("clipboard seat was removed".into());
    }
    if let Some(error) = state.error.take() {
        return Err(error);
    }
    connection
        .flush()
        .map_err(|_| "clipboard compositor write failed")?;
    if let Some(read) = queue.prepare_read() {
        let mut fd = libc::pollfd {
            fd: connection.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: this is a live owned connection and one stack poll descriptor.
        let ready = unsafe { libc::poll(&mut fd, 1, 2) };
        if ready > 0 {
            read.read()
                .map_err(|_| "clipboard compositor read failed")?;
        } else if ready < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return Err("clipboard polling failed".into());
        }
    }
    queue
        .dispatch_pending(state)
        .map_err(|_| "clipboard compositor disconnected")?;
    state.progress();
    Ok(())
}

/// Compatibility for compositors whose wl-copy implementation uses a focused
/// popup instead of data-control. Keep the foreground owner, acknowledge its
/// selection by reading it, and reap the entire group when it is replaced.
/// Borrow/restore operations require native source-cancellation events.
pub(crate) struct CliSelection {
    child: crate::child::OwnedChild,
}
impl CliSelection {
    pub fn copy(payload: &Payload, sensitive: bool, guard: &Guard) -> Result<Self, String> {
        use std::process::{Command, Stdio};
        check_guard(guard)?;
        let (mime, bytes) = payload.first().ok_or("empty fallback clipboard payload")?;
        if bytes.len() > MAX_BYTES {
            return Err("clipboard payload limit exceeded".into());
        }
        let mut command = Command::new("wl-copy");
        command.args(["--foreground", "--type", mime]);
        if sensitive {
            command.arg("--sensitive");
        }
        command.arg("--");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = crate::child::OwnedChild::spawn(&mut command)?;
        let mut input = child.0.stdin.take().ok_or("clipboard stdin unavailable")?;
        crate::child::nonblocking(&input).map_err(|_| "cannot configure clipboard input")?;
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut written = 0;
        while written < bytes.len() {
            check_guard(guard)?;
            if child
                .try_status()
                .map_err(|_| "clipboard process status unavailable")?
                .is_some()
            {
                return Err("wl-copy exited before owning the clipboard".into());
            }
            match input.write(&bytes[written..]) {
                Ok(0) => return Err("clipboard input closed".into()),
                Ok(n) => written += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err("clipboard input failed".into()),
            }
            if Instant::now() >= deadline {
                return Err("clipboard input deadline exceeded".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(input);
        while Instant::now() < deadline {
            check_guard(guard)?;
            if child
                .try_status()
                .map_err(|_| "clipboard process status unavailable")?
                .is_some()
            {
                return Err("wl-copy failed or lost the clipboard selection".into());
            }
            let result = crate::child::run_guarded(
                Command::new("wl-paste").args(["--no-newline", "--type", mime]),
                &[],
                deadline.saturating_duration_since(Instant::now()),
                MAX_BYTES,
                || guard(),
            );
            if result.as_ref().is_ok_and(|actual| actual == bytes.as_ref())
                && child
                    .try_status()
                    .map_err(|_| "clipboard process status unavailable")?
                    .is_none()
            {
                return Ok(Self { child });
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Err("clipboard selection readiness deadline exceeded".into())
    }
    pub fn alive(&self) -> bool {
        self.child.try_status().is_ok_and(|status| status.is_none())
    }
}

#[cfg(test)]
mod regressions {
    use super::*;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    fn data(generation: u64, bytes: Vec<u8>) -> SourceData {
        SourceData {
            generation,
            payload: vec![("text/plain".into(), Arc::new(bytes))],
        }
    }
    #[test]
    fn partial_unicode_transfer_completes_only_after_all_bytes_and_eof() {
        let bytes = "🦀 синтетичний transcript\n".repeat(20_000).into_bytes();
        let mut state = State {
            generation: 3,
            armed: true,
            ..Default::default()
        };
        let (writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        state.send(
            "text/plain".into(),
            OwnedFd::from(writer),
            &data(3, bytes.clone()),
        );
        state.progress();
        assert_eq!(
            state.completed, 0,
            "a partially buffered transfer is not completion"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = Vec::new();
        loop {
            state.progress();
            let mut buffer = [0; 1337];
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("{error}"),
            }
            assert!(Instant::now() < deadline);
        }
        assert_eq!(received, bytes);
        assert_eq!(state.completed, 1);
        assert!(state.transfers.is_empty());
    }
    #[test]
    fn clipboard_preloads_markers_and_old_sources_do_not_complete_current_paste() {
        let mut state = State {
            generation: 9,
            ..Default::default()
        };
        let (write, _read) = UnixStream::pair().unwrap();
        state.send(
            "text/plain".into(),
            write.into(),
            &data(9, b"preload".to_vec()),
        );
        state.armed = true;
        state.progress();
        assert_eq!(state.completed, 0);
        let (write, _read) = UnixStream::pair().unwrap();
        state.send(
            "text/plain".into(),
            write.into(),
            &data(8, b"older".to_vec()),
        );
        let mime = marker(9, &state.nonce);
        let (write, _read) = UnixStream::pair().unwrap();
        state.send(
            mime.clone(),
            write.into(),
            &SourceData {
                generation: 9,
                payload: vec![(mime, Arc::new(Vec::new()))],
            },
        );
        state.progress();
        assert_eq!(state.completed, 0);
        let (write, _read) = UnixStream::pair().unwrap();
        state.send(
            "text/plain".into(),
            write.into(),
            &data(9, b"current".to_vec()),
        );
        state.progress();
        assert_eq!(state.completed, 1);
    }
    #[test]
    fn stalled_closed_and_excessive_recipients_release_every_descriptor() {
        let mut state = State {
            generation: 1,
            armed: true,
            ..Default::default()
        };
        let source = data(1, vec![1; 1024 * 1024]);
        let mut readers = Vec::new();
        for _ in 0..8 {
            let (write, read) = UnixStream::pair().unwrap();
            readers.push(read);
            state.send("text/plain".into(), write.into(), &source);
        }
        assert_eq!(state.transfers.len(), 8);
        let (write, mut read) = UnixStream::pair().unwrap();
        state.send("text/plain".into(), write.into(), &source);
        assert!(state.error.take().unwrap().contains("capacity"));
        let mut byte = [0];
        assert_eq!(
            read.read(&mut byte).unwrap(),
            0,
            "rejected recipient FD must close"
        );
        for transfer in &mut state.transfers {
            transfer.deadline = Instant::now();
        }
        state.progress();
        assert!(state.transfers.is_empty());
        assert_eq!(state.completed, 0);
        assert!(state.error.take().unwrap().contains("stalled"));
        let (write, read) = UnixStream::pair().unwrap();
        drop(read);
        state.send("text/plain".into(), write.into(), &source);
        state.progress();
        assert!(state.transfers.is_empty());
        assert_eq!(state.completed, 0);
        drop(readers);
    }
    #[test]
    fn cancellation_interrupts_a_compositor_that_never_answers() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (socket, _server) = UnixStream::pair().unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let mut queue = connection.new_event_queue();
        let mut state = State::default();
        connection.display().sync(&queue.handle(), 1);
        let permitted = Arc::new(AtomicBool::new(true));
        let live = permitted.clone();
        let guard: Guard = Arc::new(move || live.load(Ordering::Acquire));
        let cancellation = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            permitted.store(false, Ordering::Release);
        });
        let started = Instant::now();
        assert!(
            pump_until(&connection, &mut queue, &mut state, &guard, |s| s
                .acknowledgement
                == 1)
            .unwrap_err()
            .contains("cancelled")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        cancellation.join().unwrap();
    }
}
