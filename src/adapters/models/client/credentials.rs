use serde_json::Value;

pub(super) fn contains_credential(value: &Value, key: &str) -> bool {
    match value {
        Value::String(text) => contains_text(text, key),
        Value::Array(items) => items.iter().any(|item| contains_credential(item, key)),
        Value::Object(fields) => fields
            .iter()
            .any(|(name, value)| contains_text(name, key) || contains_credential(value, key)),
        _ => false,
    }
}

pub(super) fn contains_text(text: &str, key: &str) -> bool {
    text.contains(key)
        || serde_json::from_str::<Value>(text).is_ok_and(|value| contains_credential(&value, key))
}

fn decoded_string(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let length = if bytes.get(index + 1) == Some(&b'u') {
                if bytes.get(index + 6..index + 8) == Some(b"\\u") {
                    12
                } else {
                    6
                }
            } else {
                2
            };
            let mut consumed = false;
            for length in [length, 6, 2] {
                if let Some(escape) = bytes.get(index..index + length) {
                    let mut token = vec![b'"'];
                    token.extend(escape);
                    token.push(b'"');
                    if let Ok(value) = serde_json::from_slice::<String>(&token) {
                        decoded.extend(value.as_bytes());
                        index += length;
                        consumed = true;
                        break;
                    }
                }
            }
            if consumed {
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

pub(super) fn scrub_capture(bytes: &mut [u8], key: &[u8]) {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        index += 1;
        let start = index;
        let mut closed = false;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' => index = (index + 2).min(bytes.len()),
                b'"' => {
                    closed = true;
                    break;
                }
                _ => index += 1,
            }
        }
        let mut decoded = decoded_string(&bytes[start..index]);
        let original = decoded.clone();
        scrub_capture(&mut decoded, key);
        if decoded != original {
            bytes[start..index].fill(b'*');
        }
        index += usize::from(closed);
    }
    let mut index = 0;
    while index + key.len() <= bytes.len() {
        if bytes[index..].starts_with(key) {
            bytes[index..index + key.len()].fill(b'*');
            index += key.len();
        } else {
            index += 1;
        }
    }
}
