//! Lenient JSON repair for tool-call arguments.
//!
//! Small local models frequently emit malformed tool-call JSON: markdown
//! fences, trailing commas, unbalanced braces or truncated strings. The
//! orchestrator retries calls, but each retry costs a full generation — so
//! arguments are repaired *before* parsing. Repair is best-effort and
//! conservative: if the result still doesn't parse as JSON, the caller must
//! fail loud (never silently guess intent).

/// Attempt to repair a fragment of JSON into something `serde_json` can parse.
/// Returns the repaired text; callers must still validate by parsing.
pub fn repair_json(input: &str) -> String {
    let mut s = strip_code_fences(input.trim());
    s = strip_trailing_commas(&s);
    if serde_json::from_str::<serde_json::Value>(&s).is_ok() {
        return s;
    }
    // Scan tracking string/escape state; remember unclosed container types in
    // order so we can close them in reverse.
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' | '[' => {
                stack.push(ch);
                out.push(ch);
            }
            '}' | ']' => {
                stack.pop();
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    if in_string {
        if escaped {
            out.pop(); // dangling backslash would escape our synthetic quote
        }
        out.push('"');
    }
    // Close in reverse-open order (correct JSON nesting), dropping matches
    // that would clash with what remains unbalanced in the text itself.
    for open in stack.iter().rev() {
        out.push(match open {
            '{' => '}',
            '[' => ']',
            _ => continue,
        });
    }
    let s = strip_trailing_commas(&out);
    s
}

/// Remove ```json fences and stray ``` markers.
fn strip_code_fences(input: &str) -> String {
    let mut s = input.trim().to_string();
    if s.starts_with("```") {
        // Drop the first line (```json / ```) and the closing fence.
        if let Some(rest) = s.split_once('\n') {
            s = rest.1.to_string();
        }
        if let Some(idx) = s.rfind("```") {
            s.truncate(idx);
        }
    }
    s.trim().to_string()
}

/// Remove commas immediately preceding `}` or `]` (outside strings handled by
/// simplicity: trailing commas inside string literals are rare in malformed
/// fragments, and the scanner below re-validates anyway).
fn strip_trailing_commas(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &byte) in bytes.iter().enumerate() {
        let ch = byte as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            ',' => {
                // Peek past whitespace for } or ]
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                    continue; // drop the comma
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parses(s: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(s).is_ok()
    }

    #[test]
    fn valid_json_passes_through() {
        assert_eq!(repair_json(r#"{"a":1}"#), r#"{"a":1}"#);
    }

    #[test]
    fn strips_code_fences() {
        let s = "```json\n{\"a\": 1}\n```";
        assert!(parses(&repair_json(s)));
    }

    #[test]
    fn strips_trailing_commas() {
        let s = r#"{"a": 1, "b": [1, 2,],}"#;
        let out = repair_json(s);
        assert!(parses(&out));
    }

    #[test]
    fn closes_unbalanced_brace() {
        let s = r#"{"a": {"b": 1}"#;
        assert!(parses(&repair_json(s)));
    }

    #[test]
    fn closes_unbalanced_string() {
        let s = r#"{"a": "unterminated"#;
        let out = repair_json(s);
        assert!(parses(&out), "repaired: {out}");
    }

    #[test]
    fn closes_unbalanced_string_with_dangling_escape() {
        let s = r#"{"a": "ends with backslash\"#;
        let out = repair_json(s);
        assert!(parses(&out), "repaired: {out}");
    }

    #[test]
    fn repairs_fenced_truncated_tool_call() {
        let s = "```json\n{\"name\": \"write_file\", \"arguments\": {\"path\": \"a.txt\", \"content\": \"hi";
        let out = repair_json(s);
        assert!(parses(&out), "repaired: {out}");
    }

    #[test]
    fn empty_string_is_untouched() {
        assert_eq!(repair_json("   "), "");
    }
}
