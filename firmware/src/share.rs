//! The card over Wi-Fi while its dialog is open.
//!
//! The dialog raises [`OPEN`]; the Wi-Fi task then runs an access point beside its station and
//! leaves out its scans, because a scan switches channels, stalls a download and drops a joined
//! client for seconds. [`busy`] says a file is on its way, for the BLE scans and the background. [`serve`] runs all the time: without the access
//! point its sockets simply hear nothing.
//!
//! A client gets an address over DHCP and **no gateway**, so a phone keeps its own route to the
//! internet. `GET` on a directory answers a listing, on a file the file. `PUT` uploads a file,
//! `MKCOL` makes a directory and `DELETE` removes a file or an empty directory; the listing's page
//! drives all three and sends the times, since the Knob has no clock.

use alloc::format;
use alloc::string::String;

use core::cell::Cell;
use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use edge_dhcp::server::{Server as DhcpServer, ServerOptions};
use edge_dhcp::{Options, Packet};
use embassy_futures::join::join3;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Address, Ipv4Cidr, Stack, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant};
use fatfs::{Date, DateTime, Time};
use log::{error, info, warn};
use teetotum::fat::{self, Volume};
use teetotum::sd::{self, SdCard};

use crate::storage::{self, FsError, FsFile};

/// Whether the dialog is open, and with it the access point. Set by the device loop.
pub static OPEN: AtomicBool = AtomicBool::new(false);

/// Files on their way to or from a client right now.
static SENDING: AtomicU8 = AtomicU8::new(0);

/// Whether a file is on its way to or from a client.
pub fn busy() -> bool {
    SENDING.load(Ordering::Relaxed) > 0
}

/// The access point's own address, and the DHCP server's.
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
/// What a client opens, as the dialog shows it.
pub const HOST: &str = "192.168.4.1";

/// Sockets the IP stack keeps room for: DHCP and the HTTP ones.
pub const SOCKETS: usize = 1 + HTTP_SOCKETS;
/// Connections served at once. Browsers open a second one on their own, for the icon or ahead
/// of the next click, and with one socket that one would hold up the real request.
const HTTP_SOCKETS: usize = 2;
/// TCP receive buffer. An upload arrives about five times faster than the card takes it, and a
/// card write blocks the executor, so how much the window can swallow during one write sets the
/// rate: 8 KiB gives 36 KiB/s, 64 KiB gives 54 KiB/s over a megabyte.
const TCP_RX_BUFFER: usize = 65536;
/// TCP transmit buffer. A download is not held up this way: the card reads faster than the air.
const TCP_TX_BUFFER: usize = 8192;
/// What a request head may take; a longer one is refused.
const HEAD: usize = 2048;
/// Bytes read off the card per write to the socket: one 4 KiB cluster, which the reader fetches
/// in a single command, where smaller pieces pay a command and a stop each.
const CHUNK: usize = 4096;
/// DHCP socket buffer, each way, and the server's scratch for a message.
const DHCP_BUFFER: usize = 1024;
/// Leases the DHCP server keeps.
const LEASES: usize = 4;
/// How long a connection may sit without progress.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The bytes [`serve`] takes for its buffers. They are only copied by the CPU, so external RAM
/// does: the driver builds every frame in its own memory.
pub const BUFFER_BYTES: usize =
    HTTP_SOCKETS * (TCP_RX_BUFFER + TCP_TX_BUFFER + HEAD + CHUNK) + 4 * DHCP_BUFFER;

/// The IP configuration of the access point interface.
pub fn ip_config() -> embassy_net::Config {
    let [a, b, c, d] = ADDRESS.octets();
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(a, b, c, d), 24),
        gateway: None,
        dns_servers: Default::default(),
    })
}

/// The network's name and password, made once per boot.
pub struct Credentials {
    pub ssid: String,
    pub password: String,
}

impl Credentials {
    /// The name from the access point's MAC, so two knobs differ; the password from `random`.
    pub fn new(mac: [u8; 6], random: [u8; 6]) -> Self {
        let mut password = String::new();
        for byte in random {
            let _ = write!(password, "{byte:02x}");
        }
        Self {
            ssid: format!("TeeToTum-{:02X}{:02X}", mac[4], mac[5]),
            password,
        }
    }

    /// The text of a QR code a phone joins the network with.
    pub fn join_text(&self) -> String {
        format!(
            "WIFI:T:WPA;S:{};P:{};;",
            escape_wifi(&self.ssid),
            escape_wifi(&self.password)
        )
    }
}

/// Escapes what the `WIFI:` scheme reserves.
fn escape_wifi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

type Card<'d> = Mutex<NoopRawMutex, Option<Volume<'d>>>;

/// Answers DHCP and HTTP on `stack` for as long as it runs, which is for ever.
///
/// `area` holds every buffer and must be [`BUFFER_BYTES`] long.
pub async fn serve(stack: Stack<'_>, area: &mut [u8], volume: Option<Volume<'_>>) {
    assert_eq!(area.len(), BUFFER_BYTES, "the share's buffers");
    let card: Card<'_> = Mutex::new(volume);

    let (dhcp_area, http_area) = area.split_at_mut(4 * DHCP_BUFFER);
    let (first, second) = http_area.split_at_mut(http_area.len() / 2);

    join3(
        dhcp(stack, dhcp_area),
        http(stack, first, &card),
        http(stack, second, &card),
    )
    .await;
}

/// Hands out addresses. The lease table lives in this future, the buffers in `area`.
async fn dhcp(stack: Stack<'_>, area: &mut [u8]) {
    let (sockets, scratch) = area.split_at_mut(2 * DHCP_BUFFER);
    let (rx, tx) = sockets.split_at_mut(DHCP_BUFFER);
    let (incoming, outgoing) = scratch.split_at_mut(DHCP_BUFFER);
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, rx, &mut tx_meta, tx);
    if let Err(err) = socket.bind(67) {
        error!("Share: DHCP could not bind: {err:?}");
        return;
    }
    let mut leases = DhcpServer::<fn() -> u64, LEASES>::new(now_secs, ADDRESS);
    // No gateway: the knob must never become a client's route anywhere.
    let options = ServerOptions::new(ADDRESS, None);

    loop {
        let len = match socket.recv_from(incoming).await {
            Ok((len, _)) => len,
            Err(err) => {
                warn!("Share: DHCP receive: {err:?}");
                continue;
            }
        };
        let Ok(request) = Packet::decode(&incoming[..len]) else {
            continue;
        };
        let mut option_buf = Options::buf();
        let Some(reply) = leases.handle_request(&mut option_buf, &options, &request) else {
            continue;
        };
        let bytes = match reply.encode(outgoing) {
            Ok(bytes) => bytes,
            Err(err) => {
                warn!("Share: DHCP encode: {err:?}");
                continue;
            }
        };
        // A client without an address only hears a broadcast.
        let to = if !request.ciaddr.is_unspecified() && !request.broadcast {
            request.ciaddr
        } else {
            Ipv4Addr::BROADCAST
        };
        match socket.send_to(bytes, (to, 68)).await {
            Ok(()) => info!("Share: DHCP offered {}", reply.yiaddr),
            Err(err) => warn!("Share: DHCP send: {err:?}"),
        }
    }
}

fn now_secs() -> u64 {
    Instant::now().as_secs()
}

/// One HTTP socket, answering one connection after another.
async fn http(stack: Stack<'_>, area: &mut [u8], card: &Card<'_>) {
    let (rx, rest) = area.split_at_mut(TCP_RX_BUFFER);
    let (tx, rest) = rest.split_at_mut(TCP_TX_BUFFER);
    let (head, chunk) = rest.split_at_mut(HEAD);
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(TIMEOUT));

    loop {
        if let Err(err) = socket.accept(80).await {
            warn!("Share: accept: {err:?}");
            continue;
        }
        let len = read_head(&mut socket, head).await;
        let _ = answer(&mut socket, &head[..len], chunk, card).await;
        socket.close();
        let _ = socket.flush().await;
        socket.abort();
    }
}

/// Reads until the blank line that ends a request head, or until `head` is full.
async fn read_head(socket: &mut TcpSocket<'_>, head: &mut [u8]) -> usize {
    let mut len = 0;
    while len < head.len() {
        match socket.read(&mut head[len..]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => len += n,
        }
        if head[..len].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    len
}

/// A write to the socket failed; the connection is gone.
struct Gone;

async fn answer(
    socket: &mut TcpSocket<'_>,
    head: &[u8],
    chunk: &mut [u8],
    card: &Card<'_>,
) -> Result<(), Gone> {
    let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") else {
        return status(socket, "431 Request Header Fields Too Large").await;
    };
    let (lines, body) = (&head[..end], &head[end + 4..]);
    let mut request = lines
        .split(|&b| b == b'\r')
        .next()
        .unwrap_or_default()
        .split(|&b| b == b' ');
    let (Some(method), Some(target)) = (request.next(), request.next()) else {
        return status(socket, "400 Bad Request").await;
    };
    let (target, query) = match target.iter().position(|&b| b == b'?') {
        Some(at) => (&target[..at], &target[at + 1..]),
        None => (target, &[][..]),
    };
    let Some(path) = percent_decode(target) else {
        return status(socket, "400 Bad Request").await;
    };
    match method {
        b"GET" => {}
        b"PUT" => return upload(socket, lines, query, &path, body, chunk, card).await,
        b"MKCOL" | b"DELETE" => return change(socket, method, query, &path, card).await,
        _ => return status(socket, "405 Method Not Allowed").await,
    }

    // Look the path up with the card held only as long as that takes.
    let found = {
        let mut guard = card.lock().await;
        let Some(volume) = guard.as_mut() else {
            return status(socket, "503 Service Unavailable").await;
        };
        match volume.dir(&path) {
            Ok(dir) => Ok(Found::Dir(dir)),
            Err(fat::Error::WrongKind) => volume.entry(&path).map(Found::File),
            Err(err) => Err(err),
        }
    };
    match found {
        Ok(Found::Dir(dir)) => listing(socket, &path, dir, card).await,
        Ok(Found::File(entry)) => send_file(socket, &path, entry, chunk, card).await,
        Err(fat::Error::NotFound | fat::Error::NotADirectory) => {
            status(socket, "404 Not Found").await
        }
        Err(err) => {
            error!("Share: {path}: {err:?}");
            status(socket, "500 Internal Server Error").await
        }
    }
}

enum Found {
    Dir(fat::Dir),
    File(fat::Entry),
}

async fn status(socket: &mut TcpSocket<'_>, line: &str) -> Result<(), Gone> {
    explain(socket, line, line).await
}

/// A status with a sentence the page shows as it is.
async fn explain(socket: &mut TcpSocket<'_>, line: &str, text: &str) -> Result<(), Gone> {
    let response = format!(
        "HTTP/1.1 {line}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}\n",
        text.len() + 1
    );
    write_all(socket, response.as_bytes()).await
}

/// What a `PUT` announced.
struct Incoming<'r> {
    name: &'r str,
    created: DateTime,
    modified: DateTime,
    length: u32,
    /// Body bytes that came with the head.
    first: &'r [u8],
}

/// How an upload ended, if not with the file on the card.
enum Received {
    Gone,
    Fs(FsError),
}

/// Receives a file into `path`, replacing one of that name. A file that does not arrive whole is
/// removed again.
async fn upload(
    socket: &mut TcpSocket<'_>,
    lines: &[u8],
    query: &[u8],
    path: &str,
    first: &[u8],
    chunk: &mut [u8],
    card: &Card<'_>,
) -> Result<(), Gone> {
    let Some(name) = target_name(path) else {
        return status(socket, "400 Bad Request").await;
    };
    let (Ok(created), Ok(modified)) = (time_param(query, "created"), time_param(query, "modified"))
    else {
        return status(socket, "400 Bad Request").await;
    };
    let Some(length) = header(lines, "content-length").and_then(|v| v.parse::<u32>().ok()) else {
        return status(socket, "411 Length Required").await;
    };
    let incoming = Incoming {
        name,
        created,
        modified,
        length,
        first,
    };

    let _sending = Sending::start();
    let began = Instant::now();
    let mut writing = Duration::from_ticks(0);
    let failure = Cell::new(None);
    let outcome = {
        let mut guard = card.lock().await;
        let Some((mut sd, start)) = take_card(&mut guard) else {
            return status(socket, "503 Service Unavailable").await;
        };
        let outcome = receive(
            socket,
            &mut sd,
            start,
            &failure,
            &incoming,
            chunk,
            &mut writing,
        )
        .await;
        give_back(&mut guard, sd);
        outcome
    };
    if let Some(err) = failure.take() {
        error!("Share: {path}: the card failed after the upload: {err:?}");
        return explain(socket, "500 Internal Server Error", "the card failed").await;
    }
    match outcome {
        Ok(()) => {
            let ms = began.elapsed().as_millis().max(1);
            info!(
                "Share: received {path}, {length} bytes in {ms} ms ({} KiB/s), card {} ms",
                u64::from(length) * 1000 / 1024 / ms,
                writing.as_millis()
            );
            status(socket, "201 Created").await
        }
        Err(Received::Gone) => {
            warn!("Share: {path} did not arrive whole and was removed");
            Err(Gone)
        }
        Err(Received::Fs(err)) => {
            warn!("Share: receiving {path}: {err:?}");
            let (line, why) = refusal(&err);
            explain(socket, line, why).await
        }
    }
}

async fn receive(
    socket: &mut TcpSocket<'_>,
    sd: &mut SdCard<'_>,
    start: u32,
    failure: &Cell<Option<sd::Error>>,
    incoming: &Incoming<'_>,
    chunk: &mut [u8],
    writing: &mut Duration,
) -> Result<(), Received> {
    let fs = storage::mount(sd, start, incoming.created, failure).map_err(Received::Fs)?;
    let filled = {
        let root = fs.root_dir();
        let mut file = root.create_file(incoming.name).map_err(Received::Fs)?;
        let filled = fill(socket, &mut file, incoming, chunk, writing).await;
        drop(file);
        if filled.is_err()
            && let Err(err) = root.remove(incoming.name)
        {
            warn!("Share: removing what arrived of {}: {err:?}", incoming.name);
        }
        filled
    };
    filled.and(fs.unmount().map_err(Received::Fs))
}

/// Copies the body into `file` a chunk at a time, so the card is written in whole clusters.
async fn fill(
    socket: &mut TcpSocket<'_>,
    file: &mut FsFile<'_, '_, '_>,
    incoming: &Incoming<'_>,
    chunk: &mut [u8],
    writing: &mut Duration,
) -> Result<(), Received> {
    use fatfs::Write as _;

    file.truncate().map_err(Received::Fs)?;
    let mut left = incoming.length as usize;
    let mut got = incoming.first.len().min(left);
    chunk[..got].copy_from_slice(&incoming.first[..got]);
    while left > 0 {
        let want = left.min(chunk.len());
        while got < want {
            match socket.read(&mut chunk[got..want]).await {
                Ok(0) | Err(_) => return Err(Received::Gone),
                Ok(n) => got += n,
            }
        }
        let started = Instant::now();
        file.write_all(&chunk[..want]).map_err(Received::Fs)?;
        *writing += started.elapsed();
        left -= want;
        got = 0;
    }
    file.set_created(incoming.created);
    file.set_modified(incoming.modified);
    file.flush().map_err(Received::Fs)
}

/// `MKCOL` makes a directory, `DELETE` removes a file or an empty directory.
async fn change(
    socket: &mut TcpSocket<'_>,
    method: &[u8],
    query: &[u8],
    path: &str,
    card: &Card<'_>,
) -> Result<(), Gone> {
    let Some(name) = target_name(path) else {
        return status(socket, "400 Bad Request").await;
    };
    let Ok(created) = time_param(query, "created") else {
        return status(socket, "400 Bad Request").await;
    };
    let make = method == b"MKCOL";
    let failure = Cell::new(None);
    let outcome = {
        let mut guard = card.lock().await;
        let Some((mut sd, start)) = take_card(&mut guard) else {
            return status(socket, "503 Service Unavailable").await;
        };
        let outcome = alter(&mut sd, start, &failure, make, name, created);
        give_back(&mut guard, sd);
        outcome
    };
    if let Some(err) = failure.take() {
        error!("Share: {path}: the card failed: {err:?}");
        return explain(socket, "500 Internal Server Error", "the card failed").await;
    }
    match outcome {
        Ok(()) if make => {
            info!("Share: made {name}/");
            status(socket, "201 Created").await
        }
        Ok(()) => {
            info!("Share: removed {name}");
            status(socket, "200 OK").await
        }
        Err(err) => {
            warn!(
                "Share: {} {name}: {err:?}",
                if make { "making" } else { "removing" }
            );
            let (line, why) = refusal(&err);
            explain(socket, line, why).await
        }
    }
}

fn alter(
    sd: &mut SdCard<'_>,
    start: u32,
    failure: &Cell<Option<sd::Error>>,
    make: bool,
    name: &str,
    clock: DateTime,
) -> Result<(), FsError> {
    let fs = storage::mount(sd, start, clock, failure)?;
    {
        let root = fs.root_dir();
        if !make {
            root.remove(name)?;
        } else if root.open_dir(name).is_ok() {
            // `create_dir` would open it and call that a success.
            return Err(FsError::AlreadyExists);
        } else {
            root.create_dir(name)?;
        }
    }
    fs.unmount()
}

/// Takes the card from the reader for a writer, with the first sector of its filesystem.
fn take_card<'d>(slot: &mut Option<Volume<'d>>) -> Option<(SdCard<'d>, u32)> {
    let volume = slot.take()?;
    let start = volume.layout().partition_start;
    Some((volume.into_card(), start))
}

/// Mounts the reader again after a writer. A card that no longer mounts stays unavailable.
fn give_back<'d>(slot: &mut Option<Volume<'d>>, card: SdCard<'d>) {
    match Volume::mount(card) {
        Ok(volume) => *slot = Some(volume),
        Err(err) => error!("Share: the card does not mount again: {err:?}"),
    }
}

/// The response to a writer's failure, and a sentence for the page.
fn refusal(err: &FsError) -> (&'static str, &'static str) {
    match err {
        FsError::NotFound => ("404 Not Found", "the folder is not there"),
        FsError::AlreadyExists => ("409 Conflict", "that name is taken"),
        FsError::InvalidInput => (
            "409 Conflict",
            "a file or folder of that name is in the way",
        ),
        FsError::DirectoryIsNotEmpty => ("409 Conflict", "the folder is not empty"),
        FsError::InvalidFileNameLength | FsError::UnsupportedFileNameCharacter => {
            ("400 Bad Request", "the name does not fit the card")
        }
        FsError::NotEnoughSpace => ("507 Insufficient Storage", "the card is full"),
        _ => ("500 Internal Server Error", "the card failed"),
    }
}

/// The path a change names, without the slashes around it, if every part of it is a name.
fn target_name(path: &str) -> Option<&str> {
    let name = path.trim_matches('/');
    let named = !name.is_empty() && name.split('/').all(|part| !matches!(part, "" | "." | ".."));
    named.then_some(name)
}

/// A header's value, its name compared without case.
fn header<'h>(lines: &'h [u8], name: &str) -> Option<&'h str> {
    lines.split(|&b| b == b'\n').skip(1).find_map(|line| {
        let (key, value) = core::str::from_utf8(line).ok()?.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// The time a query gives for `key` as `YYYYMMDDhhmmss`; without one, 1980-01-01 00:00, the
/// first moment FAT can store. `Err` for a value that is not a time FAT can store.
fn time_param(query: &[u8], key: &str) -> Result<DateTime, ()> {
    let Some(value) = query
        .split(|&b| b == b'&')
        .find_map(|pair| pair.strip_prefix(key.as_bytes())?.strip_prefix(b"="))
    else {
        return Ok(DateTime::new(Date::new(1980, 1, 1), Time::new(0, 0, 0, 0)));
    };
    if value.len() != 14 || !value.iter().all(u8::is_ascii_digit) {
        return Err(());
    }
    let part = |at: usize, len: usize| {
        value[at..at + len]
            .iter()
            .fold(0u16, |n, d| n * 10 + u16::from(d - b'0'))
    };
    let (year, month, day) = (part(0, 4), part(4, 2), part(6, 2));
    let (hour, minute, second) = (part(8, 2), part(10, 2), part(12, 2));
    // `fatfs` asserts these ranges.
    if !(1980..=2107).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(());
    }
    Ok(DateTime::new(
        Date::new(year, month, day),
        Time::new(hour, minute, second, 0),
    ))
}

/// Lists a directory as a page of links with the controls that change it. Its length is not known
/// ahead, so the page ends where the connection closes.
async fn listing(
    socket: &mut TcpSocket<'_>,
    path: &str,
    dir: fat::Dir,
    card: &Card<'_>,
) -> Result<(), Gone> {
    let base = path.trim_matches('/');
    let title = if base.is_empty() {
        String::from("/")
    } else {
        format!("/{base}/")
    };
    let mut page = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
         <!doctype html><meta name=viewport content=\"width=device-width\">\
         <title>TeeToTum {0}</title>\
         <style>div{{overflow-x:auto}}table{{border-collapse:collapse}}\
         th,td{{padding:.15em 1em .15em 0;text-align:left;vertical-align:top}}\
         .n{{text-align:right}}span{{white-space:nowrap}}</style>\
         <h1>{0}</h1>\
         <p><input type=file multiple id=f onchange=up()> <button onclick=md()>New folder</button> \
         <span id=s></span><div><table>\
         <tr><th>Name<th class=n>Size<th>Created<th>Modified<th>Accessed<th>Attributes<th>",
        escape_html(&title)
    );
    if let Some(parent) = (!base.is_empty()).then(|| base.rsplit_once('/').map_or("", |(p, _)| p)) {
        let _ = write!(
            page,
            "<tr><td><a href=\"/{}\">..</a>",
            percent_encode(parent)
        );
    }
    write_all(socket, page.as_bytes()).await?;

    let mut entries = card.lock().await.as_mut().map(|volume| volume.entries(dir));
    let mut count = 0usize;
    loop {
        // One entry at a time, so a second connection can use the card in between.
        let next = {
            let mut guard = card.lock().await;
            match (guard.as_mut(), entries.as_mut()) {
                (Some(volume), Some(entries)) => entries.next(volume),
                _ => Ok(None),
            }
        };
        let entry = match next {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(err) => {
                error!("Share: listing {title}: {err:?}");
                break;
            }
        };
        let name = entry.name();
        if name == "." || name == ".." {
            continue;
        }
        let href = if base.is_empty() {
            percent_encode(name)
        } else {
            format!("{}/{}", percent_encode(base), percent_encode(name))
        };
        let (slash, size) = if entry.directory {
            ("/", String::new())
        } else {
            ("", size_text(entry.size))
        };
        let line = format!(
            "<tr><td><a href=\"/{href}{slash}\">{0}{slash}</a><td class=n>{size}<td>{1}<td>{2}<td>{3}<td>{4}\
             <td><button data-p=\"/{href}{slash}\" data-n=\"{0}{slash}\" onclick=rm(this)>Delete</button>",
            escape_html(name),
            stamp_text(entry.created, true),
            stamp_text(entry.modified, true),
            stamp_text(entry.accessed, false),
            attributes_text(entry.attributes)
        );
        write_all(socket, line.as_bytes()).await?;
        count += 1;
    }
    info!("Share: listed {title}, {count} entries");
    write_all(socket, b"</table></div>").await?;
    write_all(socket, SCRIPT.as_bytes()).await
}

/// The listing's controls: uploads with the browser's times, a new folder, a delete per row.
const SCRIPT: &str = "<script>\
const s=document.getElementById('s'),\
t=d=>[d.getFullYear(),d.getMonth()+1,d.getDate(),d.getHours(),d.getMinutes(),d.getSeconds()]\
.map((n,i)=>String(n).padStart(i?2:4,'0')).join(''),\
send=(m,u,body,what)=>new Promise(done=>{const x=new XMLHttpRequest();x.open(m,u);\
x.upload.onprogress=e=>{if(e.lengthComputable)s.textContent=what+' '+Math.floor(e.loaded*100/e.total)+' %'};\
x.onload=()=>{if(x.status<300)done(true);else{s.textContent=what+': '+x.responseText;done(false)}};\
x.onerror=()=>{s.textContent=what+': connection lost';done(false)};x.send(body)}),\
here=location.pathname.replace(/\\/?$/,'/');\
async function up(){const now=t(new Date());\
for(const f of document.getElementById('f').files){\
if(!await send('PUT',here+encodeURIComponent(f.name)+'?created='+now+'&modified='+t(new Date(f.lastModified)),f,f.name))return}\
location.reload()}\
async function md(){const n=prompt('New folder');\
if(n&&await send('MKCOL',here+encodeURIComponent(n)+'?created='+t(new Date()),null,n))location.reload()}\
async function rm(b){if(confirm('Delete '+b.dataset.n+'?')&&await send('DELETE',b.dataset.p,null,b.dataset.n))location.reload()}\
</script>";

/// `2026-09-15 14:03`, date and time each unbroken so a narrow screen wraps between them.
fn stamp_text(stamp: Option<fat::Stamp>, with_time: bool) -> String {
    let Some(s) = stamp else {
        return String::from("\u{2014}");
    };
    let mut text = format!("<span>{:04}-{:02}-{:02}</span>", s.year, s.month, s.day);
    if with_time {
        let _ = write!(text, " <span>{:02}:{:02}</span>", s.hour, s.minute);
    }
    text
}

/// The set flags as letters: R read-only, H hidden, S system, A archive.
fn attributes_text(a: fat::Attributes) -> String {
    [
        (a.read_only(), 'R'),
        (a.hidden(), 'H'),
        (a.system(), 'S'),
        (a.archive(), 'A'),
    ]
    .iter()
    .filter(|(set, _)| *set)
    .map(|(_, letter)| *letter)
    .collect()
}

/// Sends a file, with its length so the client shows progress.
async fn send_file(
    socket: &mut TcpSocket<'_>,
    path: &str,
    entry: fat::Entry,
    chunk: &mut [u8],
    card: &Card<'_>,
) -> Result<(), Gone> {
    let Some(mut file) = entry.open() else {
        return status(socket, "404 Not Found").await;
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        content_type(path),
        entry.size
    );
    write_all(socket, head.as_bytes()).await?;

    let _sending = Sending::start();
    let began = Instant::now();
    let mut sent = 0u32;
    // Where the time goes: the card, or waiting for the socket to take the bytes.
    let (mut reading, mut writing) = (Duration::from_ticks(0), Duration::from_ticks(0));
    while !file.at_end() {
        let started = Instant::now();
        let read = {
            let mut guard = card.lock().await;
            match guard.as_mut() {
                Some(volume) => file.read(volume, chunk),
                None => break,
            }
        };
        let n = match read {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                // The length is already promised; closing early is how the client learns.
                error!("Share: {path} after {sent} bytes: {err:?}");
                return Err(Gone);
            }
        };
        let written = Instant::now();
        reading += written - started;
        write_all(socket, &chunk[..n]).await?;
        writing += written.elapsed();
        sent += n as u32;
    }
    let ms = began.elapsed().as_millis().max(1);
    info!(
        "Share: sent {path}, {sent} bytes in {ms} ms ({} KiB/s), card {} ms, socket {} ms",
        u64::from(sent) * 1000 / 1024 / ms,
        reading.as_millis(),
        writing.as_millis()
    );
    Ok(())
}

/// Counts a file on its way for as long as it lives.
struct Sending;

impl Sending {
    fn start() -> Self {
        SENDING.fetch_add(1, Ordering::Relaxed);
        Sending
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        SENDING.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn write_all(socket: &mut TcpSocket<'_>, mut bytes: &[u8]) -> Result<(), Gone> {
    while !bytes.is_empty() {
        match socket.write(bytes).await {
            Ok(0) | Err(_) => return Err(Gone),
            Ok(n) => bytes = &bytes[n..],
        }
    }
    Ok(())
}

/// The path a request names, `%XX` resolved. `None` if that is not UTF-8 or a code is broken.
fn percent_decode(raw: &[u8]) -> Option<String> {
    let mut bytes = alloc::vec::Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hex = raw.get(i + 1..i + 3)?;
            let hex = core::str::from_utf8(hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// Encodes everything in a path but unreserved characters and `/`.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &b in text.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn size_text(bytes: u32) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..0x10_0000 => format!("{} KiB", bytes / 1024),
        _ => format!("{}.{} MiB", bytes >> 20, ((bytes & 0xF_FFFF) * 10) >> 20),
    }
}

/// A content type for the kinds a browser shows by itself; anything else is downloaded.
fn content_type(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map_or("", |(_, e)| e);
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "txt" | "log" | "csv" => "text/plain; charset=utf-8",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}
