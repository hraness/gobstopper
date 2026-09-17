use serde_json::Value;

fn text_block(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("text" | "input_text" | "output_text")
    )
}

/// Concatenate the text content of a string or an array of text blocks.
pub fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| text_block(b))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

pub fn text_bytes(value: &Value) -> u64 {
    match value {
        Value::String(s) => s.len() as u64,
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| text_block(b))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .fold(0u64, |total, text| total.saturating_add(text.len() as u64)),
        _ => 0,
    }
}

pub fn eligible_bytes(value: &Value) -> u64 {
    let bytes = text_bytes(value);
    if bytes > 256 {
        bytes
    } else {
        0
    }
}

pub fn elide(value: &mut Value, stub: String) -> u64 {
    let old = eligible_bytes(value);
    if old == 0 || stub.len() as u64 >= old {
        return 0;
    }
    let mut candidate = value.clone();
    match &mut candidate {
        Value::String(s) => *s = stub,
        Value::Array(blocks) => {
            let mut replacement = Some(stub);
            for block in blocks.iter_mut().filter(|b| text_block(b)) {
                if block.get("text").is_some_and(Value::is_string) {
                    block["text"] = Value::String(replacement.take().unwrap_or_default());
                }
            }
        }
        _ => return 0,
    }
    if candidate.to_string().len() >= value.to_string().len() {
        return 0;
    }
    *value = candidate;
    old
}
