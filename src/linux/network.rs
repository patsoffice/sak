//! `sak linux network` — decode the socket tables in `/proc/net/{tcp,tcp6,udp,udp6}`.
//!
//! These files are notoriously awkward to read by eye: addresses are hex in the
//! kernel's *host* byte order (so on a little-endian box the bytes come out
//! reversed unless swapped), ports are hex, and the connection state is a hex
//! code (`0A` = LISTEN, `01` = ESTABLISHED, ...). This command decodes all of
//! that into `proto<TAB>state<TAB>local<TAB>remote<TAB>uid<TAB>inode`, with IPv4
//! as dotted-quad and IPv6 as bracketed colon-hex (`[::1]:80`).
//!
//! The byte-order swap is the subtle part and the reason the decoders
//! ([`decode_v4`], [`decode_v6`]) are pure functions unit-tested on known-good
//! v4 and v6 entries: `0100007F` must decode to `127.0.0.1`, and each 32-bit
//! word of a v6 address is individually byte-reversed.

use crate::output::Outcome;
use std::io;
use std::net::Ipv6Addr;

use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

use super::read_proc_file;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Decode /proc/net/{tcp,tcp6,udp,udp6} socket tables",
    long_about = "Decode the socket tables in /proc/net/tcp, tcp6, udp, and udp6.\n\n\
        Default output is TSV:\n\n  \
        proto<TAB>state<TAB>local<TAB>remote<TAB>uid<TAB>inode\n\n\
        Addresses and ports are decoded from the kernel's hex/host-byte-order \
        form: IPv4 as dotted-quad, IPv6 as bracketed colon-hex (`[::1]:80`). \
        The connection state hex code is mapped to its name (`0A` -> LISTEN, \
        `01` -> ESTABLISHED, `06` -> TIME_WAIT, ...); an unknown code passes \
        through as the raw hex.\n\n\
        `--proto` selects one family or `all` (default). `--state` keeps only \
        rows in a given state (case-insensitive). `--format json` emits NDJSON. \
        Output is sorted by (proto, local, remote) for deterministic diffs. \
        A missing family file (e.g. tcp6 on a host without IPv6) is skipped.",
    after_help = "\
Examples:
  sak linux network                      All sockets, all families
  sak linux network --state LISTEN       Only listening sockets
  sak linux network --proto tcp          Only IPv4 TCP
  sak linux network --format json        NDJSON for further processing"
)]
pub struct NetworkArgs {
    /// Protocol family to read
    #[arg(long, value_enum, default_value_t = Proto::All)]
    pub proto: Proto,

    /// Keep only sockets in this state (e.g. LISTEN, ESTABLISHED)
    #[arg(long)]
    pub state: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Proto {
    Tcp,
    Tcp6,
    Udp,
    Udp6,
    All,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated columns
    Tsv,
    /// Newline-delimited JSON, one socket per line
    Json,
}

/// One decoded socket-table row.
#[derive(Debug, PartialEq)]
struct Conn {
    proto: String,
    state: String,
    local: String,
    remote: String,
    uid: String,
    inode: String,
}

pub fn run(args: &NetworkArgs) -> Result<Outcome> {
    let families: &[&str] = match args.proto {
        Proto::Tcp => &["tcp"],
        Proto::Tcp6 => &["tcp6"],
        Proto::Udp => &["udp"],
        Proto::Udp6 => &["udp6"],
        Proto::All => &["tcp", "tcp6", "udp", "udp6"],
    };

    let mut conns: Vec<Conn> = Vec::new();
    for proto in families {
        // A missing family file (no IPv6, say) is fine — skip it.
        let Ok(raw) = read_proc_file(&format!("/proc/net/{proto}")) else {
            continue;
        };
        for line in raw.lines() {
            if let Some(conn) = parse_line(proto, line) {
                if let Some(state) = &args.state
                    && !conn.state.eq_ignore_ascii_case(state)
                {
                    continue;
                }
                conns.push(conn);
            }
        }
    }

    conns.sort_by(|a, b| (&a.proto, &a.local, &a.remote).cmp(&(&b.proto, &b.local, &b.remote)));

    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    for c in &conns {
        let line = match args.format {
            Format::Tsv => format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                c.proto, c.state, c.local, c.remote, c.uid, c.inode
            ),
            Format::Json => serde_json::to_string(&build_json(c))?,
        };
        if !writer.write_line(&line)? {
            break;
        }
        wrote_any = true;
    }

    writer.flush()?;
    if wrote_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

/// Parse one socket-table line. Returns `None` for the header row and any line
/// that doesn't have the expected shape.
fn parse_line(proto: &str, line: &str) -> Option<Conn> {
    let t: Vec<&str> = line.split_whitespace().collect();
    // sl local rem st tx:rx tr:tm retrnsmt uid timeout inode ...
    if t.len() < 10 {
        return None;
    }
    // Header row has "local_address" here, which lacks the ':' separator.
    if !t[1].contains(':') {
        return None;
    }
    let v6 = proto.ends_with('6');
    let local = decode_endpoint(t[1], v6)?;
    let remote = decode_endpoint(t[2], v6)?;
    let state = state_name(t[3]);
    Some(Conn {
        proto: proto.to_string(),
        state,
        local,
        remote,
        uid: t[7].to_string(),
        inode: t[9].to_string(),
    })
}

/// Decode an `ADDR:PORT` endpoint (both hex) into a printable string.
fn decode_endpoint(field: &str, v6: bool) -> Option<String> {
    let (addr_hex, port_hex) = field.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if v6 {
        let ip = decode_v6(addr_hex)?;
        Some(format!("[{ip}]:{port}"))
    } else {
        let ip = decode_v4(addr_hex)?;
        Some(format!("{ip}:{port}"))
    }
}

/// Decode an 8-hex-char IPv4 address. The kernel writes the 4 bytes in host
/// (little-endian) byte order, so they are reversed to form the dotted quad:
/// `0100007F` -> `127.0.0.1`.
fn decode_v4(hex: &str) -> Option<String> {
    if hex.len() != 8 {
        return None;
    }
    let mut b = [0u8; 4];
    for (i, byte) in b.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(format!("{}.{}.{}.{}", b[3], b[2], b[1], b[0]))
}

/// Decode a 32-hex-char IPv6 address. The 16 bytes are written as four 32-bit
/// words, each in host (little-endian) byte order, so each word's 4 bytes are
/// reversed before reassembling the network-order byte array.
fn decode_v6(hex: &str) -> Option<String> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for word in 0..4 {
        let base = word * 8;
        let mut wb = [0u8; 4];
        for (i, byte) in wb.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[base + i * 2..base + i * 2 + 2], 16).ok()?;
        }
        // Reverse this word's bytes (host -> network order).
        bytes[word * 4] = wb[3];
        bytes[word * 4 + 1] = wb[2];
        bytes[word * 4 + 2] = wb[1];
        bytes[word * 4 + 3] = wb[0];
    }
    Some(Ipv6Addr::from(bytes).to_string())
}

/// Map a TCP/UDP state hex code to its name; unknown codes pass through as the
/// raw hex. (UDP uses the same `TCP_*` state enum in the kernel.)
fn state_name(code: &str) -> String {
    match code {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        "0C" => "NEW_SYN_RECV",
        other => other,
    }
    .to_string()
}

fn build_json(c: &Conn) -> Value {
    json!({
        "proto": c.proto,
        "state": c.state,
        "local": c.local,
        "remote": c.remote,
        "uid": super::json_num(&c.uid),
        "inode": super::json_num(&c.inode),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A known-good IPv4 TCP table: a LISTEN on 127.0.0.1:53 and an ESTABLISHED
    // 127.0.0.1:5432 -> 127.0.0.1:35554.
    const TCP4: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0035 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0
   1: 0100007F:1538 0100007F:8AE2 01 00000000:00000000 00:00000000 00000000  1000        0 67890 1 0000000000000000 20 0 0 10 0
";

    // A known-good IPv6 TCP table: LISTEN on [::]:5432 and ESTABLISHED on [::1]:80.
    const TCP6: &str = "\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:1538 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 11111 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000001000000:0050 00000000000000000000000001000000:CF24 01 00000000:00000000 00:00000000 00000000  1000        0 22222 1 0000000000000000 20 0 0 10 0
";

    #[test]
    fn decode_v4_reverses_byte_order() {
        assert_eq!(decode_v4("0100007F").as_deref(), Some("127.0.0.1"));
        assert_eq!(decode_v4("00000000").as_deref(), Some("0.0.0.0"));
        assert_eq!(decode_v4("0101A8C0").as_deref(), Some("192.168.1.1"));
    }

    #[test]
    fn decode_v4_rejects_wrong_length() {
        assert_eq!(decode_v4("0100"), None);
    }

    #[test]
    fn decode_v6_swaps_each_word() {
        assert_eq!(
            decode_v6("00000000000000000000000001000000").as_deref(),
            Some("::1")
        );
        assert_eq!(
            decode_v6("00000000000000000000000000000000").as_deref(),
            Some("::")
        );
    }

    #[test]
    fn state_codes_map_to_names() {
        assert_eq!(state_name("0A"), "LISTEN");
        assert_eq!(state_name("01"), "ESTABLISHED");
        assert_eq!(state_name("06"), "TIME_WAIT");
        // Unknown code passes through.
        assert_eq!(state_name("FF"), "FF");
    }

    #[test]
    fn parses_v4_line() {
        let lines: Vec<&str> = TCP4.lines().collect();
        let listen = parse_line("tcp", lines[1]).unwrap();
        assert_eq!(
            listen,
            Conn {
                proto: "tcp".into(),
                state: "LISTEN".into(),
                local: "127.0.0.1:53".into(),
                remote: "0.0.0.0:0".into(),
                uid: "0".into(),
                inode: "12345".into(),
            }
        );
        let est = parse_line("tcp", lines[2]).unwrap();
        assert_eq!(est.state, "ESTABLISHED");
        assert_eq!(est.local, "127.0.0.1:5432");
        assert_eq!(est.remote, "127.0.0.1:35554");
        assert_eq!(est.uid, "1000");
        assert_eq!(est.inode, "67890");
    }

    #[test]
    fn parses_v6_line() {
        let lines: Vec<&str> = TCP6.lines().collect();
        let est = parse_line("tcp6", lines[2]).unwrap();
        assert_eq!(est.local, "[::1]:80");
        assert_eq!(est.remote, "[::1]:53028");
        assert_eq!(est.state, "ESTABLISHED");
    }

    #[test]
    fn header_line_is_skipped() {
        let lines: Vec<&str> = TCP4.lines().collect();
        assert_eq!(parse_line("tcp", lines[0]), None);
    }
}
