use std::io::{self, StdoutLock, Write};

/// A writer that writes to stdout and tracks lines written, stopping at a configurable limit.
/// When the limit is reached, it writes a truncation notice to stderr.
pub struct BoundedWriter<'a> {
    inner: StdoutLock<'a>,
    limit: Option<usize>,
    lines_written: usize,
    truncated: bool,
}

impl<'a> BoundedWriter<'a> {
    pub fn new(inner: StdoutLock<'a>, limit: Option<usize>) -> Self {
        Self {
            inner,
            limit,
            lines_written: 0,
            truncated: false,
        }
    }

    /// Write a single line (including newline). Returns Ok(true) if the line
    /// was written, Ok(false) if the limit has been reached.
    pub fn write_line(&mut self, line: &str) -> io::Result<bool> {
        if let Some(limit) = self.limit
            && self.lines_written >= limit
        {
            if !self.truncated {
                self.truncated = true;
                eprintln!(
                    "sak: output truncated at {} results (use --limit to adjust)",
                    limit
                );
            }
            return Ok(false);
        }
        self.inner.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            self.inner.write_all(b"\n")?;
        }
        self.lines_written += 1;
        Ok(true)
    }

    /// Write a line without counting it toward the limit (for separators, headings, etc.)
    pub fn write_decoration(&mut self, line: &str) -> io::Result<()> {
        if self.truncated {
            return Ok(());
        }
        self.inner.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            self.inner.write_all(b"\n")?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Format a line number right-aligned to the given width, followed by a tab.
pub fn format_line_number(n: usize, width: usize) -> String {
    format!("{:>width$}\t", n, width = width)
}

/// Compute the display width needed for line numbers up to `max`.
pub fn line_number_width(max: usize) -> usize {
    if max == 0 {
        1
    } else {
        ((max as f64).log10().floor() as usize) + 1
    }
}

/// Convert a path to a display string relative to a base path.
pub fn relative_path(path: &std::path::Path, base: &std::path::Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Check if a file appears to be binary by looking for NUL bytes in the first 8KB.
pub fn is_binary(path: &std::path::Path) -> io::Result<bool> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf)?;
    Ok(buf[..n].contains(&0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_line_number() {
        assert_eq!(format_line_number(1, 4), "   1\t");
        assert_eq!(format_line_number(42, 4), "  42\t");
        assert_eq!(format_line_number(1000, 4), "1000\t");
    }

    #[test]
    fn test_line_number_width() {
        assert_eq!(line_number_width(1), 1);
        assert_eq!(line_number_width(9), 1);
        assert_eq!(line_number_width(10), 2);
        assert_eq!(line_number_width(99), 2);
        assert_eq!(line_number_width(100), 3);
        assert_eq!(line_number_width(999), 3);
        assert_eq!(line_number_width(1000), 4);
    }

    #[test]
    fn test_relative_path() {
        use std::path::Path;
        let base = Path::new("/home/user/project");
        let full = Path::new("/home/user/project/src/main.rs");
        assert_eq!(relative_path(full, base), "src/main.rs");

        let other = Path::new("/tmp/file.txt");
        assert_eq!(relative_path(other, base), "/tmp/file.txt");
    }
}
