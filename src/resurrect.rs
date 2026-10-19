//! Normalize tmux 3.8 JSON layouts to the legacy format understood by resurrect.
use serde_json::Value;

pub fn normalize_snapshot(input: &str) -> Result<String, String> {
    let mut output = String::with_capacity(input.len());
    for line in input.lines() {
        let mut fields: Vec<_> = line.split('\t').map(str::to_owned).collect();
        if fields.first().is_some_and(|kind| kind == "window") {
            let layout = fields.get_mut(6).ok_or("invalid resurrect window record")?;
            *layout = normalize_layout(layout)?;
        }
        output.push_str(&fields.join("\t"));
        output.push('\n');
    }
    Ok(output)
}

pub fn normalize_layout(input: &str) -> Result<String, String> {
    if !input.trim_start().starts_with('{') {
        return Ok(input.to_owned());
    }
    let value: Value = serde_json::from_str(input)
        .map_err(|error| format!("invalid resurrect JSON layout: {error}"))?;
    if value.get("V").and_then(Value::as_u64) != Some(2) {
        return Err("unsupported resurrect JSON layout version".into());
    }
    let layout = render(value.get("L").ok_or("missing resurrect layout root")?, 0)?;
    let checksum = layout.bytes().fold(0u16, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u16::from(byte))
    });
    Ok(format!("{checksum:04x},{layout}"))
}

fn render(node: &Value, depth: usize) -> Result<String, String> {
    if depth > 128 {
        return Err("resurrect layout nesting limit exceeded".into());
    }
    let number = |key| {
        node.get(key)
            .and_then(Value::as_u64)
            .filter(|value| *value <= u64::from(u32::MAX))
            .ok_or_else(|| format!("invalid resurrect layout coordinate: {key}"))
    };
    let (width, height, x, y) = (number("w")?, number("h")?, number("x")?, number("y")?);
    if width == 0 || height == 0 {
        return Err("invalid resurrect layout size".into());
    }
    let prefix = format!("{width}x{height},{x},{y}");
    match node.get("t").and_then(Value::as_str) {
        Some("p") => {
            // I is the global pane ID; i is only the window-local pane index.
            let pane = node
                .get("I")
                .and_then(Value::as_str)
                .and_then(|id| id.strip_prefix('%'))
                .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|id| id.parse::<u32>().ok())
                .ok_or("invalid resurrect layout pane ID")?;
            Ok(format!("{prefix},{pane}"))
        }
        Some(kind @ ("h" | "v")) => {
            let children = node
                .get("c")
                .and_then(Value::as_array)
                .filter(|children| children.len() >= 2)
                .ok_or("invalid resurrect layout children")?;
            let children = children
                .iter()
                .map(|child| render(child, depth + 1))
                .collect::<Result<Vec<_>, _>>()?
                .join(",");
            let (open, close) = if kind == "h" { ('{', '}') } else { ('[', ']') };
            Ok(format!("{prefix}{open}{children}{close}"))
        }
        _ => Err("invalid resurrect layout node type".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_snapshots_are_unchanged() {
        let input = "pane\ta\t1\t0\t:-\t0\ttitle\t:/tmp\t1\tsh\t:\nwindow\ta\t1\t:name\t0\t:-\tbb62,80x24,0,0,42\toff\n";
        assert_eq!(normalize_snapshot(input).unwrap(), input);
    }

    #[test]
    fn json_uses_global_pane_ids_and_legacy_checksum() {
        let json = r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"i":1,"I":"%42","a":true}}"#;
        let normalized = normalize_layout(json).unwrap();
        assert!(normalized.ends_with(",80x24,0,0,42"));
        // Independently compute the checksum using the legacy tmux recurrence.
        let mut sum = 0u32;
        for byte in b"80x24,0,0,42" {
            sum = ((sum >> 1) + ((sum & 1) << 15) + u32::from(*byte)) & 0xffff;
        }
        assert_eq!(normalized, format!("{sum:04x},80x24,0,0,42"));
    }

    #[test]
    fn nested_json_preserves_horizontal_and_vertical_geometry() {
        let json = serde_json::json!({"V": 2, "L": {
            "t": "h", "w": 80, "h": 24, "x": 0, "y": 0, "c": [
                {"t": "p", "w": 20, "h": 24, "x": 0, "y": 0, "i": 0, "I": "%1"},
                {"t": "v", "w": 59, "h": 24, "x": 21, "y": 0, "c": [
                    {"t": "p", "w": 59, "h": 12, "x": 21, "y": 0, "i": 1, "I": "%42"},
                    {"t": "p", "w": 59, "h": 11, "x": 21, "y": 13, "i": 2, "I": "%99"}
                ]}
            ]
        }});
        let normalized = normalize_layout(&json.to_string()).unwrap();
        assert_eq!(
            normalized.split_once(',').unwrap().1,
            "80x24,0,0{20x24,0,0,1,59x24,21,0[59x12,21,0,42,59x11,21,13,99]}"
        );
    }

    #[test]
    fn invalid_json_is_rejected_instead_of_saving_a_broken_layout() {
        for input in [r#"{"V":3}"#, r#"{"V":2,"L":{}}"#, "{broken"] {
            assert!(normalize_layout(input).is_err(), "{input}");
        }
    }
}
