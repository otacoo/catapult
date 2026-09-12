// ── Chat attachments (files/images for multimodal models) ───────────────────
//
// The frontend reads nothing itself: paths + optional base64 payloads arrive
// here and are folded into the user message as OpenAI multimodal content:
// a text part plus one `image_url` part per attached image (data URL), and
// text attachments appended as fenced blocks.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use harness::client::ChatMessage;

/// An attachment sent from the Chat UI. Images carry `data_base64` (read via
/// the read command); text files carry `text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// "image" | "text"
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

fn mime_for(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let ext = std::path::Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Fold the typed message and its attachments into a single OpenAI user
/// message: a multimodal parts array when images are present, otherwise a
/// plain string with text attachments appended as fenced blocks.
pub fn build_user_message(message: String, attachments: Option<Vec<Attachment>>) -> ChatMessage {
    let attachments = attachments.unwrap_or_default();
    if attachments.is_empty() {
        return ChatMessage::user(message);
    }

    let mut text_parts: Vec<String> = Vec::new();
    if !message.trim().is_empty() {
        text_parts.push(message);
    }
    let mut image_parts: Vec<Value> = Vec::new();
    for a in attachments {
        match a.kind.as_str() {
            "image" => {
                if let Some(b64) = a.data_base64 {
                    let mime = mime_for(&a.name);
                    text_parts.push(format!("[image: {}]", a.name));
                    image_parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", mime, b64) }
                    }));
                }
            }
            _ => {
                if let Some(text) = a.text {
                    const TEXT_CAP: usize = 50_000;
                    let shown = if text.chars().count() > TEXT_CAP {
                        let mut t: String = text.chars().take(TEXT_CAP).collect();
                        t.push_str("\n[truncated]");
                        t
                    } else {
                        text
                    };
                    text_parts.push(format!("Attached file {}:\n```\n{}\n```", a.name, shown));
                }
            }
        }
    }

    let combined = if text_parts.is_empty() {
        "(no text)".to_string()
    } else {
        text_parts.join("\n\n")
    };

    if image_parts.is_empty() {
        return ChatMessage::user(combined);
    }
    let mut parts = vec![json!({ "type": "text", "text": combined })];
    parts.extend(image_parts);
    ChatMessage {
        role: "user".into(),
        content: Some(json!(parts)),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Read an attachment for the Chat UI: images are returned as base64
/// (frontend builds the data URL), text files as UTF-8 text.
#[derive(Debug, Serialize)]
pub struct AttachmentRead {
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "bmp"];

#[tauri::command]
pub async fn harness_read_attachment(path: String) -> Result<AttachmentRead, String> {
    let p = std::path::Path::new(&path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let is_image = IMAGE_EXTS.contains(&ext.as_str());
    let bytes = std::fs::read(p).map_err(|e| format!("Cannot read {}: {e}", p.display()))?;
    if is_image {
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(AttachmentRead { kind: "image".into(), name, data_base64: Some(b64), text: None })
    } else {
        // Text attachments: refuse files > 5 MB and anything that isn't UTF-8.
        if bytes.len() > 5_000_000 {
            return Err("File is too large to attach as text (>5 MB)".to_string());
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| "Not a text file (binary content)".to_string())?;
        Ok(AttachmentRead { kind: "text".into(), name, data_base64: None, text: Some(text) })
    }
}
