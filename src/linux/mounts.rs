//! `sak linux mounts` — parse the mount table from `/proc/self/mountinfo`.
//!
//! `/proc/self/mountinfo` is richer than `/proc/mounts`: it carries mount and
//! parent IDs, the device `major:minor`, the subtree `root` (which is how a
//! bind mount is distinguished — its root is a subdirectory rather than `/`),
//! and a *variable-length* run of optional propagation fields (`shared:1`,
//! `master:1`, ...) terminated by a literal `-` separator before the filesystem
//! type, source, and super-block options. The parser splits on that `-` first,
//! so the optional fields never throw off the fixed columns on either side.
//!
//! Default output is the full mountinfo column set; `--mounts` falls back to the
//! simpler `/proc/mounts` shape for callers that only want device / mount point
//! / fs type / options. Both parsers are pure functions over `&str`, unit-tested
//! on fixtures that include a bind mount and an overlay mount (the overlay line
//! has zero optional fields, exercising the empty-optional case).

use crate::output::Outcome;
use std::io;

use anyhow::Result;
use clap::Args;
use serde_json::{Value, json};

use super::read_proc_file;
use crate::output::BoundedWriter;

#[derive(Args)]
#[command(
    about = "Parse the mount table from /proc/self/mountinfo",
    long_about = "Parse the mount table from /proc/self/mountinfo, which is \
        richer than /proc/mounts.\n\n\
        Default output is TSV with the full mountinfo column set:\n\n  \
        mount_id<TAB>parent<TAB>device<TAB>root<TAB>mount_point<TAB>options<TAB>fs_type<TAB>source\n\n\
        `device` is the `major:minor` pair; `root` is the mounted subtree \
        within the source filesystem (a `root` other than `/` is the tell-tale \
        of a bind mount); `source` is the device or pseudo-source after the \
        filesystem type. Path-like fields keep the kernel's octal escaping \
        (`\\040` for space, etc.) so a space in a mount point never breaks the \
        TSV columns.\n\n\
        `--type <fs>` keeps only rows of a given filesystem type. `--mounts` \
        falls back to the simpler /proc/mounts shape \
        (device<TAB>mount_point<TAB>fs_type<TAB>options). `--format json` emits \
        one JSON object per mount (NDJSON).",
    after_help = "\
Examples:
  sak linux mounts                   Full mountinfo table
  sak linux mounts --type ext4       Only ext4 mounts
  sak linux mounts --mounts          Simpler /proc/mounts shape
  sak linux mounts --format json     NDJSON for further processing"
)]
pub struct MountsArgs {
    /// Keep only mounts of this filesystem type (e.g. ext4, tmpfs, overlay)
    #[arg(long = "type")]
    pub fs_type: Option<String>,

    /// Use the simpler /proc/mounts shape instead of /proc/self/mountinfo
    #[arg(long)]
    pub mounts: bool,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Tsv)]
    pub format: Format,

    /// Maximum number of output lines
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Tab-separated columns
    Tsv,
    /// Newline-delimited JSON, one mount per line
    Json,
}

/// A row from `/proc/self/mountinfo`.
#[derive(Debug, PartialEq)]
struct MountInfo {
    mount_id: String,
    parent: String,
    device: String,
    root: String,
    mount_point: String,
    options: String,
    fs_type: String,
    source: String,
}

/// A row from the simpler `/proc/mounts`.
#[derive(Debug, PartialEq)]
struct MountEntry {
    device: String,
    mount_point: String,
    fs_type: String,
    options: String,
}

pub fn run(args: &MountsArgs) -> Result<Outcome> {
    let stdout = io::stdout();
    let handle = stdout.lock();
    let mut writer = BoundedWriter::new(handle, args.limit);

    let mut wrote_any = false;
    if args.mounts {
        let raw = read_proc_file("/proc/mounts")?;
        for m in parse_mounts(&raw) {
            if let Some(t) = &args.fs_type
                && &m.fs_type != t
            {
                continue;
            }
            let line = match args.format {
                Format::Tsv => format!(
                    "{}\t{}\t{}\t{}",
                    m.device, m.mount_point, m.fs_type, m.options
                ),
                Format::Json => serde_json::to_string(&mount_entry_json(&m))?,
            };
            if !writer.write_line(&line)? {
                break;
            }
            wrote_any = true;
        }
    } else {
        let raw = read_proc_file("/proc/self/mountinfo")?;
        for m in parse_mountinfo(&raw) {
            if let Some(t) = &args.fs_type
                && &m.fs_type != t
            {
                continue;
            }
            let line = match args.format {
                Format::Tsv => format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    m.mount_id,
                    m.parent,
                    m.device,
                    m.root,
                    m.mount_point,
                    m.options,
                    m.fs_type,
                    m.source,
                ),
                Format::Json => serde_json::to_string(&mount_info_json(&m))?,
            };
            if !writer.write_line(&line)? {
                break;
            }
            wrote_any = true;
        }
    }

    writer.flush()?;
    if wrote_any {
        Ok(Outcome::Found)
    } else {
        Ok(Outcome::NotFound)
    }
}

/// Parse `/proc/self/mountinfo`.
///
/// Each line is `mount_id parent major:minor root mount_point options
/// [optional_fields...] - fs_type source super_options`. The optional fields
/// vary in count (zero or more), so the line is split on the lone `-` separator
/// first; the six fixed fields sit before it and the filesystem type / source
/// sit immediately after. Lines that don't have a separator or enough fields are
/// skipped rather than erroring.
fn parse_mountinfo(input: &str) -> Vec<MountInfo> {
    let mut out = Vec::new();
    for line in input.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(sep) = tokens.iter().position(|t| *t == "-") else {
            continue;
        };
        let pre = &tokens[..sep];
        let post = &tokens[sep + 1..];
        if pre.len() < 6 || post.len() < 2 {
            continue;
        }
        out.push(MountInfo {
            mount_id: pre[0].to_string(),
            parent: pre[1].to_string(),
            device: pre[2].to_string(),
            root: pre[3].to_string(),
            mount_point: pre[4].to_string(),
            options: pre[5].to_string(),
            // pre[6..] are the optional propagation fields — intentionally dropped.
            fs_type: post[0].to_string(),
            source: post[1].to_string(),
        });
    }
    out
}

/// Parse the simpler `/proc/mounts`: `device mount_point fs_type options dump pass`.
fn parse_mounts(input: &str) -> Vec<MountEntry> {
    let mut out = Vec::new();
    for line in input.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            continue;
        }
        out.push(MountEntry {
            device: tokens[0].to_string(),
            mount_point: tokens[1].to_string(),
            fs_type: tokens[2].to_string(),
            options: tokens[3].to_string(),
        });
    }
    out
}

fn mount_info_json(m: &MountInfo) -> Value {
    json!({
        "mount_id": m.mount_id,
        "parent": m.parent,
        "device": m.device,
        "root": m.root,
        "mount_point": m.mount_point,
        "options": m.options,
        "fs_type": m.fs_type,
        "source": m.source,
    })
}

fn mount_entry_json(m: &MountEntry) -> Value {
    json!({
        "device": m.device,
        "mount_point": m.mount_point,
        "fs_type": m.fs_type,
        "options": m.options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Includes a bind mount (mount_id 100, root /srv/data) and an overlay mount
    // (mount_id 200) that has *zero* optional fields — the `-` separator comes
    // straight after the mount options.
    const MOUNTINFO: &str = "\
21 28 0:20 / /sys rw,nosuid,nodev,noexec,relatime shared:7 - sysfs sysfs rw
24 28 0:5 / /dev rw,nosuid shared:2 - devtmpfs devtmpfs rw,size=8192k
26 24 0:23 / /dev/shm rw,nosuid,nodev shared:4 - tmpfs tmpfs rw
30 28 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw,errors=remount-ro
100 30 8:1 /srv/data /mnt/bind rw,relatime shared:1 - ext4 /dev/sda1 rw,errors=remount-ro
200 30 0:50 / /var/lib/docker/overlay2/abc/merged rw,relatime - overlay overlay rw,lowerdir=/a,upperdir=/b
";

    const MOUNTS: &str = "\
/dev/sda1 / ext4 rw,relatime 0 0
tmpfs /dev/shm tmpfs rw,nosuid,nodev 0 0
overlay /var/lib/docker/overlay2/abc/merged overlay rw,relatime,lowerdir=/a 0 0
";

    #[test]
    fn parses_root_mount_fixed_fields() {
        let mounts = parse_mountinfo(MOUNTINFO);
        assert_eq!(mounts.len(), 6);
        let root = mounts.iter().find(|m| m.mount_id == "30").unwrap();
        assert_eq!(root.parent, "28");
        assert_eq!(root.device, "8:1");
        assert_eq!(root.root, "/");
        assert_eq!(root.mount_point, "/");
        assert_eq!(root.options, "rw,relatime");
        assert_eq!(root.fs_type, "ext4");
        assert_eq!(root.source, "/dev/sda1");
    }

    #[test]
    fn bind_mount_has_non_root_subtree() {
        let mounts = parse_mountinfo(MOUNTINFO);
        let bind = mounts.iter().find(|m| m.mount_id == "100").unwrap();
        assert_eq!(bind.root, "/srv/data");
        assert_eq!(bind.mount_point, "/mnt/bind");
        assert_eq!(bind.fs_type, "ext4");
    }

    #[test]
    fn overlay_mount_with_zero_optional_fields() {
        let mounts = parse_mountinfo(MOUNTINFO);
        let overlay = mounts.iter().find(|m| m.mount_id == "200").unwrap();
        // No optional fields: the `-` comes right after the mount options.
        assert_eq!(overlay.options, "rw,relatime");
        assert_eq!(overlay.fs_type, "overlay");
        assert_eq!(overlay.source, "overlay");
    }

    #[test]
    fn skips_lines_without_separator() {
        let mounts = parse_mountinfo("30 28 8:1 / / rw,relatime ext4 /dev/sda1 rw\n");
        assert!(mounts.is_empty());
    }

    #[test]
    fn parses_proc_mounts_fallback() {
        let entries = parse_mounts(MOUNTS);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            MountEntry {
                device: "/dev/sda1".to_string(),
                mount_point: "/".to_string(),
                fs_type: "ext4".to_string(),
                options: "rw,relatime".to_string(),
            }
        );
    }

    #[test]
    fn mountinfo_json_has_full_column_set() {
        let mounts = parse_mountinfo(MOUNTINFO);
        let root = mounts.iter().find(|m| m.mount_id == "30").unwrap();
        let v = mount_info_json(root);
        assert_eq!(v["device"], "8:1");
        assert_eq!(v["fs_type"], "ext4");
        assert_eq!(v["source"], "/dev/sda1");
    }
}
