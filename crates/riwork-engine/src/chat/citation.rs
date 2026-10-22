//! Codex's file citations: a directive an agent writes into a message to point at a
//! file, on a line of its own or inside a sentence:
//!
//! `:codex-file-citation{path="/work/out/Bericht März.docx" artifact_kind="document" purpose="output"}`
//!
//! The chat views draw one as the file it names; what turns a message into plain text
//! writes the file's name or path instead. A directive that does not read (no `path`,
//! no closing brace, a line break inside) stays text, and nothing here panics.

use std::{borrow::Cow, ops::Range};

const OPEN: &str = ":codex-file-citation{";
/// Longer than any path a file link accepts. A brace that never closes costs no more
/// than this to rule out.
const MAX_LEN: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileCitation {
    pub path: String,
    /// What the agent says the file is (`document`, `image`, …), if it says.
    pub artifact_kind: Option<String>,
    /// Why it cites it (`output`, `input`, …), if it says.
    pub purpose: Option<String>,
}

impl FileCitation {
    /// The last part of the path, or the whole path when it has no other.
    pub fn name(&self) -> &str {
        self.path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.path)
    }
}

/// The citation at the start of `text` and how many bytes it takes. Attributes are
/// `name="value"` pairs apart by blanks; a value may hold `\"` and `\\`, and any other
/// character but a line break. Unknown attributes are skipped.
pub fn parse(text: &str) -> Option<(FileCitation, usize)> {
    if !text.starts_with(OPEN) {
        return None;
    }
    let mut at = OPEN.len();
    let (mut path, mut artifact_kind, mut purpose) = (None, None, None);
    loop {
        let rest = &text[at..];
        let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        at += blanks;
        let rest = &text[at..];
        if rest.starts_with('}') {
            at += 1;
            break;
        }
        // Attributes are apart by blanks: `a="1"b="2"` is not a directive.
        if blanks == 0 && at > OPEN.len() {
            return None;
        }
        let name = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .filter(|&end| end > 0)?;
        let (name, rest) = rest.split_at(name);
        let rest = rest.strip_prefix("=\"")?;
        at += name.len() + 2;
        let mut value = String::new();
        let mut chars = rest.char_indices();
        let end = loop {
            let (offset, c) = chars.next()?;
            if at + offset > MAX_LEN {
                return None;
            }
            match c {
                '"' => break offset,
                '\n' | '\r' => return None,
                '\\' if matches!(rest[offset + 1..].chars().next(), Some('"' | '\\')) => {
                    value.push(chars.next()?.1);
                }
                c => value.push(c),
            }
        };
        at += end + 1;
        let slot = match name {
            "path" => &mut path,
            "artifact_kind" => &mut artifact_kind,
            "purpose" => &mut purpose,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(value);
        }
    }
    let path = path.filter(|path| !path.trim().is_empty())?;
    let given = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    Some((
        FileCitation {
            path,
            artifact_kind: given(artifact_kind),
            purpose: given(purpose),
        },
        at,
    ))
}

/// Every citation in `text`, where it is.
pub fn find(text: &str) -> Vec<(Range<usize>, FileCitation)> {
    let mut found = Vec::new();
    let mut end = 0;
    for (start, _) in text.match_indices(OPEN) {
        if start < end {
            continue;
        }
        if let Some((citation, used)) = parse(&text[start..]) {
            end = start + used;
            found.push((start..end, citation));
        }
    }
    found
}

/// The citations of a line that holds nothing else, at least one.
pub fn line(line: &str) -> Option<Vec<FileCitation>> {
    let mut rest = line.trim();
    let mut files = Vec::new();
    while !rest.is_empty() {
        let (citation, used) = parse(rest)?;
        files.push(citation);
        rest = rest[used..].trim_start();
    }
    (!files.is_empty()).then_some(files)
}

/// `text` with each citation written as the name of its file.
pub fn with_names(text: &str) -> Cow<'_, str> {
    rewrite(text, FileCitation::name)
}

/// `text` with each citation written as its path.
pub fn with_paths(text: &str) -> Cow<'_, str> {
    rewrite(text, |citation| &citation.path)
}

fn rewrite(text: &str, write: fn(&FileCitation) -> &str) -> Cow<'_, str> {
    let found = find(text);
    if found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (range, citation) in found {
        out.push_str(&text[at..range.start]);
        out.push_str(write(&citation));
        at = range.end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "/Users/me/orca/projects/Baumer_featuring_UBS/outputs/ocra-ky3p/Baumer_Organisation_Fragen_und_Nachweise_20261021.docx";

    #[test]
    fn reads_path_kind_and_purpose_and_skips_unknown_attributes() {
        let source = format!(
            ":codex-file-citation{{path=\"{REPORT}\" artifact_kind=\"document\" line=\"4\" purpose=\"output\"}} after"
        );
        let (citation, used) = parse(&source).unwrap();
        assert_eq!(&source[used..], " after");
        assert_eq!(citation.path, REPORT);
        assert_eq!(citation.artifact_kind.as_deref(), Some("document"));
        assert_eq!(citation.purpose.as_deref(), Some("output"));
        assert_eq!(
            citation.name(),
            "Baumer_Organisation_Fragen_und_Nachweise_20261021.docx"
        );
        // Only `path` is needed, and values keep spaces, umlauts and escaped quotes.
        let (citation, _) =
            parse(r#":codex-file-citation{path="/tmp/Bericht März \"final\".pdf"}"#).unwrap();
        assert_eq!(citation.path, "/tmp/Bericht März \"final\".pdf");
        assert_eq!(citation.name(), "Bericht März \"final\".pdf");
        assert_eq!(citation.artifact_kind, None);
    }

    #[test]
    fn malformed_directives_are_not_citations() {
        for text in [
            ":codex-file-citation{}",
            ":codex-file-citation{artifact_kind=\"document\"}",
            ":codex-file-citation{path=\"\"}",
            ":codex-file-citation{path=\"/a.txt\"",
            ":codex-file-citation{path=\"/a.txt",
            ":codex-file-citation{path=\"/a\nb.txt\"}",
            ":codex-file-citation{path=/a.txt}",
            ":codex-file-citation{path=\"/a.txt\"purpose=\"x\"}",
            ":codex-file-citation{ = \"x\" path=\"/a.txt\"}",
            ":codex-file-citation{path=\"/a.txt\\",
            ":codex-file-citation",
            ":codex-file-citation{path=\"é",
            "codex-file-citation{path=\"/a.txt\"}",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
            assert!(find(text).is_empty(), "{text:?}");
            assert_eq!(with_names(text), text);
        }
        let long = format!(":codex-file-citation{{path=\"{}\"}}", "a".repeat(MAX_LEN));
        assert_eq!(parse(&long), None);
    }

    #[test]
    fn lines_of_citations_and_rewritten_text() {
        let cite = |path: &str| format!(":codex-file-citation{{path=\"{path}\"}}");
        let both = format!("  {} {}  ", cite("/a/one.docx"), cite("/b/two.pdf"));
        let files = line(&both).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].path, "/b/two.pdf");
        assert_eq!(line(&format!("See {}", cite("/a/one.docx"))), None);
        assert_eq!(line("   "), None);

        let text = format!(
            "{}\nQbit: 114 eigene Einträge, see {}.\n:codex-file-citation{{path=\"/x",
            cite(REPORT),
            cite("/b/two.pdf")
        );
        assert_eq!(
            with_names(&text),
            "Baumer_Organisation_Fragen_und_Nachweise_20261021.docx\nQbit: 114 eigene Einträge, see two.pdf.\n:codex-file-citation{path=\"/x"
        );
        assert!(with_paths(&text).starts_with(&format!("{REPORT}\n")));
        assert!(matches!(with_names("no citations"), Cow::Borrowed(_)));
        assert_eq!(
            FileCitation {
                path: "/dir/".into(),
                artifact_kind: None,
                purpose: None
            }
            .name(),
            "dir"
        );
    }
}
