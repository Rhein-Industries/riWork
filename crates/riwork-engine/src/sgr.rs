//! The SGR-only filter of `riwork shell output --styled`.
//!
//! `tmux capture-pane -e` writes colors and text attributes as SGR sequences
//! (`ESC [ ... m`), but also OSC hyperlinks, charset shifts and, in other tmux
//! versions, more. The phone renders exactly one kind of escape, so this
//! filter keeps well-formed SGR sequences byte for byte and removes every
//! other escape or control sequence, whatever introduces it.
//!
//! What stays:
//! - printable text (all of Unicode except the control characters below);
//! - `\n` and `\t`;
//! - `ESC [ P m` where `P` is at most 64 characters of digits, `;` and `:`
//!   (so `ESC [ m`, `ESC [ 1 ; 31 m`, `ESC [ 38 ; 5 ; 200 m`,
//!   `ESC [ 38 ; 2 ; 1 ; 2 ; 3 m` and the colon forms).
//!
//! What goes, including the whole sequence and never just its introducer:
//! - CSI with any other final byte (`ESC [ 2 J`, `ESC [ ? 25 l`, ...), with a
//!   private-parameter prefix (`ESC [ > 4 ; 2 m` is not SGR), or with
//!   intermediate bytes;
//! - OSC, DCS, SOS, PM and APC strings, up to BEL, ST (`ESC \`) or the end of
//!   the line (a string never swallows a newline);
//! - every other escape: charset selection (`ESC ( 0`), `ESC 7`, `ESC c`, ...;
//! - the 8-bit C1 forms (U+0080 to U+009F) of all of the above;
//! - the remaining C0 controls and DEL, including SO and SI, CR and BEL.
//!
//! A sequence that is cut short by a byte it cannot contain (a control
//! character, or a new ESC) is dropped up to that byte, which is then read
//! normally, as a terminal would. `\n` therefore survives everything, which
//! keeps the line count, and with it the screen rule of `shell output`,
//! intact.
use std::{iter::Peekable, str::Chars};

/// The longest parameter string of a kept SGR sequence. A truecolor
/// foreground and background with several attributes is about 50 characters.
const MAX_SGR_PARAMETERS: usize = 64;

/// `input` with everything but text, newlines, tabs and SGR sequences removed.
pub fn keep_sgr_only(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\t' => output.push(c),
            '\u{1b}' => escape(&mut chars, &mut output),
            // 8-bit CSI is never kept: the phone only receives the 7-bit form.
            '\u{9b}' => control_sequence(&mut chars, &mut output, false),
            // DCS, SOS, OSC, PM and APC.
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => string_body(&mut chars),
            control if control.is_control() => {}
            text => output.push(text),
        }
    }
    output
}

fn between(c: char, low: char, high: char) -> bool {
    (low..=high).contains(&c)
}

/// After `ESC`.
fn escape(chars: &mut Peekable<Chars<'_>>, output: &mut String) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            control_sequence(chars, output, true);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            string_body(chars);
        }
        // Intermediates then a final byte: `ESC ( 0`, `ESC # 8`, `ESC SP F`.
        Some(c) if between(c, ' ', '/') => {
            while chars.next_if(|&c| between(c, ' ', '/')).is_some() {}
            chars.next_if(|&c| between(c, '0', '~'));
        }
        // One-byte escapes: `ESC 7`, `ESC =`, `ESC c`, `ESC \`.
        Some(c) if between(c, '0', '~') => {
            chars.next();
        }
        // A lone ESC: the next character is read as text or as a control.
        _ => {}
    }
}

/// After `CSI`: parameter bytes, intermediate bytes, one final byte. Only an
/// intact SGR sequence is written, and only when `keep` allows it.
fn control_sequence(chars: &mut Peekable<Chars<'_>>, output: &mut String, keep: bool) {
    let mut parameters = String::new();
    while let Some(c) = chars.next_if(|&c| between(c, '0', '?')) {
        parameters.push(c);
    }
    let mut intermediates = false;
    while chars.next_if(|&c| between(c, ' ', '/')).is_some() {
        intermediates = true;
    }
    // Anything else aborts the sequence and is left to the caller.
    if chars.next_if_eq(&'m').is_some() {
        let sgr = keep
            && !intermediates
            && parameters.len() <= MAX_SGR_PARAMETERS
            && parameters
                .chars()
                .all(|c| c.is_ascii_digit() || c == ';' || c == ':');
        if sgr {
            output.push_str("\u{1b}[");
            output.push_str(&parameters);
            output.push('m');
        }
    } else {
        chars.next_if(|&c| between(c, '@', '~'));
    }
}

/// After the introducer of an OSC, DCS, SOS, PM or APC string: consume the
/// body and its terminator. A newline or an ESC that does not start ST ends
/// the string without being consumed.
fn string_body(chars: &mut Peekable<Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        match c {
            '\u{07}' | '\u{9c}' => {
                chars.next();
                return;
            }
            '\u{1b}' => {
                let mut ahead = chars.clone();
                ahead.next();
                if ahead.peek() == Some(&'\\') {
                    chars.next();
                    chars.next();
                }
                return;
            }
            '\n' => return,
            _ => {
                chars.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keep_sgr_only as filter;

    const ESC: &str = "\u{1b}";

    /// `text` with every well-formed SGR sequence removed.
    fn without_sgr(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                out.push(c);
                continue;
            }
            assert_eq!(chars.next(), Some('['), "ESC without CSI in {text:?}");
            let mut closed = false;
            for c in chars.by_ref() {
                if c == 'm' {
                    closed = true;
                    break;
                }
                assert!(
                    c.is_ascii_digit() || c == ';' || c == ':',
                    "not SGR in {text:?}"
                );
            }
            assert!(closed, "unterminated SGR in {text:?}");
        }
        out
    }

    fn assert_clean(filtered: &str) {
        let text = without_sgr(filtered);
        assert!(
            text.chars()
                .all(|c| !c.is_control() || c == '\n' || c == '\t'),
            "control characters survived: {filtered:?}"
        );
    }

    #[test]
    fn colors_and_attributes_pass_byte_for_byte() {
        for sgr in [
            "\u{1b}[m",
            "\u{1b}[0m",
            "\u{1b}[1m",
            "\u{1b}[2m",
            "\u{1b}[3m",
            "\u{1b}[4m",
            "\u{1b}[7m",
            "\u{1b}[22m",
            "\u{1b}[1;31m",
            "\u{1b}[38;5;200m",
            "\u{1b}[48;5;7m",
            "\u{1b}[38;2;10;20;30m",
            "\u{1b}[48;2;1;2;3m",
            "\u{1b}[38:2::10:20:30m",
            "\u{1b}[4:3m",
            "\u{1b}[39m",
            "\u{1b}[49m",
            "\u{1b}[;m",
        ] {
            let input = format!("a{sgr}b{ESC}[0mc");
            assert_eq!(filter(&input), input, "{sgr:?}");
        }
        let line = "\u{1b}[1m\u{1b}[31mred bold\u{1b}[0m plain\n\u{1b}[0m\ttab \u{e9}\u{4e16}\n";
        assert_eq!(filter(line), line);
    }

    #[test]
    fn other_csi_sequences_are_removed_whole() {
        for csi in [
            "\u{1b}[2J",
            "\u{1b}[H",
            "\u{1b}[10;5H",
            "\u{1b}[1;5A",
            "\u{1b}[K",
            "\u{1b}[6n",
            "\u{1b}[?25l",
            "\u{1b}[?1049h",
            "\u{1b}[?2004l",
            "\u{1b}[>c",
            "\u{1b}[0 q",
            "\u{1b}[!p",
            "\u{1b}[2;3;4r",
            "\u{1b}[s",
        ] {
            assert_eq!(filter(&format!("a{csi}b")), "ab", "{csi:?}");
        }
    }

    #[test]
    fn a_final_m_alone_does_not_make_a_sequence_sgr() {
        for csi in [
            // Private-parameter prefixes: XTMODKEYS and friends.
            "\u{1b}[>4;2m",
            "\u{1b}[?4m",
            "\u{1b}[<1m",
            "\u{1b}[=1m",
            // Intermediate bytes before the final byte.
            "\u{1b}[1 m",
            "\u{1b}[1!m",
            "\u{1b}[1$m",
            // Too long to be a real SGR.
            &format!("\u{1b}[{}m", "1;".repeat(40)),
        ] {
            assert_eq!(filter(&format!("a{csi}b")), "ab", "{csi:?}");
        }
        // The longest kept run is exactly at the limit.
        let edge = format!("\u{1b}[{}m", "1".repeat(64));
        assert_eq!(filter(&edge), edge);
        assert_eq!(filter(&format!("\u{1b}[{}m", "1".repeat(65))), "");
    }

    #[test]
    fn osc_dcs_apc_and_friends_are_removed_up_to_their_terminator() {
        assert_eq!(
            filter("x\u{1b}]8;;http://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\y"),
            "xlinky"
        );
        assert_eq!(filter("x\u{1b}]0;title\u{7}y"), "xy");
        assert_eq!(filter("x\u{1b}]52;c;SGVsbG8=\u{1b}\\y"), "xy");
        assert_eq!(filter("x\u{1b}P1$r0m\u{1b}\\y"), "xy");
        assert_eq!(filter("x\u{1b}_Gf=100;AAAA\u{1b}\\y"), "xy");
        assert_eq!(filter("x\u{1b}^private\u{1b}\\y"), "xy");
        assert_eq!(filter("x\u{1b}Xsos\u{7}y"), "xy");
        // An SGR that follows a string on the same line survives it.
        assert_eq!(filter("\u{1b}]0;t\u{7}\u{1b}[31mred"), "\u{1b}[31mred");
        // A string never swallows a newline, terminated or not.
        assert_eq!(filter("a\u{1b}]0;never closed\nb\n"), "a\nb\n");
        assert_eq!(filter("a\u{1b}Pdcs\nb"), "a\nb");
        // An ESC that is not ST starts a new sequence instead.
        assert_eq!(filter("a\u{1b}]0;t\u{1b}[31mb"), "a\u{1b}[31mb");
        assert_eq!(filter("a\u{1b}]0;t\u{1b}"), "a");
        assert_eq!(filter("a\u{1b}]0;never closed"), "a");
    }

    #[test]
    fn charset_and_single_byte_escapes_are_removed() {
        for escape in [
            "\u{1b}(0", "\u{1b}(B", "\u{1b})0", "\u{1b}#8", "\u{1b} F", "\u{1b}7", "\u{1b}8",
            "\u{1b}=", "\u{1b}>", "\u{1b}c", "\u{1b}M", "\u{1b}D", "\u{1b}E", "\u{1b}\\",
            "\u{1b}~", "\u{1b}%G",
        ] {
            assert_eq!(filter(&format!("a{escape}b")), "ab", "{escape:?}");
        }
        // Shift out and in, as `capture-pane -e` writes for line drawing.
        assert_eq!(filter("\u{e}lqk\u{f}"), "lqk");
    }

    #[test]
    fn eight_bit_controls_never_leave_partial_sequences() {
        // 8-bit CSI, even with an SGR body, is removed whole.
        assert_eq!(filter("a\u{9b}31mb"), "ab");
        assert_eq!(filter("a\u{9b}2Jb"), "ab");
        assert_eq!(filter("a\u{9d}0;title\u{7}b"), "ab");
        assert_eq!(filter("a\u{9d}0;title\u{9c}b"), "ab");
        assert_eq!(filter("a\u{90}dcs\u{9c}b"), "ab");
        assert_eq!(filter("a\u{9f}apc\u{9c}b"), "ab");
        assert_eq!(filter("a\u{85}\u{80}\u{99}b"), "ab");
    }

    #[test]
    fn control_characters_are_removed_but_newline_and_tab_stay() {
        assert_eq!(
            filter("a\u{0}b\u{7}c\u{8}d\re\u{b}f\u{c}g\u{7f}h"),
            "abcdefgh"
        );
        assert_eq!(filter("a\tb\nc"), "a\tb\nc");
        assert_eq!(filter("a\r\nb"), "a\nb");
    }

    #[test]
    fn broken_sequences_are_dropped_and_the_next_byte_is_read_normally() {
        // Cut short by a new ESC: only the intact one survives.
        assert_eq!(filter("\u{1b}[3\u{1b}[31mx"), "\u{1b}[31mx");
        assert_eq!(filter("\u{1b}[3;\u{1b}]0;t\u{7}x"), "x");
        // Cut short by a newline: the newline survives.
        assert_eq!(filter("a\u{1b}[3\nb"), "a\nb");
        assert_eq!(filter("a\u{1b}(\nb"), "a\nb");
        assert_eq!(filter("a\u{1b}\nb"), "a\nb");
        // Cut short by text: the sequence is gone, the text is not.
        assert_eq!(filter("\u{1b}[3\u{e9}x"), "\u{e9}x");
        assert_eq!(filter("\u{1b}[31\u{1}m"), "m");
        // Cut short by the end of the input.
        for tail in [
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}[31",
            "\u{1b}[31;",
            "\u{1b}(",
            "\u{9b}",
        ] {
            assert_eq!(filter(&format!("a{tail}")), "a", "{tail:?}");
        }
        // A parameter byte after an intermediate is not a valid sequence.
        assert_eq!(filter("\u{1b}[1 1m"), "1m");
    }

    /// Small deterministic generator; no dependency for a fuzz loop.
    struct Xorshift(u64);
    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn nothing_but_sgr_survives_random_escape_soup() {
        const PIECES: &[&str] = &[
            "\u{1b}",
            "\u{1b}",
            "\u{1b}",
            "[",
            "[",
            "]",
            "(",
            ")",
            "\\",
            "P",
            "_",
            "^",
            "X",
            "m",
            "m",
            "H",
            "J",
            "?",
            ">",
            "<",
            "=",
            "!",
            " ",
            "$",
            ";",
            ":",
            "0",
            "1",
            "3",
            "8",
            "38",
            "200",
            "\u{7}",
            "\u{8}",
            "\r",
            "\n",
            "\t",
            "\u{e}",
            "\u{f}",
            "\u{0}",
            "\u{7f}",
            "\u{80}",
            "\u{85}",
            "\u{90}",
            "\u{98}",
            "\u{9b}",
            "\u{9c}",
            "\u{9d}",
            "\u{9e}",
            "\u{9f}",
            "\u{e9}",
            "\u{4e16}",
            "\u{1f600}",
            "x",
            "text ",
            "\u{1b}[31m",
            "\u{1b}[0m",
            "\u{1b}]8;;u\u{1b}\\",
            "\u{1b}[2J",
        ];
        let mut random = Xorshift(0x9e37_79b9_7f4a_7c15);
        for _ in 0..30_000 {
            let count = 1 + random.next() % 24;
            let input: String = (0..count)
                .map(|_| PIECES[(random.next() % PIECES.len() as u64) as usize])
                .collect();
            let filtered = filter(&input);
            assert_clean(&filtered);
            assert_eq!(filter(&filtered), filtered, "not idempotent for {input:?}");
            assert_eq!(
                filtered.matches('\n').count(),
                input.matches('\n').count(),
                "lines changed for {input:?}"
            );
            assert!(filtered.len() <= input.len());
        }
    }
}
