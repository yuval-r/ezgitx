use serde::Serialize;

pub const DEFAULT_MAX_BYTES: usize = 2048;

/// Keep the last `max` bytes of `bytes` (PRD §3.4: output tails). Returns the
/// lossily-decoded string and whether capping occurred.
pub fn cap_tail(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() <= max {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    } else {
        let slice = &bytes[bytes.len() - max..];
        (String::from_utf8_lossy(slice).into_owned(), true)
    }
}

/// Streams JSONL to stdout, or buffers rows for an aligned table in `--human`
/// mode. JSONL lines are emitted the moment a repo completes; human tables
/// need all rows for column widths, so they print on `finish`.
pub struct Emitter {
    human: bool,
    headers: Vec<&'static str>,
    rows: Vec<Vec<String>>,
    footers: Vec<String>,
}

impl Emitter {
    pub fn new(human: bool, headers: &[&'static str]) -> Self {
        Self {
            human,
            headers: headers.to_vec(),
            rows: Vec::new(),
            footers: Vec::new(),
        }
    }

    /// Emit one result: `value` is the JSONL line, `row` its human rendering.
    pub fn emit<T: Serialize>(&mut self, value: &T, row: Vec<String>) {
        if self.human {
            self.rows.push(row);
        } else {
            println!("{}", serde_json::to_string(value).unwrap());
        }
    }

    /// Emit a trailing summary line: JSONL object, or a plain text footer.
    pub fn emit_summary<T: Serialize>(&mut self, value: &T, human_text: String) {
        if self.human {
            self.footers.push(human_text);
        } else {
            println!("{}", serde_json::to_string(value).unwrap());
        }
    }

    pub fn finish(self) {
        if !self.human {
            return;
        }
        if !self.rows.is_empty() {
            let cols = self.headers.len();
            let mut widths: Vec<usize> = self.headers.iter().map(|h| h.len()).collect();
            for row in &self.rows {
                for (i, cell) in row.iter().enumerate().take(cols) {
                    widths[i] = widths[i].max(cell.len());
                }
            }
            let render = |cells: Vec<&str>| {
                cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
                    .collect::<Vec<_>>()
                    .join("  ")
                    .trim_end()
                    .to_string()
            };
            println!("{}", render(self.headers.clone()));
            for row in &self.rows {
                println!("{}", render(row.iter().map(String::as_str).collect()));
            }
        }
        for footer in &self.footers {
            println!("{footer}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_tail_under_limit_passes_through() {
        let (s, truncated) = cap_tail(b"hello", 10);
        assert_eq!(s, "hello");
        assert!(!truncated);
    }

    #[test]
    fn cap_tail_keeps_last_bytes() {
        let (s, truncated) = cap_tail(b"0123456789", 4);
        assert_eq!(s, "6789");
        assert!(truncated);
    }

    #[test]
    fn cap_tail_handles_split_utf8() {
        // "héllo" — cutting mid-é must not panic; lossy replacement is fine.
        let bytes = "h\u{e9}llo".as_bytes();
        let (s, truncated) = cap_tail(bytes, 4);
        assert!(truncated);
        assert!(s.ends_with("llo"));
    }

    #[test]
    fn cap_tail_exact_boundary() {
        let (s, truncated) = cap_tail(b"abcd", 4);
        assert_eq!(s, "abcd");
        assert!(!truncated);
    }
}
