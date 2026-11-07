//! Feedback from the terminal wizard: the same report Snaglist makes in the browser, sent with a
//! QR code a phone can scan, shown as a link, or saved to a file.
//!
//! The report holds the step, Reelix's version, the operating system and processor type, the last
//! error and the person's own words. Never an answer to the wizard, a password, a key or the
//! set-up code. Email addresses, network addresses, user names in paths and anything that looks
//! like a key are replaced before the report is shown.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};
use reelix_api_types::install::{FEEDBACK_REPO, Step};

/// One report.
#[derive(Debug, Clone)]
pub struct Report {
    pub kind: &'static str,
    pub kind_label: &'static str,
    pub step: &'static Step,
    pub words: String,
    pub expected: String,
    pub error: Option<String>,
    pub version: String,
}

impl Report {
    fn system() -> String {
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
    }

    fn title(&self) -> String {
        format!("Set-up wizard, {}: {}", self.step.title, self.kind_label)
    }

    /// The whole report, as Markdown, already cleaned.
    pub fn markdown(&self) -> String {
        let mut md = String::new();
        let _ = writeln!(md, "## {}\n", self.title());
        let _ = writeln!(md, "{}\n", redact(&self.words));
        if !self.expected.trim().is_empty() {
            let _ = writeln!(md, "**What I expected:** {}\n", redact(&self.expected));
        }
        let _ = writeln!(md, "| | |\n|---|---|");
        let _ = writeln!(md, "| Wizard step | `{}` ({}) |", self.step.key, self.step.title);
        let _ = writeln!(md, "| Wizard | terminal |");
        let _ = writeln!(md, "| Reelix | {} |", self.version);
        let _ = writeln!(md, "| System | {} |", Self::system());
        if let Some(e) = &self.error {
            let _ = writeln!(md, "| Last error | {} |", redact(e).replace('|', "/"));
        }
        let _ = writeln!(
            md,
            "\n<sub>Sent from the terminal wizard, which leaves out every answer, password, key and set-up code.</sub>"
        );
        md
    }

    /// A new-issue address with the whole report filled in, for Show the link.
    pub fn link(&self, saved: Option<&Path>) -> String {
        let mut body = self.markdown();
        if let Some(p) = saved {
            let _ = write!(
                body,
                "\nThe whole report is also saved on the server: `{}`",
                redact(&p.display().to_string())
            );
        }
        format!(
            "https://github.com/{FEEDBACK_REPO}/issues/new?title={}&labels=feedback&body={}",
            encode(&self.title()),
            encode(&body)
        )
    }

    /// A short new-issue address for a QR code, no longer than `max` bytes: the kind, the step,
    /// the person's words cut to fit, the version and system on one line, and where the whole
    /// report is saved. Returns the address and whether the words were cut; `None` when even the
    /// shortest address does not fit.
    pub fn short_link(&self, max: usize, saved: Option<&Path>) -> Option<(String, bool)> {
        let words = redact(&self.words);
        let make = |w: &str, cut: bool| {
            let mut body = format!(
                "{w}{}\n\nReelix {} {}/{}",
                if cut { "..." } else { "" },
                self.version,
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            if let Some(p) = saved {
                let _ = write!(body, "\nSaved {}", redact(&p.display().to_string()));
            }
            let title = format!("{} ({})", self.kind_label, self.step.key);
            format!(
                "https://github.com/{FEEDBACK_REPO}/issues/new?title={}&body={}",
                encode_short(&title),
                encode_short(&body)
            )
        };
        let full = make(words.trim(), false);
        if full.len() <= max {
            return Some((full, false));
        }
        // Cut the words, a few characters at a time, until the address fits.
        let chars: Vec<char> = words.trim().chars().collect();
        let mut n = chars.len();
        while n > 0 {
            n = n.saturating_sub((n / 10).max(1));
            let cut: String = chars[..n].iter().collect();
            let url = make(cut.trim_end(), true);
            if url.len() <= max {
                return Some((url, true));
            }
        }
        let url = make("", true);
        (url.len() <= max).then_some((url, true))
    }

    /// Saves the whole report to `dir`, and returns where.
    pub fn save(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        // Short, as the QR code's address names it.
        let name = format!(
            "{}-{}.md",
            self.step.key,
            chrono::Utc::now().format("%m%d-%H%M%S")
        );
        let path = dir.join(name);
        std::fs::write(&path, self.markdown())?;
        Ok(path)
    }
}

/// Percent-encodes a query value.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Encodes a query value as briefly as a QR code needs: `+` for spaces, and the characters a
/// query may hold as they are.
pub fn encode_short(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        if b == b' ' {
            out.push('+');
        } else if b.is_ascii_alphanumeric() || b"-_.~/:,()".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Replaces email addresses, network addresses, user names in home folders and anything that
/// looks like a key or token.
pub fn redact(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|chunk| {
            let word = chunk.trim_end();
            let space = &chunk[word.len()..];
            let core = word.trim_matches(|c: char| ",.;:()[]{}\"'".contains(c));
            let replaced = if core.contains('@') && core.contains('.') {
                Some("[email]".to_owned())
            } else if looks_like_address(core) {
                Some("[address]".to_owned())
            } else if let Some(rest) = core
                .strip_prefix("/home/")
                .or_else(|| core.strip_prefix("/Users/"))
            {
                let tail = rest.split_once('/').map(|(_, t)| t).unwrap_or("");
                Some(format!("/home/[user]/{tail}"))
            } else if looks_like_key(core) {
                Some("[key]".to_owned())
            } else {
                None
            };
            match replaced {
                Some(r) if !core.is_empty() => format!("{}{space}", word.replacen(core, &r, 1)),
                _ => chunk.to_owned(),
            }
        })
        .collect()
}

fn looks_like_address(s: &str) -> bool {
    let host = s
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    let parts: Vec<&str> = host.split('.').collect();
    (parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.parse::<u8>().is_ok()))
        || (s.matches(':').count() >= 3 && s.chars().all(|c| c.is_ascii_hexdigit() || c == ':'))
}

fn looks_like_key(s: &str) -> bool {
    s.len() >= 20
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_=+/".contains(c))
}

/// The largest QR code version that fits a window of `cols` by `rows`, keeping one title line and
/// the two menu lines. Half blocks draw two rows of modules on each line, with a quiet zone of 2.
pub fn largest_version(cols: usize, rows: usize) -> Option<u8> {
    (1..=40u8).rev().find(|v| {
        let modules = 17 + 4 * *v as usize + 4;
        modules <= cols && modules.div_ceil(2) + 3 <= rows
    })
}

/// The QR code for `text`, no larger than `max_version`, at error correction level L.
pub fn qr(text: &str, max_version: u8) -> Option<QrCode> {
    let segs = QrSegment::make_segments(text);
    QrCode::encode_segments_advanced(
        &segs,
        QrCodeEcc::Low,
        Version::new(1),
        Version::new(max_version.clamp(1, 40)),
        None,
        false,
    )
    .ok()
}

/// The bytes a version holds at level L in byte mode.
pub fn capacity(version: u8) -> usize {
    // The address is encoded as bytes: find the longest that fits by trying, as the table is long.
    let mut lo = 0usize;
    let mut hi = 3000usize;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if qr(&"a".repeat(mid), version).is_some_and(|c| c.version().value() <= version) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// The code as lines of half blocks, dark on light in forced colours so it scans on a dark
/// terminal too, with a quiet zone of 2 modules.
pub fn draw(code: &QrCode) -> Vec<String> {
    let quiet = 2i32;
    let size = code.size();
    let dark = |x: i32, y: i32| x >= 0 && y >= 0 && x < size && y < size && code.get_module(x, y);
    let mut lines = Vec::new();
    let mut y = -quiet;
    while y < size + quiet {
        let mut line = String::from("\x1b[30;107m");
        for x in -quiet..size + quiet {
            line.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        line.push_str("\x1b[0m");
        lines.push(line);
        y += 2;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelix_api_types::install::{StepId, step};

    fn report(words: &str) -> Report {
        Report {
            kind: "ux",
            kind_label: "Hard to use",
            step: step(StepId::Tmdb),
            words: words.into(),
            expected: String::new(),
            error: Some("connection refused by 192.168.1.20:7878".into()),
            version: "0.8.3".into(),
        }
    }

    #[test]
    fn a_78_by_30_window_fits_version_8_and_about_190_bytes() {
        assert_eq!(largest_version(78, 30), Some(8));
        assert_eq!(capacity(8), 192);
        assert_eq!(largest_version(20, 10), None);
    }

    #[test]
    fn the_link_is_cut_to_fit_the_code_and_says_so() {
        let r = report(&"The token field did not say where to find the token. ".repeat(10));
        let saved = Path::new("/config/feedback/tmdb-1105-121530.md");
        let (url, cut) = r.short_link(capacity(8), Some(saved)).unwrap();
        assert!(cut && url.len() <= 192);
        assert!(qr(&url, 8).is_some());
        let lines = draw(&qr(&url, 8).unwrap());
        assert_eq!(lines.len(), (49 + 4usize).div_ceil(2));
        assert!(url.contains("Saved+/config/feedback/tmdb-1105-121530.md"));
        assert!(url.contains("title=Hard+to+use+(tmdb)"));
        let full = r.link(Some(saved));
        assert!(full.len() > url.len() && full.contains("labels=feedback"));
    }

    #[test]
    fn private_details_are_replaced() {
        let out = redact(
            "mail me at someone@example.com, server 192.168.1.20:7878 in /home/alex/media key abcd1234efgh5678ijkl9012",
        );
        assert!(!out.contains("someone@"));
        assert!(!out.contains("192.168"));
        assert!(!out.contains("alex"));
        assert!(!out.contains("abcd1234"));
        assert!(out.contains("[email]") && out.contains("[address]") && out.contains("[key]"));
        let md = report("fine").markdown();
        assert!(md.contains("`tmdb`") && !md.contains("192.168"));
    }

    #[test]
    fn encoding_keeps_only_safe_characters() {
        assert_eq!(encode("a b&c/é"), "a%20b%26c%2F%C3%A9");
    }
}
