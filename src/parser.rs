use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// Represents a single line/entry in the JSONL conversation file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationEntry {
    /// The type of this entry (e.g., "user", "assistant", "file-history-snapshot")
    ///
    /// This field identifies what kind of entry this is in the conversation.
    /// Common types include user messages, assistant responses, and system events.
    #[serde(rename = "type")]
    pub entry_type: String,

    /// Unique identifier for this conversation entry
    ///
    /// Each entry may have its own UUID to uniquely identify it within the conversation.
    /// Not all entry types require a UUID, hence this is optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// UUID of the parent entry in the conversation thread
    ///
    /// This links entries together in a conversation tree, allowing for branching
    /// and threading of messages. If present, it references the UUID of the entry
    /// that this entry is responding to or following from.
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// Session identifier grouping related conversation entries together
    ///
    /// All entries within a single conversation session share the same session ID.
    /// This is used to associate entries across multiple files or to reconstruct
    /// conversation context. If not present in the entry, the filename may be used.
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// ISO 8601 timestamp indicating when this entry was created
    ///
    /// Format is typically "YYYY-MM-DDTHH:MM:SS.sssZ" (e.g., "2025-01-01T00:00:00.000Z").
    /// Used for sorting entries chronologically and determining the latest activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    /// The actual message content as a JSON value
    ///
    /// Contains the text and structured data of the user or assistant message.
    /// Stored as a generic JSON value to accommodate different message formats
    /// and structures without strict schema requirements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,

    /// Current working directory at the time this entry was created
    ///
    /// Stores the filesystem path of the working directory, providing context
    /// about where the conversation or command was executed. Useful for
    /// reproducing environments and understanding file references.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Version string of the Claude Code CLI that created this entry
    ///
    /// Records which version of the tool generated this conversation entry,
    /// helpful for debugging compatibility issues and tracking feature support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Git branch name active when this entry was created
    ///
    /// Captures the current git branch context, allowing conversation entries
    /// to be associated with specific branches in version control. Useful for
    /// tracking which branch work was performed on.
    #[serde(rename = "gitBranch", skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Catch-all field for additional JSON properties not explicitly defined
    ///
    /// Preserves any extra fields in the JSON that aren't part of the explicit schema.
    /// This allows forward compatibility - newer versions can add fields without breaking
    /// older parsers. The flattened serde attribute merges these fields at the same level
    /// as the named fields when serializing/deserializing.
    #[serde(flatten)]
    pub extra: Value,
}

/// Represents a complete conversation session
#[derive(Debug, Clone)]
pub struct ConversationSession {
    /// Unique identifier for this conversation session
    ///
    /// Either extracted from the first entry that contains a sessionId field,
    /// or derived from the filename (without extension) if no entries contain
    /// a session ID. Used to group related conversation entries together.
    pub session_id: String,

    /// All conversation entries in chronological order
    ///
    /// Contains the complete sequence of entries from the JSONL file, including
    /// user messages, assistant responses, and system events like file history
    /// snapshots. Preserves the original order from the file.
    pub entries: Vec<ConversationEntry>,

    /// Path to the JSONL file this session was loaded from
    ///
    /// Stores the filesystem path of the source file, used for tracking the
    /// origin of the conversation data and for potential file operations like
    /// rewriting or updating the session.
    pub file_path: String,
}

impl ConversationSession {
    /// Parse a JSONL file into a ConversationSession
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let file =
            File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut session_id = None;

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.with_context(|| {
                format!("Failed to read line {} in {}", line_num + 1, path.display())
            })?;

            if line.trim().is_empty() {
                continue;
            }

            let entry: ConversationEntry = serde_json::from_str(&line).with_context(|| {
                format!(
                    "Failed to parse JSON at line {} in {}",
                    line_num + 1,
                    path.display()
                )
            })?;

            // Extract session ID from first entry that has one
            if session_id.is_none() {
                if let Some(ref sid) = entry.session_id {
                    session_id = Some(sid.clone());
                }
            }

            entries.push(entry);
        }

        // If no session ID in entries, use filename (without extension) as session ID
        let session_id = session_id
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .with_context(|| {
                format!(
                    "No session ID found in file or filename: {}",
                    path.display()
                )
            })?;

        Ok(ConversationSession {
            session_id,
            entries,
            file_path: path.to_string_lossy().to_string(),
        })
    }

    /// Write the conversation session to a JSONL file
    pub fn write_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let mut file = File::create(path)
            .with_context(|| format!("Failed to create file: {}", path.display()))?;

        for entry in &self.entries {
            let json =
                serde_json::to_string(entry).context("Failed to serialize conversation entry")?;
            writeln!(file, "{json}")
                .with_context(|| format!("Failed to write to file: {}", path.display()))?;
        }

        Ok(())
    }

    /// Get the latest timestamp from the conversation
    pub fn latest_timestamp(&self) -> Option<String> {
        self.entries
            .iter()
            .filter_map(|e| e.timestamp.clone())
            .max()
    }

    /// Get the number of messages (user + assistant) in the conversation
    pub fn message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.entry_type == "user" || e.entry_type == "assistant")
            .count()
    }

    /// Get the project name from the first entry's `cwd` path
    ///
    /// This function handles both Unix and Windows paths to support
    /// cross-platform sync (e.g., pulling Windows paths on Mac/Linux).
    pub fn project_name(&self) -> Option<&str> {
        self.entries
            .iter()
            .find_map(|e| e.cwd.as_ref())
            .and_then(|cwd| {
                // Split by both / and \ to handle cross-platform paths
                // Take the last non-empty component
                cwd.split(&['/', '\\'])
                    .filter(|s| !s.is_empty())
                    .last()
            })
    }

    /// Get the full cwd path from the first entry that has it
    pub fn cwd(&self) -> Option<&str> {
        self.entries
            .iter()
            .find_map(|e| e.cwd.as_deref())
    }

    /// Get the session title
    ///
    /// Priority: custom-title entry (from Claude Code rename) > first real user message.
    /// Skips system-generated content like `<ide_opened_file>` tags and "Warmup" messages.
    pub fn title(&self) -> Option<String> {
        // Priority 1: custom-title entry (set by Claude Code rename)
        // Use the last one in case of multiple renames
        if let Some(custom) = self
            .entries
            .iter()
            .rev()
            .find(|e| e.entry_type == "custom-title")
            .and_then(|e| e.extra.get("customTitle"))
            .and_then(|v| v.as_str())
        {
            if !custom.is_empty() {
                return Some(custom.to_string());
            }
        }

        // Priority 2: first real user message
        for entry in self.entries.iter().filter(|e| e.entry_type == "user") {
            if let Some(msg) = entry.message.as_ref() {
                if let Some(content) = msg.get("content") {
                    // content can be a string or an array of content blocks
                    if let Some(s) = content.as_str() {
                        // Skip if it's system content, continue to next user entry
                        if !Self::is_system_content(s) {
                            return Some(s.to_string());
                        }
                    } else if let Some(arr) = content.as_array() {
                        // Handle structured content like [{"type": "text", "text": "..."}]
                        // Find the first text that is not system-generated content
                        if let Some(text) = arr
                            .iter()
                            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                            .find(|text| !Self::is_system_content(text))
                        {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    /// Check if the content is system-generated (should be skipped for title)
    fn is_system_content(text: &str) -> bool {
        let trimmed = text.trim();
        // Skip IDE file open notifications
        trimmed.starts_with("<ide_opened_file>")
            || trimmed.starts_with("<ide_selection>")
            // Skip system injected tags
            || trimmed.starts_with("<task-notification>")
            || trimmed.starts_with("<local-command-caveat>")
            || trimmed.starts_with("<command-name>")
            || trimmed.starts_with("<local-command-stdout>")
            // Skip warmup/system messages
            || trimmed.to_lowercase() == "warmup"
            // Skip empty content
            || trimmed.is_empty()
    }

    /// Extract text content from a message Value, filtering out system-generated content
    pub fn extract_user_text(message: &Value) -> Option<String> {
        let content = message.get("content")?;
        if let Some(s) = content.as_str() {
            if Self::is_system_content(s) {
                return None;
            }
            return Some(s.to_string());
        }
        if let Some(arr) = content.as_array() {
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .filter(|text| !Self::is_system_content(text))
                .collect();
            if texts.is_empty() {
                return None;
            }
            return Some(texts.join("\n"));
        }
        None
    }

    /// Get the first timestamp from the conversation (creation time)
    pub fn first_timestamp(&self) -> Option<String> {
        self.entries.iter().filter_map(|e| e.timestamp.clone()).next()
    }

    /// Format a single content block for display.
    /// Simplifies tool_use, tool_result, image blocks into tags.
    pub fn format_content_block(block: &Value) -> Option<String> {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match block_type {
            "text" => {
                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.trim().is_empty() {
                    return None;
                }
                Some(simplify_text_content(text))
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let hint = block
                    .get("input")
                    .and_then(|inp| inp.get("file_path"))
                    .and_then(|fp| fp.as_str())
                    .and_then(|fp| {
                        fp.split(&['/', '\\'])
                            .rfind(|s| !s.is_empty())
                    });
                if let Some(file) = hint {
                    Some(format!("[Tool: {} -> {}]", name, file))
                } else {
                    Some(format!("[Tool: {}]", name))
                }
            }
            "tool_result" => {
                let content_str = block
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                if is_user_interaction_result(content_str) {
                    Some(format_user_interaction(content_str))
                } else {
                    None
                }
            }
            "image" => Some("[Image]".to_string()),
            _ => None,
        }
    }

    /// Check if a user entry is a system-generated tool result (not user interaction).
    /// Returns false if any block contains a user interaction response.
    pub fn is_tool_result_entry(entry: &ConversationEntry) -> bool {
        if entry.entry_type != "user" {
            return false;
        }
        if let Some(msg) = &entry.message {
            if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                if arr.is_empty() {
                    return false;
                }
                // All blocks must be tool_result AND none must be user interaction
                return arr.iter().all(|block| {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        return false;
                    }
                    let content = block
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    !is_user_interaction_result(content)
                });
            }
        }
        false
    }

    /// Extract and format message content for display.
    /// For user messages, filters system content and tool results.
    /// For assistant messages, simplifies tool_use/image/code blocks.
    pub fn extract_display_content(message: &Value, is_user: bool) -> Option<String> {
        let content = message.get("content")?;

        // Case 1: content is a plain string
        if let Some(s) = content.as_str() {
            if is_user && Self::is_system_content(s) {
                return None;
            }
            return Some(simplify_text_content(s));
        }

        // Case 2: content is an array of blocks
        if let Some(arr) = content.as_array() {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|block| {
                    // Filter system content on raw text before formatting
                    if is_user {
                        if let Some(raw) = block.get("text").and_then(|t| t.as_str()) {
                            if Self::is_system_content(raw) {
                                return None;
                            }
                        }
                    }
                    Self::format_content_block(block)
                })
                .collect();

            if parts.is_empty() {
                return None;
            }

            return Some(parts.join("\n"));
        }

        None
    }

    /// Calculate a simple hash of the conversation content
    pub fn content_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        for entry in &self.entries {
            if let Ok(json) = serde_json::to_string(entry) {
                json.hash(&mut hasher);
            }
        }
        format!("{:x}", hasher.finish())
    }
}

/// Check if a tool_result content is a user interaction response.
fn is_user_interaction_result(content: &str) -> bool {
    content.starts_with("User has answered")
        || content.starts_with("User has approved")
        || content.starts_with("The user doesn't want to proceed")
}

/// Format a user interaction tool_result into a concise display tag.
fn format_user_interaction(content: &str) -> String {
    if content.starts_with("User has answered") {
        // Format: 'User has answered your questions: "question"="answer". ...'
        // Extract the question="answer" part
        if let Some(qa) = content.strip_prefix("User has answered your questions: ") {
            // Take up to the first ". " delimiter
            let text = if let Some(pos) = qa.find(". ") {
                &qa[..pos]
            } else {
                qa
            };
            // Unicode-safe truncation
            let chars: Vec<char> = text.chars().collect();
            let display = if chars.len() > 150 {
                let s: String = chars[..150].iter().collect();
                format!("{}...", s)
            } else {
                text.to_string()
            };
            return format!("[User answered: {}]", display);
        }
        "[User answered]".to_string()
    } else if content.starts_with("User has approved") {
        "[User approved plan]".to_string()
    } else if content.starts_with("The user doesn't want to proceed") {
        // Extract: "the user said:\n<actual feedback>"
        if let Some(pos) = content.find("the user said:\n") {
            let feedback = &content[pos + "the user said:\n".len()..];
            let chars: Vec<char> = feedback.chars().collect();
            let truncated = if chars.len() > 100 {
                let s: String = chars[..100].iter().collect();
                format!("{}...", s)
            } else {
                feedback.to_string()
            };
            return format!("[User rejected: {}]", truncated.trim());
        }
        "[User rejected]".to_string()
    } else {
        format!("[User response: {}]", &content[..content.len().min(80)])
    }
}

/// Simplify text content for display:
/// - Replace fenced code blocks (```...```) with [Code] tag
/// - Truncate text exceeding 500 characters
fn simplify_text_content(text: &str) -> String {
    simplify_text_content_with_limit(text, 500)
}

fn simplify_text_content_with_limit(text: &str, max_chars: usize) -> String {
    let mut result = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !in_code_block {
                in_code_block = true;
                let lang = line.trim_start().trim_start_matches('`').trim();
                if lang.is_empty() {
                    result.push_str("[Code]");
                } else {
                    result.push_str(&format!("[Code: {}]", lang));
                }
                result.push('\n');
            } else {
                in_code_block = false;
            }
        } else if !in_code_block {
            result.push_str(line);
            result.push('\n');
        }
    }

    let result = result.trim_end().to_string();

    let chars: Vec<char> = result.chars().collect();
    if chars.len() > max_chars {
        let truncated: String = chars[..max_chars].iter().collect();
        format!("{}...[truncated]", truncated.trim_end())
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_conversation_entry() {
        let json =
            r#"{"type":"user","uuid":"123","sessionId":"abc","timestamp":"2025-01-01T00:00:00Z"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, "user");
        assert_eq!(entry.uuid.unwrap(), "123");
    }

    #[test]
    fn test_read_write_session() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(temp_file, r#"{{"type":"assistant","sessionId":"test-123","uuid":"2","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        let session = ConversationSession::from_file(temp_file.path()).unwrap();
        assert_eq!(session.session_id, "test-123");
        assert_eq!(session.entries.len(), 2);
        assert_eq!(session.message_count(), 2);

        // Test write
        let output_temp = NamedTempFile::new().unwrap();
        session.write_to_file(output_temp.path()).unwrap();

        let reloaded = ConversationSession::from_file(output_temp.path()).unwrap();
        assert_eq!(reloaded.session_id, session.session_id);
        assert_eq!(reloaded.entries.len(), session.entries.len());
    }

    #[test]
    fn test_session_id_from_filename() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir
            .path()
            .join("248a0cdf-1466-48a7-b3d0-00f9e8e6e4ee.jsonl");

        // Create file with entries that don't have sessionId field
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"abc","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"def","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        // Parse should succeed using filename as session ID
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "248a0cdf-1466-48a7-b3d0-00f9e8e6e4ee");
        assert_eq!(session.entries.len(), 2);
    }

    #[test]
    fn test_session_id_from_entry_preferred() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("filename-uuid.jsonl");

        // Create file with sessionId in entries
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"entry-uuid","uuid":"1","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();

        // Should prefer sessionId from entry over filename
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "entry-uuid");
    }

    #[test]
    fn test_mixed_entries_with_and_without_session_id() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let session_file = temp_dir.path().join("test-session.jsonl");

        // Create file with mix of entries
        let mut file = File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"file-history-snapshot","messageId":"abc","timestamp":"2025-01-01T00:00:00Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"test-123","uuid":"1","timestamp":"2025-01-01T00:01:00Z"}}"#).unwrap();

        // Should use sessionId from the entry that has it
        let session = ConversationSession::from_file(&session_file).unwrap();
        assert_eq!(session.session_id, "test-123");
        assert_eq!(session.entries.len(), 2);
    }

    #[test]
    fn test_project_name_from_cwd() {
        let json = r#"{"type":"user","uuid":"1","cwd":"/Users/abc/my-cool-project"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![entry],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.project_name(), Some("my-cool-project"));
    }

    #[test]
    fn test_project_name_no_cwd() {
        let json = r#"{"type":"user","uuid":"1"}"#;
        let entry: ConversationEntry = serde_json::from_str(json).unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![entry],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.project_name(), None);
    }

    #[test]
    fn test_title_prefers_custom_title() {
        let user_entry: ConversationEntry = serde_json::from_str(
            r#"{"type":"user","uuid":"1","message":{"content":"Hello world"}}"#,
        )
        .unwrap();
        let custom_title_entry: ConversationEntry = serde_json::from_str(
            r#"{"type":"custom-title","customTitle":"my-custom-title","sessionId":"s1"}"#,
        )
        .unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![user_entry, custom_title_entry],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.title(), Some("my-custom-title".to_string()));
    }

    #[test]
    fn test_title_falls_back_to_user_message() {
        let user_entry: ConversationEntry = serde_json::from_str(
            r#"{"type":"user","uuid":"1","message":{"content":"Hello world"}}"#,
        )
        .unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![user_entry],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.title(), Some("Hello world".to_string()));
    }

    #[test]
    fn test_title_uses_last_custom_title() {
        let user_entry: ConversationEntry = serde_json::from_str(
            r#"{"type":"user","uuid":"1","message":{"content":"Hello"}}"#,
        )
        .unwrap();
        let ct1: ConversationEntry = serde_json::from_str(
            r#"{"type":"custom-title","customTitle":"first-rename","sessionId":"s1"}"#,
        )
        .unwrap();
        let ct2: ConversationEntry = serde_json::from_str(
            r#"{"type":"custom-title","customTitle":"second-rename","sessionId":"s1"}"#,
        )
        .unwrap();
        let session = ConversationSession {
            session_id: "test".to_string(),
            entries: vec![user_entry, ct1, ct2],
            file_path: "test.jsonl".to_string(),
        };
        assert_eq!(session.title(), Some("second-rename".to_string()));
    }

    #[test]
    fn test_simplify_text_replaces_code_blocks() {
        let input = "Here is some code:\n```rust\nfn main() {}\n```\nDone.";
        let result = simplify_text_content(input);
        assert!(result.contains("[Code: rust]"));
        assert!(!result.contains("fn main"));
        assert!(result.contains("Done."));
    }

    #[test]
    fn test_simplify_text_bare_code_block() {
        let input = "Before\n```\nsome code\n```\nAfter";
        let result = simplify_text_content(input);
        assert!(result.contains("[Code]"));
        assert!(!result.contains("some code"));
        assert!(result.contains("Before"));
        assert!(result.contains("After"));
    }

    #[test]
    fn test_simplify_text_truncates_long_content() {
        let long_text = "a".repeat(600);
        let result = simplify_text_content(&long_text);
        assert!(result.contains("...[truncated]"));
        assert!(result.len() < 600);
    }

    #[test]
    fn test_simplify_text_no_truncation_for_short() {
        let short = "Hello world";
        let result = simplify_text_content(short);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_format_tool_use_block_with_file() {
        let block = serde_json::json!({
            "type": "tool_use",
            "name": "Write",
            "id": "toolu_123",
            "input": {"file_path": "/src/main.rs", "content": "fn main() {}"}
        });
        let result = ConversationSession::format_content_block(&block);
        assert_eq!(result, Some("[Tool: Write -> main.rs]".to_string()));
    }

    #[test]
    fn test_format_tool_use_block_without_file() {
        let block = serde_json::json!({
            "type": "tool_use",
            "name": "Bash",
            "id": "toolu_123",
            "input": {"command": "ls -la"}
        });
        let result = ConversationSession::format_content_block(&block);
        assert_eq!(result, Some("[Tool: Bash]".to_string()));
    }

    #[test]
    fn test_format_image_block() {
        let block = serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": "image/png", "data": "abc"}
        });
        let result = ConversationSession::format_content_block(&block);
        assert_eq!(result, Some("[Image]".to_string()));
    }

    #[test]
    fn test_format_tool_result_returns_none() {
        let block = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "toolu_123",
            "content": "File created"
        });
        let result = ConversationSession::format_content_block(&block);
        assert_eq!(result, None);
    }

    #[test]
    fn test_is_tool_result_entry_true() {
        let entry: ConversationEntry = serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "1",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "abc", "content": "ok"}
                ]
            }
        }))
        .unwrap();
        assert!(ConversationSession::is_tool_result_entry(&entry));
    }

    #[test]
    fn test_is_tool_result_entry_false_for_real_user() {
        let entry: ConversationEntry = serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "1",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "Hello"}
                ]
            }
        }))
        .unwrap();
        assert!(!ConversationSession::is_tool_result_entry(&entry));
    }

    #[test]
    fn test_is_tool_result_entry_false_for_assistant() {
        let entry: ConversationEntry = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "uuid": "1",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "Hi"}]
            }
        }))
        .unwrap();
        assert!(!ConversationSession::is_tool_result_entry(&entry));
    }

    #[test]
    fn test_extract_display_content_assistant_mixed() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I'll create the file."},
                {"type": "tool_use", "name": "Write", "id": "t1", "input": {"file_path": "/src/app.rs", "content": "..."}}
            ]
        });
        let result = ConversationSession::extract_display_content(&msg, false).unwrap();
        assert!(result.contains("I'll create the file."));
        assert!(result.contains("[Tool: Write -> app.rs]"));
    }

    #[test]
    fn test_extract_display_content_user_plain_string() {
        let msg = serde_json::json!({
            "role": "user",
            "content": "Hello world"
        });
        let result = ConversationSession::extract_display_content(&msg, true).unwrap();
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_extract_display_content_skips_system_content() {
        let msg = serde_json::json!({
            "role": "user",
            "content": "<ide_opened_file>/src/main.rs</ide_opened_file>"
        });
        let result = ConversationSession::extract_display_content(&msg, true);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_display_content_skips_tool_result_only() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
            ]
        });
        let result = ConversationSession::extract_display_content(&msg, true);
        assert!(result.is_none());
    }

    #[test]
    fn test_user_interaction_answer_detected() {
        assert!(is_user_interaction_result(
            "User has answered your questions: \"tag lang\"=\"english\""
        ));
    }

    #[test]
    fn test_user_interaction_approved() {
        assert!(is_user_interaction_result("User has approved your plan."));
    }

    #[test]
    fn test_user_interaction_rejected() {
        assert!(is_user_interaction_result(
            "The user doesn't want to proceed with this tool use."
        ));
    }

    #[test]
    fn test_regular_tool_result_not_interaction() {
        assert!(!is_user_interaction_result("File created successfully"));
        assert!(!is_user_interaction_result("   1→use anyhow"));
    }

    #[test]
    fn test_format_user_interaction_answer() {
        let content = "User has answered your questions: \"Which lang?\"=\"English\". You can now continue.";
        let result = format_user_interaction(content);
        assert!(result.starts_with("[User answered:"));
        assert!(result.contains("Which lang?"));
        assert!(result.contains("English"));
    }

    #[test]
    fn test_format_user_interaction_approved() {
        let result = format_user_interaction("User has approved your plan. Start coding.");
        assert_eq!(result, "[User approved plan]");
    }

    #[test]
    fn test_format_user_interaction_rejected() {
        let content = "The user doesn't want to proceed with this tool use. The tool use was rejected. To tell you how to proceed, the user said:\nPlease add memory search support";
        let result = format_user_interaction(content);
        assert!(result.starts_with("[User rejected:"));
        assert!(result.contains("memory search"));
    }

    #[test]
    fn test_is_tool_result_entry_not_skipped_for_user_interaction() {
        let entry: ConversationEntry = serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "1",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "User has answered your questions: \"q\"=\"a\""}
                ]
            }
        }))
        .unwrap();
        // Should NOT be treated as a tool_result entry (should be displayed)
        assert!(!ConversationSession::is_tool_result_entry(&entry));
    }

    #[test]
    fn test_format_content_block_user_interaction_tool_result() {
        let block = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "t1",
            "content": "User has approved your plan."
        });
        let result = ConversationSession::format_content_block(&block);
        assert_eq!(result, Some("[User approved plan]".to_string()));
    }
}
