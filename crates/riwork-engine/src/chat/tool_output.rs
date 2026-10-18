//! Bounded, optional views of recognizable tool results. The original tool body remains
//! authoritative and available to older readers and the raw-output disclosure.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

const MAX_BYTES: usize = 128 * 1024;
const MAX_RESULTS: usize = 64;
const MAX_TEXT: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolOutput {
    FileSearch {
        query: String,
        hits: Vec<FileHit>,
        omitted: usize,
    },
    WebSearch {
        query: String,
        hits: Vec<WebHit>,
        omitted: usize,
        #[serde(default)]
        raw: String,
    },
    FileRead {
        path: String,
        text: String,
        truncated: bool,
    },
    Json {
        text: String,
        truncated: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileHit {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default)]
    pub preview: String,
}
impl FileHit {
    pub fn target(&self) -> String {
        self.line
            .map_or_else(|| self.path.clone(), |line| format!("{}:{line}", self.path))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebHit {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
}

pub(crate) fn lenient<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ToolOutput>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

fn bounded(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| value.get(*name)?.as_str())
}
fn path(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && !value.contains("://")
        && !value.starts_with("data:"))
    .then(|| value.to_owned())
}
fn query(input: &Value) -> String {
    bounded(
        field(input, &["query", "pattern", "search", "regex"]).unwrap_or_default(),
        512,
    )
}
fn array(value: &Value) -> Option<&Vec<Value>> {
    value.as_array().or_else(|| {
        ["matches", "results", "files", "links", "sources"]
            .iter()
            .find_map(|key| value.get(*key)?.as_array())
    })
}
fn file_hit(value: &Value) -> Option<FileHit> {
    if let Some(text) = value.as_str() {
        return path(text).map(|path| FileHit {
            path,
            line: None,
            preview: String::new(),
        });
    }
    let value = value
        .get("data")
        .filter(|_| value["type"] == "match")
        .unwrap_or(value);
    let name = field(value, &["path", "file", "file_path", "filename"])
        .or_else(|| value.get("path")?.get("text")?.as_str())?;
    let line = ["line", "line_number", "lineNumber"]
        .iter()
        .find_map(|name| value.get(*name)?.as_u64())
        .and_then(|line| u32::try_from(line).ok())
        .filter(|line| *line > 0);
    let preview = field(value, &["preview", "text", "content", "snippet"])
        .or_else(|| value.get("lines")?.get("text")?.as_str())
        .unwrap_or_default();
    Some(FileHit {
        path: path(name)?,
        line,
        preview: bounded(preview.trim_end(), 2048),
    })
}
fn web_hit(value: &Value) -> Option<WebHit> {
    let url = field(value, &["url", "href", "link"])?;
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.len() > 4096
        || url.chars().any(char::is_control)
    {
        return None;
    }
    Some(WebHit {
        title: bounded(field(value, &["title", "name"]).unwrap_or(url), 512),
        url: url.into(),
        snippet: bounded(
            field(value, &["snippet", "description", "text", "content"]).unwrap_or_default(),
            2048,
        ),
    })
}
fn unwrap_output(output: &str) -> (String, Option<Value>) {
    let mut text = output.to_owned();
    for _ in 0..3 {
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return (text, None);
        };
        if let Some(structured) = value
            .get("structuredContent")
            .filter(|value| !value.is_null())
        {
            return (text, Some(structured.clone()));
        }
        if let Some(parts) = value.get("content").and_then(Value::as_array) {
            let nested = parts
                .iter()
                .filter_map(|part| part.get("text")?.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !nested.is_empty() && nested != text {
                text = nested;
                continue;
            }
        }
        return (text, Some(value));
    }
    let value = serde_json::from_str(&text).ok();
    (text, value)
}

/// Unknown or ambiguous text stays raw. Recognition never executes a URL or reads a file.
pub fn parse(tool: &str, input: &Value, output: &str) -> Option<ToolOutput> {
    if output.len() > MAX_BYTES || output.trim().is_empty() {
        return None;
    }
    let name = tool
        .rsplit("__")
        .next()
        .unwrap_or(tool)
        .to_ascii_lowercase()
        .replace('-', "_");
    let (text, json) = unwrap_output(output);
    if matches!(
        name.as_str(),
        "grep" | "ripgrep" | "rg" | "glob" | "search_files" | "file_search" | "find_files"
    ) {
        let mut hits = Vec::new();
        let mut total: usize = 0;
        let mut take = |hit: FileHit| {
            total += 1;
            if hits.len() < MAX_RESULTS {
                hits.push(hit);
            }
        };
        if let Some(values) = json.as_ref().and_then(array) {
            for value in values {
                if let Some(hit) = file_hit(value) {
                    take(hit);
                }
            }
        } else {
            for line in text.lines() {
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    if value["type"] == "match" {
                        if let Some(hit) = file_hit(&value) {
                            take(hit);
                        }
                    }
                    continue;
                }
                let file_list =
                    input.get("output_mode").and_then(Value::as_str) == Some("files_with_matches");
                if file_list || matches!(name.as_str(), "glob" | "find_files") {
                    if let Some(path) = path(line.trim())
                        .filter(|path| file_list || path.contains('/') || path.contains('.'))
                    {
                        take(FileHit {
                            path,
                            line: None,
                            preview: String::new(),
                        });
                    }
                } else {
                    // Grep content mode is path:line:text. Scan for the numeric field so
                    // spaces and colons in a filename do not split the path prematurely.
                    for (at, _) in line.match_indices(':') {
                        let Some((number, preview)) = line[at + 1..].split_once(':') else {
                            continue;
                        };
                        if let Ok(number) = number.parse::<u32>() {
                            if number > 0
                                && let Some(path) = path(&line[..at])
                            {
                                take(FileHit {
                                    path,
                                    line: Some(number),
                                    preview: bounded(preview, 2048),
                                });
                            }
                            break;
                        }
                    }
                }
            }
        }
        if total > 0 || json.as_ref().and_then(array).is_some_and(Vec::is_empty) {
            return Some(ToolOutput::FileSearch {
                query: query(input),
                omitted: total.saturating_sub(hits.len()),
                hits,
            });
        }
    }
    if matches!(
        name.as_str(),
        "websearch" | "web_search" | "search_web" | "search"
    ) {
        let embedded = text.find("Links: [").and_then(|start| {
            let start = start + "Links: ".len();
            let end = text[start..].rfind(']')? + start + 1;
            serde_json::from_str::<Value>(&text[start..end]).ok()
        });
        if let Some(values) = json.as_ref().or(embedded.as_ref()).and_then(array) {
            let all: Vec<_> = values.iter().filter_map(web_hit).collect();
            let omitted = all.len().saturating_sub(MAX_RESULTS);
            if !all.is_empty() || values.is_empty() {
                return Some(ToolOutput::WebSearch {
                    query: query(input),
                    hits: all.into_iter().take(MAX_RESULTS).collect(),
                    omitted,
                    raw: bounded(output, MAX_TEXT),
                });
            }
        }
    }
    if matches!(
        name.as_str(),
        "read" | "read_file" | "readfile" | "file_read"
    ) {
        let name = field(input, &["file_path", "path", "filename"])?;
        let content = json
            .as_ref()
            .and_then(|value| field(value, &["content", "text"]))
            .unwrap_or(&text);
        return Some(ToolOutput::FileRead {
            path: path(name)?,
            text: bounded(content, MAX_TEXT),
            truncated: content.len() > MAX_TEXT,
        });
    }
    let value = json.filter(|value| value.is_object() || value.is_array())?;
    let pretty = serde_json::to_string_pretty(&value).ok()?;
    Some(ToolOutput::Json {
        text: bounded(&pretty, MAX_TEXT),
        truncated: pretty.len() > MAX_TEXT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn grep_preserves_unicode_colon_paths_and_line_numbers() {
        let Some(ToolOutput::FileSearch { hits, .. }) = parse(
            "Grep",
            &json!({"pattern":"needle"}),
            "src/ü:file.rs:42:let needle = 1;\nnot a match",
        ) else {
            panic!()
        };
        assert_eq!(
            hits,
            vec![FileHit {
                path: "src/ü:file.rs".into(),
                line: Some(42),
                preview: "let needle = 1;".into()
            }]
        );
        assert_eq!(hits[0].target(), "src/ü:file.rs:42");
    }
    #[test]
    fn recognizes_rg_json_and_mcp_wrapped_sources() {
        let Some(ToolOutput::FileSearch { hits, .. }) = parse(
            "grep",
            &Value::Null,
            r#"{"type":"match","data":{"path":{"text":"a.rs"},"line_number":2,"lines":{"text":"hi\n"}}}"#,
        ) else {
            panic!()
        };
        assert_eq!(hits[0].preview, "hi");
        let output = json!({"content":[{"type":"text","text":json!({"results":[{"title":"Docs","url":"https://example.com","snippet":"Answer"},{"url":"javascript:alert(1)"}]}).to_string()}]}).to_string();
        let Some(ToolOutput::WebSearch { hits, .. }) =
            parse("mcp__web__web_search", &json!({"query":"q"}), &output)
        else {
            panic!()
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Docs");
    }
    #[test]
    fn bounds_result_counts_and_does_not_interpret_unknown_plain_text() {
        let output = (0..80)
            .map(|n| format!("a.rs:{}:match", n + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let Some(ToolOutput::FileSearch { hits, omitted, .. }) =
            parse("Grep", &Value::Null, &output)
        else {
            panic!()
        };
        assert_eq!(hits.len(), 64);
        assert_eq!(omitted, 16);
        assert!(parse("unknown", &Value::Null, "a.rs:5:message").is_none());
        assert!(parse("Grep", &Value::Null, &"a".repeat(MAX_BYTES + 1)).is_none());
    }
    #[test]
    fn explicit_files_mode_preserves_paths_without_line_numbers() {
        let Some(ToolOutput::FileSearch { hits, .. }) = parse(
            "Grep",
            &serde_json::json!({"output_mode":"files_with_matches"}),
            "LICENSE\nsrc/日本語.rs",
        ) else {
            panic!()
        };
        assert_eq!(
            hits.iter().map(FileHit::target).collect::<Vec<_>>(),
            ["LICENSE", "src/日本語.rs"]
        );
    }
    #[test]
    fn unknown_presentation_remains_readable() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(deserialize_with = "lenient")]
            tool: Option<ToolOutput>,
        }
        assert!(
            serde_json::from_value::<Wrapper>(json!({"tool":{"kind":"future"}}))
                .unwrap()
                .tool
                .is_none()
        );
    }
}
