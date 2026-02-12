use sha2::{Digest, Sha256};

use crate::models::{MessageRole, NormalizedChatRequest, NormalizedMessage};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestFingerprint(String);

impl RequestFingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn fingerprint_for(request: &NormalizedChatRequest) -> RequestFingerprint {
    let canonical = canonical_payload(request);
    let digest = Sha256::digest(canonical.as_bytes());
    RequestFingerprint(to_hex(digest.as_ref()))
}

fn canonical_payload(request: &NormalizedChatRequest) -> String {
    let mut payload = String::new();
    payload.push_str(&request.model);
    payload.push('|');
    payload.push_str(
        &request
            .generation
            .max_tokens
            .unwrap_or_default()
            .to_string(),
    );
    payload.push('|');
    payload.push_str(&opt_float(request.generation.temperature));
    payload.push('|');
    payload.push_str(&opt_float(request.generation.top_p));

    for message in &request.messages {
        append_message(&mut payload, message);
    }

    payload
}

fn append_message(buffer: &mut String, message: &NormalizedMessage) {
    buffer.push('|');
    buffer.push_str(match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    });
    buffer.push(':');
    buffer.push_str(&message.content);
}

fn opt_float(value: Option<f32>) -> String {
    value
        .map(|number| format!("{number:.4}"))
        .unwrap_or_else(|| "none".to_owned())
}

fn to_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(nibble_to_hex(byte >> 4));
        encoded.push(nibble_to_hex(byte & 0x0f));
    }
    encoded
}

fn nibble_to_hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'a' + (value - 10)) as char,
        _ => '0',
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{GenerationParams, MessageRole, NormalizedChatRequest, NormalizedMessage};

    use super::fingerprint_for;

    #[test]
    fn fingerprint_is_stable_for_same_request_shape() {
        let request = NormalizedChatRequest {
            request_id: "req_1".to_owned(),
            user_id: "user_a".to_owned(),
            model: "gpt-test".to_owned(),
            messages: vec![NormalizedMessage {
                role: MessageRole::User,
                content: "hello".to_owned(),
            }],
            generation: GenerationParams {
                max_tokens: Some(100),
                temperature: Some(0.7),
                top_p: Some(1.0),
            },
            stream: false,
        };

        let left = fingerprint_for(&request);
        let right = fingerprint_for(&request);
        assert_eq!(left, right);
    }
}
