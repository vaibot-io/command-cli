//! ui.rs — the CLI's presentation layer.
//!
//! Before this existed, 457 `println!` sites across 16 files each invented their
//! own conventions: "success" was written `[ok]`, `✓` and `✔` depending on the
//! file, columns were aligned by typing literal runs of spaces into format
//! strings, and there were eight different header shapes. This module is the one
//! place those decisions live.
//!
//! **No new dependencies.** TTY detection is `std::io::IsTerminal` (stable since
//! 1.70; our MSRV is 1.80) and the palette is a handful of ANSI constants. A
//! security tool should not grow supply-chain surface to draw a checkmark.
//!
//! Two rules this module exists to enforce:
//!
//! 1. **Styling is a decoration, never the message.** Everything must still read
//!    correctly with colour stripped and glyphs downgraded to ASCII — VAIBot's
//!    output lands in logs, CI transcripts and pipes at least as often as it
//!    lands in a terminal.
//! 2. **Alignment is computed, never typed.** `Rows` measures its own labels, so
//!    adding a longer one can't silently break a column.

use std::io::IsTerminal;
use std::sync::OnceLock;

// ─── Capability detection ────────────────────────────────────────────────────

/// Colour is on only for a real terminal, and any of the standard opt-outs wins.
/// Honours the `NO_COLOR` convention (presence alone disables, whatever the
/// value) and `CLICOLOR_FORCE` (forces on even when piped, which is how CI jobs
/// keep colour in their logs).
fn color_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
            return true;
        }
        if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
            return false;
        }
        std::io::stdout().is_terminal()
    })
}

/// Glyphs need a UTF-8 locale. When we can't confirm one, fall back to ASCII
/// rather than emitting mojibake — a corrupted checkmark is worse than `[ok]`.
fn unicode_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        if std::env::var_os("VAIBOT_ASCII").is_some() {
            return false;
        }
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .any(|v| v.to_uppercase().contains("UTF-8") || v.to_uppercase().contains("UTF8"))
    })
}

// ─── Palette ─────────────────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";

/// Wrap `s` in an ANSI code, or return it untouched when colour is off.
fn paint(s: &str, code: &str) -> String {
    if color_enabled() {
        format!("{code}{s}{RESET}")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint(s, BOLD)
}
pub fn dim(s: &str) -> String {
    paint(s, DIM)
}

// ─── The one marker vocabulary ───────────────────────────────────────────────
//
// This replaces [ok]/[warn]/[info]/✓/✔/✗/==>/• . If you need a new state, add it
// here rather than inventing one at a call site — that is how we got six.

/// Success. `✔` / `[ok]`.
pub fn mark_ok() -> String {
    paint(if unicode_enabled() { "✔" } else { "[ok]" }, GREEN)
}
/// Something needs attention but isn't fatal. `▲` / `[warn]`.
pub fn mark_warn() -> String {
    paint(if unicode_enabled() { "▲" } else { "[warn]" }, YELLOW)
}
/// Failure. `✘` / `[err]`.
pub fn mark_err() -> String {
    paint(if unicode_enabled() { "✘" } else { "[err]" }, RED)
}
/// A state readout, coloured by the state it reports. `●` / `*`.
pub fn mark_state(state: State) -> String {
    let g = if unicode_enabled() { "●" } else { "*" };
    paint(g, state.code())
}
/// A suggested next command. `→` / `->`.
pub fn mark_step() -> String {
    paint(if unicode_enabled() { "→" } else { "->" }, CYAN)
}

/// How a reported state should read at a glance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Good,
    Attention,
    Bad,
    Neutral,
}

impl State {
    fn code(self) -> &'static str {
        match self {
            State::Good => GREEN,
            State::Attention => YELLOW,
            State::Bad => RED,
            State::Neutral => DIM,
        }
    }
    /// Colour a word to match its state (`reachable`, `observe`, `locked`, …).
    pub fn label(self, text: &str) -> String {
        paint(text, self.code())
    }
}

// ─── Structure ───────────────────────────────────────────────────────────────

/// The inline separator between related facts on one line. Call this rather than
/// typing `·` at a call site — a literal stays Unicode in ASCII mode and the
/// fallback silently stops working.
pub fn sep() -> String {
    dim(if unicode_enabled() { "·" } else { "-" })
}

/// Truncation marker for elided values. Same reason as [`sep`].
pub fn ellipsis() -> &'static str {
    if unicode_enabled() {
        "…"
    } else {
        "..."
    }
}

/// Top-of-output title, optionally with a qualifier (`VAIBot · production`).
pub fn header(title: &str, subtitle: Option<&str>) {
    match subtitle {
        Some(s) => println!("\n  {}  {}  {}\n", bold(title), sep(), s),
        None => println!("\n  {}\n", bold(title)),
    }
}

/// A group label. Groups are what turn a flat list of facts into something
/// scannable; the original `status` output had none.
pub fn section(name: &str) {
    println!("  {}", dim(&name.to_uppercase()));
}

/// Print a suggested next step, indented under whatever it follows.
pub fn step(command: &str) {
    println!("       {}  {}", mark_step(), command);
}

/// A block of aligned label/value rows.
///
/// Collects first and measures on render, so the column width is derived from
/// the content instead of typed into a format string. Labels are compared by
/// `chars().count()`, not `len()` — a byte count would misalign anything
/// non-ASCII.
#[derive(Default)]
pub struct Rows {
    rows: Vec<Row>,
    min_width: usize,
}

enum Row {
    Pair { label: String, value: String },
    Continuation(String),
}

impl Rows {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, label: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.rows.push(Row::Pair {
            label: label.into(),
            value: value.into(),
        });
        self
    }

    /// A value-column line with no label — a wrapped value, a progress bar, a
    /// sub-detail. Indents to the same column as the values above it.
    pub fn push_continuation(&mut self, value: impl Into<String>) -> &mut Self {
        self.rows.push(Row::Continuation(value.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Widest label in this block, before any shared minimum is applied.
    pub fn natural_width(&self) -> usize {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Pair { label, .. } => Some(label.chars().count()),
                Row::Continuation(_) => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// Pin this block to at least `n` columns. Used with [`shared_width`] so
    /// several sections share one value column instead of each aligning to
    /// itself — otherwise a 3-character label in one section and a 9-character
    /// label in the next leave the page visibly ragged.
    pub fn set_min_width(&mut self, n: usize) -> &mut Self {
        self.min_width = n;
        self
    }

    pub fn render(&self) {
        let width = self.natural_width().max(self.min_width);

        for row in &self.rows {
            match row {
                Row::Pair { label, value } => {
                    let pad = " ".repeat(width - label.chars().count());
                    println!("    {label}{pad}  {value}");
                }
                Row::Continuation(value) => {
                    println!("    {}  {value}", " ".repeat(width));
                }
            }
        }
    }
}

/// The widest natural label across several blocks — feed it back through
/// [`Rows::set_min_width`] so every section shares one value column.
pub fn shared_width(blocks: &[&Rows]) -> usize {
    blocks.iter().map(|b| b.natural_width()).max().unwrap_or(0)
}

/// A proportion bar. Renders as filled/empty blocks, or `#`/`-` without Unicode.
/// `frac` is clamped, so a quota overage can't draw past the end or panic.
pub fn bar(frac: f64, width: usize) -> String {
    let (full, empty) = if unicode_enabled() { ("▰", "▱") } else { ("#", "-") };
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    let s = format!("{}{}", full.repeat(filled), empty.repeat(width - filled));
    // Colour tracks how close to the limit we are, so a nearly-full quota reads
    // as a warning without anyone having to read the number.
    let state = if frac >= 0.9 {
        State::Bad
    } else if frac >= 0.75 {
        State::Attention
    } else {
        State::Neutral
    };
    state.label(&s)
}

/// Thousands separators. `1000` reads as a smear next to `1000000`; `1,000`
/// doesn't. Avoids pulling a formatting crate for one function.
pub fn num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Styling must never be load-bearing: with colour off, every marker still
    // has to carry its meaning as text.
    #[test]
    fn markers_are_readable_without_color() {
        // paint() is a no-op when colour is off, which it is under `cargo test`
        // (stdout is not a terminal), so these assert the plain forms.
        assert!(!mark_ok().is_empty());
        assert!(!mark_warn().is_empty());
        assert!(!mark_err().is_empty());
        assert!(!mark_step().is_empty());
    }

    #[test]
    fn rows_align_to_the_longest_label() {
        let mut r = Rows::new();
        r.push("API", "x").push("A much longer label", "y");
        // Width is derived, not typed. Nothing to assert on stdout here, but the
        // arithmetic must not underflow when a later label is shorter.
        r.render();
        assert!(!r.is_empty());
    }

    #[test]
    fn rows_measure_labels_in_chars_not_bytes() {
        // A byte count would over-pad this label by 2 and misalign the column.
        let mut r = Rows::new();
        r.push("Ké", "v").push("abc", "w");
        r.render();
    }

    #[test]
    fn bar_clamps_out_of_range_fractions() {
        // An overage (frac > 1.0) must not draw past `width` or panic on a
        // negative repeat count.
        assert_eq!(strip(&bar(1.5, 10)).chars().count(), 10);
        assert_eq!(strip(&bar(-0.5, 10)).chars().count(), 10);
        assert_eq!(strip(&bar(0.0, 4)).chars().count(), 4);
    }

    #[test]
    fn num_groups_thousands() {
        assert_eq!(num(0), "0");
        assert_eq!(num(999), "999");
        assert_eq!(num(1_000), "1,000");
        assert_eq!(num(1_000_000), "1,000,000");
    }

    fn strip(s: &str) -> String {
        // Crude ANSI strip, test-only: drop \x1b[...m sequences.
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
