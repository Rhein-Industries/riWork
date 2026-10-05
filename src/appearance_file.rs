//! `appearance.json`: the colors the desktop resolved, published under the
//! RiWork data directory for the phone companion.
//!
//! Kept free of GPUI so `remote/tests` compiles it under strict Clippy.
//! `remote/src/appearance.rs` validates the same document for the
//! `appearance.get` RPC; the accept/reject matrices in both crates' tests must
//! stay identical.
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;

pub const FILE_NAME: &str = "appearance.json";
/// A larger file is invalid. A real one is under a kilobyte.
pub const MAX_BYTES: u64 = 16 * 1024;
pub const VERSION: u8 = 1;
/// The CLI's error for a file that is missing or invalid. The phone maps it to
/// `not_found` "appearance not published".
pub const NOT_PUBLISHED: &str = "RiWork has not published its appearance yet; open the RiWork app";

/// A color as `0xRRGGBB`; the document spells it lowercase `#rrggbb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u32);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:06x}", self.0 & 0x00ff_ffff)
    }

    /// `#rrggbb`, either case. Nothing else: no shorthand, alpha or bare digits.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok().map(Self)
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| de::Error::custom("expected a #rrggbb color"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteColors {
    pub bg: Rgb,
    pub panel: Rgb,
    pub panel_active: Rgb,
    pub divider: Rgb,
    pub cyan: Rgb,
    pub magenta: Rgb,
    pub gold: Rgb,
    pub text: Rgb,
    pub muted: Rgb,
}

/// The terminal colors as shown: background, foreground and the 16 ANSI colors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    pub background: Rgb,
    pub foreground: Rgb,
    pub palette: [Rgb; 16],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub v: u8,
    /// Unix seconds of the last change to the colors.
    pub updated_at: u64,
    /// The palette background is dark: relative luminance below one half.
    pub dark: bool,
    pub palette: PaletteColors,
    /// Absent when the desktop could not read its terminal colors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalColors>,
    /// The Native skin is selected, so the phone draws its interface the native
    /// way too (system font, sentence case). Written only when true: a phone or
    /// relay from before this field ignores it, and one that finds it missing
    /// keeps the terminal look.
    #[serde(default, skip_serializing_if = "is_false")]
    pub native: bool,
    /// Settings → "Show microphone buttons for dictation" is on, so the phone shows its mic
    /// buttons (chat composers and the terminal key bar). Written only when true, like `native`: a document
    /// without it means off, and one from before the field reads as it always did.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mic: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Published {
    /// A snapshot not yet stamped: `publish` sets `updated_at` when it writes.
    pub fn new(dark: bool, palette: PaletteColors, terminal: Option<TerminalColors>) -> Self {
        Self {
            v: VERSION,
            updated_at: 0,
            dark,
            palette,
            terminal,
            native: false,
            mic: false,
        }
    }

    /// Equal in everything but the time of the last change.
    pub fn same_colors(&self, other: &Self) -> bool {
        Self {
            updated_at: other.updated_at,
            ..self.clone()
        } == *other
    }

    /// The file's bytes checked as a document. Fields this build does not know
    /// are dropped, so re-serializing yields exactly the contract's shape.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(format!("larger than {MAX_BYTES} bytes"));
        }
        let published: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if published.v != VERSION {
            return Err(format!("unsupported version {}", published.v));
        }
        Ok(published)
    }
}

/// The published document, or None when the file is missing, not a regular
/// file, too large or not a valid document.
pub fn read(home: &Path) -> Option<Published> {
    let path = home.join(FILE_NAME);
    // A named pipe would block the open, so look before opening.
    let metadata = fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Published::parse(&bytes).ok()
}

/// Writes `snapshot` stamped `now`, unless the file already holds these colors.
/// Any number of app processes may call this with the same colors: only the
/// first writes, and readers never see a partial file. Returns whether it wrote.
pub fn publish(home: &Path, snapshot: &Published, now: u64) -> Result<bool, String> {
    if read(home).is_some_and(|current| current.same_colors(snapshot)) {
        return Ok(false);
    }
    let document = Published {
        updated_at: now,
        ..snapshot.clone()
    };
    let mut data = serde_json::to_vec_pretty(&document)
        .map_err(|error| format!("Cannot encode appearance: {error}"))?;
    data.push(b'\n');
    write_private_file(home, &data)?;
    Ok(true)
}

/// A new private (0600) file renamed into place, so readers see the old file or
/// the whole new one, never a partial write. The file is derived state that the
/// next start publishes again, so it is not synced.
fn write_private_file(home: &Path, data: &[u8]) -> Result<(), String> {
    let path = home.join(FILE_NAME);
    let temporary = home.join(format!(".appearance-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("Cannot create {}: {error}", temporary.display()))?;
        file.write_all(data)
            .map_err(|error| format!("Cannot write appearance: {error}"))?;
        fs::rename(&temporary, &path)
            .map_err(|error| format!("Cannot replace {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn home() -> PathBuf {
        let path = std::env::temp_dir().join(format!("riwork-appearance-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn colors(base: u32) -> PaletteColors {
        PaletteColors {
            bg: Rgb(base),
            panel: Rgb(base + 1),
            panel_active: Rgb(base + 2),
            divider: Rgb(base + 3),
            cyan: Rgb(base + 4),
            magenta: Rgb(base + 5),
            gold: Rgb(base + 6),
            text: Rgb(base + 7),
            muted: Rgb(base + 8),
        }
    }

    fn terminal(base: u32) -> TerminalColors {
        TerminalColors {
            background: Rgb(base),
            foreground: Rgb(base + 1),
            palette: std::array::from_fn(|index| Rgb(base + 2 + index as u32)),
        }
    }

    fn snapshot(base: u32) -> Published {
        Published::new(true, colors(base), Some(terminal(base + 0x100)))
    }

    /// A complete document as another writer might produce it.
    fn document() -> Value {
        json!({
            "v": 1,
            "updated_at": 1_790_000_000u64,
            "dark": false,
            "palette": {
                "bg": "#fbf1c7", "panel": "#f4ebc1", "panel_active": "#ede5bb",
                "divider": "#d5cba1", "cyan": "#427b58", "magenta": "#8f3f71",
                "gold": "#8a5c00", "text": "#3c3836", "muted": "#665c54"
            },
            "terminal": {
                "background": "#fbf1c7",
                "foreground": "#3c3836",
                "palette": [
                    "#fbf1c7", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286",
                    "#689d6a", "#7c6f64", "#928374", "#9d0006", "#79740e", "#b57614",
                    "#076678", "#8f3f71", "#427b58", "#3c3836"
                ]
            }
        })
    }

    fn parse(value: &Value) -> Result<Published, String> {
        Published::parse(value.to_string().as_bytes())
    }

    #[test]
    fn colors_are_lowercase_hash_six_digit_hex() {
        assert_eq!(Rgb(0x0a0b0c).hex(), "#0a0b0c");
        assert_eq!(Rgb(0).hex(), "#000000");
        assert_eq!(Rgb(0xffffff).hex(), "#ffffff");
        assert_eq!(Rgb(0xABCDEF).hex(), "#abcdef");
        assert_eq!(Rgb::parse("#0a0b0c"), Some(Rgb(0x0a0b0c)));
        assert_eq!(Rgb::parse("#ABCDEF"), Some(Rgb(0xabcdef)));
        for bad in [
            "",
            "#",
            "#fff",
            "0a0b0c",
            "#0a0b0",
            "#0a0b0c0",
            "#0a0b0g",
            "# 0a0b0c",
            "#+a0b0c",
            "#0a0b0cff",
            " #0a0b0c",
            "#0a0b0c ",
            "rgb(1,2,3)",
            "#0a0b0é",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_document_has_the_contract_shape() {
        let mut published = snapshot(0x101010);
        published.updated_at = 1_790_000_123;
        let value = serde_json::to_value(&published).unwrap();
        assert_eq!(
            value,
            json!({
                "v": 1,
                "updated_at": 1_790_000_123u64,
                "dark": true,
                "palette": {
                    "bg": "#101010", "panel": "#101011", "panel_active": "#101012",
                    "divider": "#101013", "cyan": "#101014", "magenta": "#101015",
                    "gold": "#101016", "text": "#101017", "muted": "#101018"
                },
                "terminal": {
                    "background": "#101110",
                    "foreground": "#101111",
                    "palette": (0..16).map(|index| format!("#{:06x}", 0x101112 + index)).collect::<Vec<_>>()
                }
            })
        );
        // The text lists fields in the contract's order.
        let text = serde_json::to_string(&published).unwrap();
        let positions = [
            "\"v\"",
            "\"updated_at\"",
            "\"dark\"",
            "\"palette\"",
            "\"terminal\"",
        ]
        .map(|field| text.find(field).unwrap());
        assert!(positions.is_sorted(), "{text}");
        published.terminal = None;
        let value = serde_json::to_value(&published).unwrap();
        assert!(value.get("terminal").is_none(), "{value}");
        assert_eq!(parse(&value).unwrap(), published);
    }

    #[test]
    fn a_valid_document_is_accepted_and_normalised() {
        let published = parse(&document()).unwrap();
        assert!(!published.dark);
        assert_eq!(published.updated_at, 1_790_000_000);
        assert_eq!(published.palette.bg, Rgb(0xfbf1c7));
        assert_eq!(
            published.terminal.as_ref().unwrap().palette[15],
            Rgb(0x3c3836)
        );
        assert_eq!(serde_json::to_value(&published).unwrap(), document());

        let mut loud = document();
        loud["palette"]["bg"] = json!("#FBF1C7");
        loud["terminal"]["palette"][1] = json!("#CC241D");
        loud["future"] = json!({"anything": [1, 2, 3]});
        loud["palette"]["accent"] = json!("#123456");
        assert_eq!(
            serde_json::to_value(parse(&loud).unwrap()).unwrap(),
            document()
        );

        let mut without = document();
        without.as_object_mut().unwrap().remove("terminal");
        assert_eq!(parse(&without).unwrap().terminal, None);
        without["terminal"] = Value::Null;
        assert_eq!(parse(&without).unwrap().terminal, None);
    }

    #[test]
    fn an_invalid_document_is_rejected() {
        let mutate = |change: &dyn Fn(&mut Value)| {
            let mut value = document();
            change(&mut value);
            parse(&value)
        };
        assert!(mutate(&|_| {}).is_ok());
        assert!(mutate(&|v| v["v"] = json!(2)).is_err());
        assert!(mutate(&|v| v["v"] = json!(0)).is_err());
        assert!(mutate(&|v| v["v"] = json!("1")).is_err());
        assert!(mutate(&|v| drop(v.as_object_mut().unwrap().remove("v"))).is_err());
        assert!(mutate(&|v| drop(v.as_object_mut().unwrap().remove("dark"))).is_err());
        assert!(mutate(&|v| drop(v.as_object_mut().unwrap().remove("palette"))).is_err());
        assert!(mutate(&|v| drop(v.as_object_mut().unwrap().remove("updated_at"))).is_err());
        assert!(mutate(&|v| v["dark"] = json!("yes")).is_err());
        assert!(mutate(&|v| v["dark"] = json!(1)).is_err());
        assert!(mutate(&|v| v["updated_at"] = json!(-1)).is_err());
        assert!(mutate(&|v| v["updated_at"] = json!(1.5)).is_err());
        assert!(mutate(&|v| v["updated_at"] = json!("1790000000")).is_err());
        assert!(mutate(&|v| drop(v["palette"].as_object_mut().unwrap().remove("muted"))).is_err());
        assert!(mutate(&|v| v["palette"]["muted"] = json!(7)).is_err());
        assert!(mutate(&|v| v["palette"]["muted"] = json!("#fff")).is_err());
        assert!(mutate(&|v| v["palette"]["muted"] = json!("665c54")).is_err());
        assert!(mutate(&|v| v["palette"]["muted"] = json!("#66 c54")).is_err());
        assert!(mutate(&|v| v["palette"] = json!([])).is_err());
        assert!(mutate(&|v| v["terminal"] = json!({})).is_err());
        assert!(mutate(&|v| v["terminal"] = json!("dark")).is_err());
        assert!(
            mutate(&|v| drop(v["terminal"].as_object_mut().unwrap().remove("foreground"))).is_err()
        );
        assert!(mutate(&|v| v["terminal"]["background"] = json!("black")).is_err());
        assert!(mutate(&|v| drop(v["terminal"]["palette"].as_array_mut().unwrap().pop())).is_err());
        assert!(
            mutate(&|v| v["terminal"]["palette"]
                .as_array_mut()
                .unwrap()
                .push(json!("#000000")))
            .is_err()
        );
        assert!(mutate(&|v| v["terminal"]["palette"][3] = json!("#12345")).is_err());
        assert!(mutate(&|v| v["terminal"]["palette"] = json!({})).is_err());
        assert!(mutate(&|v| v["native"] = json!(true)).is_ok());
        assert!(mutate(&|v| v["native"] = json!(false)).is_ok());
        assert!(mutate(&|v| v["native"] = json!("yes")).is_err());
        assert!(mutate(&|v| v["native"] = json!(1)).is_err());
        assert!(mutate(&|v| v["native"] = Value::Null).is_err());
        assert!(mutate(&|v| v["mic"] = json!(true)).is_ok());
        assert!(mutate(&|v| v["mic"] = json!(false)).is_ok());
        assert!(mutate(&|v| v["mic"] = json!("yes")).is_err());
        assert!(mutate(&|v| v["mic"] = json!(1)).is_err());
        assert!(mutate(&|v| v["mic"] = Value::Null).is_err());
        assert!(mutate(&|v| v["mic"] = json!({})).is_err());
        for bytes in [
            &b""[..],
            b"{",
            b"null",
            b"[]",
            b"\"{}\"",
            b"{}",
            b"\xff\xfe",
        ] {
            assert!(Published::parse(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn a_document_over_sixteen_kib_is_invalid() {
        let mut text = document().to_string();
        assert!(Published::parse(text.as_bytes()).is_ok());
        text.push_str(&" ".repeat(MAX_BYTES as usize - text.len()));
        assert_eq!(text.len() as u64, MAX_BYTES);
        assert!(Published::parse(text.as_bytes()).is_ok());
        text.push(' ');
        assert!(Published::parse(text.as_bytes()).is_err());

        let home = home();
        fs::write(home.join(FILE_NAME), &text).unwrap();
        assert_eq!(read(&home), None);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_native_flag_is_written_only_when_set() {
        let mut published = snapshot(0x111111);
        // Off, the document is exactly what it was before the flag existed.
        let value = serde_json::to_value(&published).unwrap();
        assert!(value.get("native").is_none(), "{value}");
        assert!(!parse(&value).unwrap().native);
        published.native = true;
        let value = serde_json::to_value(&published).unwrap();
        assert_eq!(value["native"], true);
        let text = serde_json::to_string(&published).unwrap();
        assert!(
            text.find("\"terminal\"") < text.find("\"native\""),
            "{text}"
        );
        assert_eq!(parse(&value).unwrap(), published);
        // A desktop from before the flag, or with Native off, reads as not Native.
        let mut old = document();
        assert!(!parse(&old).unwrap().native);
        old["native"] = json!(false);
        assert_eq!(
            serde_json::to_value(parse(&old).unwrap()).unwrap(),
            document()
        );
        old["native"] = json!(true);
        assert!(parse(&old).unwrap().native);
    }

    #[test]
    fn the_mic_flag_is_written_only_when_set() {
        let mut published = snapshot(0x111111);
        // Off, the document is exactly what it was before the flag existed.
        let off = serde_json::to_vec_pretty(&published).unwrap();
        assert!(!String::from_utf8_lossy(&off).contains("\"mic\""));
        assert!(
            !parse(&serde_json::to_value(&published).unwrap())
                .unwrap()
                .mic
        );
        published.mic = true;
        let value = serde_json::to_value(&published).unwrap();
        assert_eq!(value["mic"], true);
        assert!(value.get("native").is_none(), "{value}");
        assert_eq!(parse(&value).unwrap(), published);
        // With Native too, both flags follow the terminal colors.
        published.native = true;
        let text = serde_json::to_string(&published).unwrap();
        assert!(
            text.find("\"terminal\"") < text.find("\"native\"")
                && text.find("\"native\"") < text.find("\"mic\""),
            "{text}"
        );
        // Missing or false is off, and false is dropped when written again.
        let mut old = document();
        assert!(!parse(&old).unwrap().mic);
        old["mic"] = json!(false);
        assert_eq!(
            serde_json::to_value(parse(&old).unwrap()).unwrap(),
            document()
        );
        old["mic"] = json!(true);
        assert!(parse(&old).unwrap().mic);
    }

    #[test]
    fn same_colors_ignores_only_the_time() {
        let first = snapshot(0x202020);
        let mut later = first.clone();
        later.updated_at = 99;
        assert!(first.same_colors(&later));
        assert!(later.same_colors(&first));
        let mut other = first.clone();
        other.dark = false;
        assert!(!first.same_colors(&other));
        other = first.clone();
        other.terminal = None;
        assert!(!first.same_colors(&other));
        other = first.clone();
        other.terminal.as_mut().unwrap().palette[15] = Rgb(1);
        assert!(!first.same_colors(&other));
        other = first.clone();
        other.palette.gold = Rgb(1);
        assert!(!first.same_colors(&other));
        // Switching Native on with the same colors is a change the phone must see.
        other = first.clone();
        other.native = true;
        assert!(!first.same_colors(&other));
        // So is switching the mic, which changes no color.
        other = first.clone();
        other.mic = true;
        assert!(!first.same_colors(&other));
    }

    #[test]
    fn read_reports_missing_and_unusable_files_as_none() {
        let home = home();
        assert_eq!(read(&home), None);
        assert_eq!(read(&home.join("absent")), None);
        fs::write(home.join(FILE_NAME), "not json").unwrap();
        assert_eq!(read(&home), None);
        fs::remove_file(home.join(FILE_NAME)).unwrap();
        // A directory of that name is not a document either.
        fs::create_dir(home.join(FILE_NAME)).unwrap();
        assert_eq!(read(&home), None);
        assert!(publish(&home, &snapshot(0x303030), 1).is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn publishing_writes_a_private_file_and_leaves_no_temporary() {
        let home = home();
        let first = snapshot(0x404040);
        assert!(publish(&home, &first, 1_790_000_001).unwrap());
        let read_back = read(&home).unwrap();
        assert_eq!(read_back.updated_at, 1_790_000_001);
        assert!(read_back.same_colors(&first));
        let text = fs::read_to_string(home.join(FILE_NAME)).unwrap();
        assert!(text.ends_with("}\n"), "{text:?}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["v"], 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = home
                .join(FILE_NAME)
                .metadata()
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let names = fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, [FILE_NAME]);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn unchanged_colors_are_not_rewritten_and_changed_ones_are() {
        let home = home();
        let path = home.join(FILE_NAME);
        let first = snapshot(0x505050);
        assert!(publish(&home, &first, 100).unwrap());
        let written = fs::read(&path).unwrap();
        #[cfg(unix)]
        let identity = || {
            use std::os::unix::fs::MetadataExt;
            let metadata = path.metadata().unwrap();
            (metadata.ino(), metadata.mtime(), metadata.mtime_nsec())
        };
        #[cfg(unix)]
        let before = identity();
        // Another process, another moment, the same colors.
        assert!(!publish(&home, &first, 200).unwrap());
        let mut stamped = first.clone();
        stamped.updated_at = 300;
        assert!(!publish(&home, &stamped, 400).unwrap());
        assert_eq!(fs::read(&path).unwrap(), written);
        #[cfg(unix)]
        assert_eq!(identity(), before);

        let mut changed = first.clone();
        changed.palette.text = Rgb(0xeeeeee);
        assert!(publish(&home, &changed, 500).unwrap());
        let now = read(&home).unwrap();
        assert_eq!(now.updated_at, 500);
        assert_eq!(now.palette.text, Rgb(0xeeeeee));
        assert!(!publish(&home, &changed, 600).unwrap());

        // Dropping the terminal colors is a change too.
        let mut unknown = changed.clone();
        unknown.terminal = None;
        assert!(publish(&home, &unknown, 700).unwrap());
        assert_eq!(read(&home).unwrap().terminal, None);
        assert!(!publish(&home, &unknown, 800).unwrap());
        assert_eq!(read(&home).unwrap().updated_at, 700);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn an_invalid_file_is_replaced() {
        let home = home();
        let path = home.join(FILE_NAME);
        for junk in ["", "garbage", "{\"v\":2}"] {
            fs::write(&path, junk).unwrap();
            let published = snapshot(0x606060);
            assert!(publish(&home, &published, 42).unwrap(), "{junk:?}");
            assert_eq!(read(&home).unwrap().updated_at, 42);
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_and_no_temporary() {
        let home = home();
        let first = snapshot(0x707070);
        assert!(publish(&home, &first, 1).unwrap());
        let missing = home.join("not-a-directory");
        assert!(publish(&missing, &snapshot(0x808080), 2).is_err());
        assert!(read(&home).unwrap().same_colors(&first));
        assert!(!missing.exists());
        fs::remove_dir_all(home).unwrap();
    }
}
