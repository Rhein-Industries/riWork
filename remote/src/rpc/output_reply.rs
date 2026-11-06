//! Validated terminal output replies, retaining the parsed text allocation.
use super::{
    Fault, Output, cli_fault, hash_shaped,
    output_delta::{self, Screens},
    screen_fields, scrollback_fields, sgr_only,
};
use serde_json::{Value, json};

pub(super) fn build(
    screens: &Screens,
    device: &str,
    p: &Output,
    lines: u32,
    styled: bool,
    mut v: Value,
) -> std::result::Result<Value, Fault> {
    // Move only the output value; validation below still decides whether it is usable.
    let output = v.get_mut("output").map(Value::take);
    let hash = v
        .get("hash")
        .and_then(Value::as_str)
        .filter(|h| hash_shaped(h));
    if v.get("unchanged") == Some(&Value::Bool(true)) {
        // Only ever the answer to a hash this request sent.
        return match (&p.if_changed, hash) {
            (Some(asked), Some(hash)) if asked == hash => {
                let mut result = json!({"shell_id":p.shell_id,"unchanged":true,"hash":hash});
                // Additive: the phone tracks the scrollback's growth.
                if let Some((size, alternate)) = scrollback_fields(&v) {
                    result["history_size"] = json!(size);
                    result["alternate"] = json!(alternate);
                }
                Ok(result)
            }
            _ => Err(cli_fault(
                "CLI reported unchanged output that was not asked",
            )),
        };
    }
    let Some(Value::String(text)) = output else {
        return Err(cli_fault("missing CLI output"));
    };
    if styled && !sgr_only(&text) {
        return Err(cli_fault(
            "CLI returned escape sequences other than SGR for styled output",
        ));
    }
    let mut result = json!({"shell_id":p.shell_id});
    // Additive: the last `rows` lines of `output` are the visible
    // screen. Absent when the desktop could not read the pane.
    if let (Some(screen), Some(map)) = (screen_fields(&v, &text), result.as_object_mut())
        && let Some(fields) = screen.as_object()
    {
        map.extend(fields.clone());
    }
    // Additive: the scrollback lines above the screen, and whether
    // a full-screen program is on the alternate screen.
    if let Some((size, alternate)) = scrollback_fields(&v) {
        result["history_size"] = json!(size);
        result["alternate"] = json!(alternate);
    }
    // Additive: names this exact answer, for `if_changed`.
    if let Some(hash) = hash {
        result["hash"] = json!(hash);
    }
    // Changed rows only, when asked for and this device holds the screen its
    // hash names; the full screen is remembered for the next one either way.
    if p.delta == Some(true)
        && let Some(hash) = hash
    {
        let asked = output_delta::Asked {
            device,
            shell: &p.shell_id,
            lines,
            styled,
        };
        let history = scrollback_fields(&v).map(|(size, _)| size);
        if let Some(base) = &p.if_changed
            && let Some(delta) = screens.delta(&asked, base, history, &text)
            && let Some(map) = result.as_object_mut()
        {
            map.insert("delta".into(), delta);
            screens.remember(&asked, hash, history, text);
            return Ok(result);
        }
        // A full reply and the cache each need their own output.
        screens.remember(&asked, hash, history, text.clone());
    }
    // The parsed allocation now belongs to the full response.
    result["output"] = Value::String(text);
    Ok(result)
}

#[cfg(test)]
#[path = "output_reply_tests.rs"]
mod tests;
