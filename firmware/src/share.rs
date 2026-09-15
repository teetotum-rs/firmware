//! The card over Wi-Fi, read-only, while its dialog is open.
//!
//! The dialog raises [`OPEN`]; the Wi-Fi task then runs an access point beside its station and
//! leaves out its scans while [`busy`] says a file is on its way, because a scan switches
//! channels and stalls a download for seconds. [`serve`] runs all the time: without the access
//! point its sockets simply hear nothing.
//!
//! A client gets an address over DHCP and **no gateway**, so a phone keeps its own route to the
//! internet. `GET` on a directory answers a listing, on a file the file.

use alloc::format;
use alloc::string::String;

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
use log::{error, info, warn};
use teetotum::fat::{self, Volume};

/// Whether the dialog is open, and with it the access point. Set by the device loop.
pub static OPEN: AtomicBool = AtomicBool::new(false);

/// Files being sent right now.
static SENDING: AtomicU8 = AtomicU8::new(0);

/// Whether a file is on its way to a client.
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
/// TCP receive and transmit buffer, each.
const TCP_BUFFER: usize = 8192;
/// What a request head may take; the rest of it is not read.
const HEAD: usize = 512;
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
pub const BUFFER_BYTES: usize = HTTP_SOCKETS * (2 * TCP_BUFFER + HEAD + CHUNK) + 4 * DHCP_BUFFER;

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
    let (rx, rest) = area.split_at_mut(TCP_BUFFER);
    let (tx, rest) = rest.split_at_mut(TCP_BUFFER);
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
    let Some(target) = head
        .strip_prefix(b"GET ")
        .and_then(|rest| rest.split(|&b| b == b' ').next())
    else {
        return status(socket, "405 Method Not Allowed").await;
    };
    let target = target.split(|&b| b == b'?').next().unwrap_or(target);
    let Some(path) = percent_decode(target) else {
        return status(socket, "400 Bad Request").await;
    };

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
    let response = format!(
        "HTTP/1.1 {line}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{line}\n",
        line.len() + 1
    );
    write_all(socket, response.as_bytes()).await
}

/// Lists a directory as a page of links. Its length is not known ahead, so the page ends where
/// the connection closes.
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
         <h1>{0}</h1><div><table>\
         <tr><th>Name<th class=n>Size<th>Created<th>Modified<th>Accessed<th>Attributes",
        escape_html(&title)
    );
    if let Some(parent) = (!base.is_empty()).then(|| base.rsplit_once('/').map_or("", |(p, _)| p)) {
        let _ = write!(page, "<tr><td><a href=\"/{}\">..</a>", percent_encode(parent));
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
            "<tr><td><a href=\"/{href}{slash}\">{}{slash}</a><td class=n>{size}<td>{}<td>{}<td>{}<td>{}",
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
    write_all(socket, b"</table></div>").await
}

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
