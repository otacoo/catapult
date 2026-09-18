// ── Chat attachments (files/images for multimodal models) ───────────────────
//
// Folded into the user message as OpenAI multimodal content (data-URL images + fenced text).

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use harness::client::ChatMessage;

/// Chat UI attachment: images carry `data_base64`, text files carry `text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// "image" | "text"
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Absolute source path; parent dirs become readable for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Where each attachment lives; lets the model `read_file` attachments with
/// either form (relative resolves from the project root; absolute covers
/// subagents rooted elsewhere, e.g. worktrees).
fn locations_block(
    attachments: &[Attachment],
    project_root: Option<&std::path::Path>,
) -> Option<String> {
    let lines: Vec<String> = attachments
        .iter()
        .filter_map(|a| {
            let p = a.path.as_deref()?.trim();
            if p.is_empty() {
                return None;
            }
            let abs = std::path::Path::new(p);
            match project_root.and_then(|r| abs.strip_prefix(r).ok()) {
                Some(rel) => Some(format!(
                    "- {}: {} (absolute: {})",
                    a.name,
                    rel.display(),
                    abs.display()
                )),
                None => Some(format!("- {}: {}", a.name, abs.display())),
            }
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Attachment locations (relative paths resolve from the project root; absolute paths work everywhere):\n{}",
        lines.join("\n")
    ))
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

/// Fold message + attachments into one OpenAI user message (parts array with
/// images, else string with fenced text blocks).
pub fn build_user_message(
    message: String,
    attachments: Option<Vec<Attachment>>,
    project_root: Option<&std::path::Path>,
) -> ChatMessage {
    let attachments = attachments.unwrap_or_default();
    if attachments.is_empty() {
        return ChatMessage::user(message);
    }

    let mut text_parts: Vec<String> = Vec::new();
    if !message.trim().is_empty() {
        text_parts.push(message);
    }
    let mut image_parts: Vec<Value> = Vec::new();
    for a in &attachments {
        match a.kind.as_str() {
            "image" => {
                if let Some(b64) = a.data_base64.as_deref() {
                    let mime = mime_for(&a.name);
                    text_parts.push(format!(
                        "[image: {} — already attached and visible; no need to read it]",
                        a.name
                    ));
                    image_parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{};base64,{}", mime, b64) }
                    }));
                }
            }
            _ => {
                if let Some(text) = a.text.as_deref() {
                    const TEXT_CAP: usize = 50_000;
                    let shown = if text.chars().count() > TEXT_CAP {
                        let mut t: String = text.chars().take(TEXT_CAP).collect();
                        t.push_str("\n[truncated]");
                        t
                    } else {
                        text.to_string()
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
    let combined = match locations_block(&attachments, project_root) {
        Some(block) => format!("{combined}\n\n{block}"),
        None => combined,
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

/// Image parts for subagent delegation (empty when no images).
pub fn image_parts(attachments: &Option<Vec<Attachment>>) -> Vec<Value> {
    let mut out = Vec::new();
    for a in attachments.as_ref().map(|v| v.as_slice()).unwrap_or(&[]) {
        if a.kind == "image" {
            if let Some(b64) = &a.data_base64 {
                out.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{};base64,{}", mime_for(&a.name), b64) }
                }));
            }
        }
    }
    out
}

/// Fenced text blocks for contexts the model can't re-read (e.g. subagents).
pub fn fenced_texts(attachments: &Option<Vec<Attachment>>) -> Vec<String> {
    let mut out = Vec::new();
    for a in attachments.as_ref().map(|v| v.as_slice()).unwrap_or(&[]) {
        if a.kind == "image" {
            continue;
        }
        if let Some(text) = &a.text {
            const TEXT_CAP: usize = 50_000;
            let shown = if text.chars().count() > TEXT_CAP {
                let mut t: String = text.chars().take(TEXT_CAP).collect();
                t.push_str("\n[truncated]");
                t
            } else {
                text.clone()
            };
            let source = match a.path.as_deref().filter(|p| !p.trim().is_empty()) {
                Some(p) => format!(" (source: {p})"),
                None => String::new(),
            };
            out.push(format!("Attached file {}{source}:\n```\n{}\n```", a.name, shown));
        }
    }
    out
}

/// Strip images for text-only models, leaving a delegation hint per image.
pub fn strip_image_parts(msg: ChatMessage) -> ChatMessage {
    let images: Vec<String> = match &msg.content {
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type")?.as_str()? != "image_url" {
                    return None;
                }
                let url = p.get("image_url")?.get("url")?.as_str().unwrap_or("");
                // Data URLs carry no filename; text markers already name them.
                Some(if url.len() > 60 { "[attached image]" } else { url }.to_string())
            })
            .collect(),
        _ => return msg,
    };
    if images.is_empty() {
        return msg;
    }
    let mut text = match &msg.content {
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text")?.as_str()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    };
    for (i, _) in images.iter().enumerate() {
        text.push_str(&format!(
            "\n\n[image {} omitted — you cannot process images; delegate visual work with spawn_subagent (the worker receives attached images)]",
            i + 1
        ));
    }
    ChatMessage {
        role: msg.role,
        content: Some(Value::String(text)),
        tool_calls: msg.tool_calls,
        tool_call_id: msg.tool_call_id,
    }
}

/// Read attachment: images as base64, text as UTF-8.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn img(name: &str) -> Attachment {
        Attachment { name: name.to_string(), kind: "image".into(), data_base64: Some("QUJD".into()), text: None, path: None }
    }

    #[test]
    fn image_parts_forward_and_strip_roundtrip() {
        let atts = Some(vec![img("pic.png")]);
        let parts = image_parts(&atts);
        assert_eq!(parts.len(), 1);
        let msg = build_user_message("describe this".to_string(), atts, None);
        // Multimodal for capable models…
        assert!(matches!(msg.content, Some(Value::Array(_))));
        // …delegation hint for text-only ones.
        let stripped = strip_image_parts(msg);
        let content = stripped.content.and_then(|c| c.as_str().map(str::to_string)).unwrap();
        assert!(content.contains("describe this"));
        assert!(content.contains("spawn_subagent"));
        assert!(!content.contains("QUJD"));
    }

    #[test]
    fn user_message_lists_attachment_paths() {
        let atts = Some(vec![Attachment {
            name: "shot.jpg".into(),
            kind: "image".into(),
            data_base64: Some("QUJD".into()),
            text: None,
            path: Some("E:\\Downloads\\shot.jpg".into()),
        }]);
        let msg = build_user_message(String::new(), atts, None);
        let text = match &msg.content {
            Some(Value::Array(parts)) => parts
                .iter()
                .find_map(|p| p.get("text").and_then(|t| t.as_str()))
                .unwrap()
                .to_string(),
            _ => panic!("expected multimodal parts"),
        };
        assert!(text.contains("Attachment locations"));
        assert!(text.contains("E:\\Downloads\\shot.jpg"));
    }

    #[test]
    fn user_message_shows_relative_and_absolute_copy_paths() {
        let atts = Some(vec![Attachment {
            name: "data.csv".into(),
            kind: "text".into(),
            data_base64: None,
            text: Some("a,b".into()),
            path: Some("E:\\proj\\.catapult\\attachments\\1\\data.csv".into()),
        }]);
        let msg = build_user_message(String::new(), atts, Some(std::path::Path::new("E:\\proj")));
        let text = match &msg.content {
            Some(Value::String(s)) => s.clone(),
            other => panic!("expected string content, got {other:?}"),
        };
        assert!(text.contains(".catapult\\attachments\\1\\data.csv (absolute:"));
    }

    #[test]
    fn strip_leaves_text_only_messages_alone() {
        let msg = ChatMessage::user("hello".to_string());
        let stripped = strip_image_parts(msg);
        assert_eq!(
            stripped.content.and_then(|c| c.as_str().map(str::to_string)).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn fenced_texts_format_text_attachments() {
        let atts = Some(vec![Attachment {
            name: "log.txt".into(),
            kind: "text".into(),
            data_base64: None,
            text: Some("boom".into()),
            path: None,
        }]);
        let blocks = fenced_texts(&atts);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains("log.txt") && blocks[0].contains("boom"));
        assert!(fenced_texts(&None).is_empty());
    }
}
