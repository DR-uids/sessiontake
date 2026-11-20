use crate::catalog::{
    array, cursor_events, find_claude_transcript, json_lines, projected_events, string,
    user_prompt, Failure, Indexed, Record, Session, Source,
};
use chrono::{DateTime, Local, SecondsFormat};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Debug)]
pub struct MessageImage {
    pub span: Range<usize>,
    pub name: Option<String>,
    pub raster: Option<Raster>,
}

#[derive(Debug)]
pub struct Raster {
    pub mime: &'static str,
    pub encoded: Range<usize>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Entry {
    Message {
        role: &'static str,
        text: String,
        time: Option<String>,
        #[serde(skip)]
        images: Vec<MessageImage>,
    },
    Call {
        call_id: Option<String>,
        name: String,
        input: Option<String>,
        time: Option<String>,
    },
    Result {
        call_id: Option<String>,
        name: Option<String>,
        output: String,
        time: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session: Session,
    pub entries: Vec<Entry>,
    pub jsonl: String,
}

impl Snapshot {
    pub fn markdown(&self) -> String {
        use crate::catalog::Origin;
        use std::fmt::Write;

        let session = &self.session;
        let tool = match session.origin {
            Origin::Codex => "Codex",
            Origin::Cursor => "Cursor",
            Origin::Workbuddy => "WorkBuddy",
            Origin::Harness => "DeepSeek Harness",
            Origin::ClaudeCli => "Claude Code (CLI)",
            Origin::ClaudeDesktop => "Claude Code (Desktop)",
        };
        let mut title = String::with_capacity(session.title.len());
        let mut in_line_break = false;
        for ch in session.title.chars() {
            let line_break = matches!(ch, '\r' | '\n');
            if !line_break || !in_line_break {
                title.push(if line_break { ' ' } else { ch });
            }
            in_line_break = line_break;
        }
        let mut body = format!("# {title}\n\n- Tool: {tool}\n- Session: {}", session.id);
        for (label, value) in [
            ("Project", &session.project),
            ("Worktree", &session.worktree),
        ] {
            if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
                write!(body, "\n- {label}: {value}").unwrap();
            }
        }
        if (1..=8_640_000_000_000_000).contains(&session.updated_at) {
            if let Some(updated) = DateTime::from_timestamp_millis(session.updated_at) {
                write!(
                    body,
                    "\n- Updated: {}",
                    updated.to_rfc3339_opts(SecondsFormat::Millis, true)
                )
                .unwrap();
            }
        }
        let calls: HashMap<_, _> = self
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Call {
                    call_id: Some(id),
                    name,
                    ..
                } if !id.is_empty() => Some((id, name)),
                _ => None,
            })
            .collect();
        for entry in &self.entries {
            body.push_str("\n\n");
            match entry {
                Entry::Message {
                    role, text, time, ..
                } => {
                    body.push_str(if *role == "user" {
                        "## User"
                    } else {
                        "## Assistant"
                    });
                    write_suffix(&mut body, None, time.as_deref());
                    write!(body, "\n\n{text}").unwrap();
                }
                Entry::Call {
                    call_id,
                    name,
                    input,
                    time,
                } => {
                    write!(body, "### Tool call · {name}").unwrap();
                    write_suffix(&mut body, call_id.as_deref(), time.as_deref());
                    if let Some(input) = input {
                        write_fenced(&mut body, input);
                    }
                }
                Entry::Result {
                    call_id,
                    name,
                    output,
                    time,
                } => {
                    body.push_str("### Tool result");
                    let name = name
                        .as_ref()
                        .or_else(|| calls.get(call_id.as_ref()?).copied());
                    if let Some(name) = name.filter(|name| !name.is_empty()) {
                        write!(body, " · {name}").unwrap();
                    }
                    write_suffix(&mut body, call_id.as_deref(), time.as_deref());
                    write_fenced(&mut body, output);
                }
            }
        }
        body.push('\n');
        body
    }
}

fn write_suffix(body: &mut String, id: Option<&str>, time: Option<&str>) {
    use std::fmt::Write;
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        write!(body, " ({id})").unwrap();
    }
    if let Some(time) = time.filter(|time| !time.is_empty()) {
        write!(body, " · {time}").unwrap();
    }
}

fn write_fenced(body: &mut String, text: &str) {
    use std::fmt::Write;
    let mut run = 0;
    let mut longest = 2;
    for byte in text.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let newline = if text.ends_with('\n') { "" } else { "\n" };
    write!(body, "\n\n{fence}\n{text}{newline}{fence}").unwrap();
}

pub fn read(indexed: Indexed) -> Result<Snapshot, Failure> {
    let (events, projection) = match &indexed.record {
        Record::File(path) => (json_lines(path)?.collect::<Result<Vec<_>, _>>()?, false),
        Record::CursorComposer { db, transcript } => {
            let rows = cursor_events(db, &indexed.session.id).or_else(|err| {
                let path = transcript.as_ref().ok_or(err)?;
                json_lines(path)?.collect::<Result<Vec<_>, _>>()
            })?;
            (rows, false)
        }
        Record::CodexProjection(path) => (projected_events(path, &indexed.session.id)?, true),
        Record::ClaudeMissing(projects) => {
            let path = find_claude_transcript(projects, &indexed.session.id)?;
            (json_lines(&path)?.collect::<Result<Vec<_>, _>>()?, false)
        }
    };
    if events.is_empty() {
        return Err(Failure::InvalidRecord);
    }
    build(indexed.session, events, projection)
}

fn build(session: Session, mut events: Vec<Value>, projection: bool) -> Result<Snapshot, Failure> {
    if matches!(session.source, Source::Workbuddy | Source::Cursor) {
        events.retain_mut(|event| {
            editor_event(event, session.source, crate::catalog::without_reminders)
        });
    }
    events.retain_mut(sanitize);
    let mut jsonl = Vec::new();
    for event in &events {
        let native = if projection { &event["item"] } else { event };
        serde_json::to_writer(&mut jsonl, native).map_err(|_| Failure::InvalidRecord)?;
        jsonl.push(b'\n');
    }
    let jsonl = String::from_utf8(jsonl).map_err(|_| Failure::InvalidRecord)?;

    let mut entries = Vec::new();
    match session.source {
        Source::Codex if projection => {
            for event in events.drain(..) {
                let time = timestamp(&event["time"]);
                codex_item(&event["item"], time, &mut entries);
            }
        }
        Source::Codex => codex_entries(&events, &mut entries),
        Source::Harness => harness_entries(&events, &mut entries),
        Source::Cursor | Source::Workbuddy => {
            events.retain_mut(|event| editor_event(event, session.source, user_prompt));
            editor_entries(&events, &mut entries);
        }
        Source::Claude => {
            for event in &events {
                if let Some(role) =
                    string(&event["message"]["role"]).or_else(|| string(&event["type"]))
                {
                    if event["isSidechain"] != true {
                        message_content(
                            role,
                            &event["message"]["content"],
                            timestamp(&event["timestamp"]),
                            &mut entries,
                        );
                    }
                }
            }
        }
    }

    Ok(Snapshot {
        session,
        entries,
        jsonl,
    })
}

// Remove runtime prompt envelopes only from user text. Tool output is opaque,
// even when a command printed something that looks like another message.
fn editor_event(row: &mut Value, source: Source, normalize: fn(&str) -> &str) -> bool {
    if source == Source::Cursor && row["type"].is_number() {
        return matches!(row["type"].as_i64(), Some(1 | 2));
    }
    if row["role"] != "user" {
        return true;
    }
    let msg = if row["message"].is_object() {
        &mut row["message"]
    } else {
        row
    };
    let clean = |value: &mut Value| {
        if let Some(text) = value.as_str() {
            let prompt = normalize(text);
            if prompt != text {
                *value = Value::String(prompt.to_owned());
            }
        }
        value.as_str() != Some("")
    };
    match msg.get_mut("content") {
        Some(Value::Array(parts)) => {
            parts.retain_mut(|part| text_part(part).is_none() || clean(&mut part["text"]));
            !parts.is_empty()
        }
        Some(value) => clean(value),
        None => true,
    }
}

fn editor_entries(events: &[Value], entries: &mut Vec<Entry>) {
    let mut workbuddy_calls = HashMap::new();
    for row in events {
        let time = timestamp(row.get("timestamp").unwrap_or(&row["createdAt"]));
        if let Some(ty) = row["type"].as_i64() {
            let role = if ty == 1 { "user" } else { "assistant" };
            message(
                entries,
                role,
                string(&row["text"]).map(str::to_owned),
                time.clone(),
            );
            let tool = &row["toolFormerData"];
            if let Some(name) = string(&tool["name"]) {
                let key = id(tool).or_else(|| string(&row["bubbleId"]).map(str::to_owned));
                call(
                    entries,
                    key.clone(),
                    Some(name),
                    call_input(tool.get("rawArgs").unwrap_or(&tool["params"])),
                    time.clone(),
                );
                result(entries, key, Some(name), content(&tool["result"]), time);
            }
            continue;
        }
        match kind(row) {
            "function_call" => {
                let key = id(row);
                if let (Some(event_id), Some(call_id)) = (string(&row["id"]), key.as_deref()) {
                    workbuddy_calls.insert(event_id, call_id.to_owned());
                }
                call(
                    entries,
                    key,
                    string(&row["name"]),
                    call_input(&row["arguments"]),
                    time,
                );
            }
            "function_call_result" | "function_call_output" => {
                let key = string(&row["callId"])
                    .or_else(|| string(&row["call_id"]))
                    .or_else(|| {
                        string(&row["parentId"])
                            .and_then(|id| workbuddy_calls.get(id).map(String::as_str))
                    });
                let output = &row["output"];
                result(
                    entries,
                    key.map(str::to_owned),
                    string(&row["name"]),
                    text_part(output)
                        .map(str::to_owned)
                        .or_else(|| content(output)),
                    time,
                );
            }
            _ => {
                if let Some(role @ ("user" | "assistant")) = row["role"].as_str() {
                    let msg = row.get("message").unwrap_or(row);
                    message_content(role, &msg["content"], time, entries);
                }
            }
        }
    }
}

fn kind(value: &Value) -> &str {
    value["type"].as_str().unwrap_or_default()
}

fn hidden(value: &Value) -> bool {
    matches!(value["role"].as_str(), Some("system" | "developer"))
        || matches!(
            kind(value),
            "system"
                | "developer"
                | "system/message"
                | "developer/message"
                | "systemMessage"
                | "developerMessage"
                | "SystemMessage"
                | "DeveloperMessage"
        )
        || (value["role"] == "user"
            && matches!(
                value["source"]["kind"].as_str(),
                Some("runtime-context" | "skill-catalog" | "time-context" | "agent-instructions")
            ))
        || (value["type"] == "response_item"
            && value["payload"]["role"] == "user"
            && value["metadata"]["user_input_order"].is_null()
            && array(&value["payload"]["content"])
                .first()
                .is_some_and(|part| {
                    part["text"].as_str().is_some_and(|text| {
                        text.starts_with("<environment_context>")
                            && text.trim_end().ends_with("</environment_context>")
                    })
                }))
}

fn sanitize(value: &mut Value) -> bool {
    if hidden(value) {
        return false;
    }
    let is_tool = matches!(
        kind(value),
        "tool_use"
            | "tool_result"
            | "tool-call"
            | "tool-result"
            | "tool/call"
            | "tool/result"
            | "function_call"
            | "function_call_output"
            | "function_call_result"
            | "custom_tool_call"
            | "custom_tool_call_output"
            | "local_shell_call"
            | "local_shell_call_output"
    );
    let codex_meta = kind(value) == "session_meta";
    let harness_context = matches!(kind(value), "request/context" | "session/title-llm-request");
    let tool_output_message = value["role"] == "tool";
    match value {
        Value::Array(items) => items.retain_mut(sanitize),
        Value::Object(fields) => {
            if harness_context {
                if let Some(data) = fields.get_mut("data").and_then(Value::as_object_mut) {
                    data.remove("systemPromptUpdate");
                    data.remove("system");
                }
            }
            for key in ["payload", "data", "message", "item"] {
                if fields.get_mut(key).is_some_and(|child| !sanitize(child)) {
                    return false;
                }
            }
            fields.retain(|key, child| {
                if codex_meta && key == "base_instructions" {
                    return false;
                }
                // Tool inputs and outputs are event data, including arbitrary
                // JSON that can itself contain fields named role or type.
                if matches!(
                    key.as_str(),
                    "payload"
                        | "data"
                        | "message"
                        | "item"
                        | "arguments"
                        | "input"
                        | "output"
                        | "toolUseResult"
                        | "toolFormerData"
                ) || (is_tool && matches!(key.as_str(), "content" | "result"))
                    || (tool_output_message && key == "content")
                {
                    return true;
                }
                sanitize(child)
            });
        }
        _ => (),
    }
    true
}

fn timestamp(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|_| text.to_owned());
    }
    let raw = value.as_i64()?;
    let millis = if raw.abs_diff(0) < 10_000_000_000 {
        raw * 1000
    } else {
        raw
    };
    Some(
        DateTime::from_timestamp_millis(millis)?
            .with_timezone(&Local)
            .to_rfc3339_opts(SecondsFormat::Millis, false),
    )
}

fn id(value: &Value) -> Option<String> {
    ["call_id", "callId", "tool_use_id", "toolCallId", "id"]
        .into_iter()
        .find_map(|key| string(&value[key]))
        .map(str::to_owned)
}

fn text_part(value: &Value) -> Option<&str> {
    matches!(kind(value), "text" | "Text" | "input_text" | "output_text")
        .then(|| value["text"].as_str())
        .flatten()
}

fn pretty(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    }
}

fn content(value: &Value) -> Option<String> {
    if let Some(parts) = value.as_array() {
        return Some(
            parts
                .iter()
                .filter_map(|part| text_part(part).map(str::to_owned).or_else(|| pretty(part)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    pretty(value)
}

fn call_input(value: &Value) -> Option<String> {
    if let Some(raw) = value.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
            if let Some(command) = parsed.get("command").or_else(|| parsed.get("cmd")) {
                return pretty(command);
            }
        }
    }
    if let Some(command) = value.get("command").or_else(|| value.get("cmd")) {
        return pretty(command);
    }
    pretty(value)
}

fn call(
    entries: &mut Vec<Entry>,
    id: Option<String>,
    name: Option<&str>,
    input: Option<String>,
    time: Option<String>,
) {
    entries.push(Entry::Call {
        call_id: id,
        name: name.unwrap_or("Tool").to_owned(),
        input,
        time,
    });
}

fn result(
    entries: &mut Vec<Entry>,
    id: Option<String>,
    name: Option<&str>,
    output: Option<String>,
    time: Option<String>,
) {
    if let Some(output) = output {
        entries.push(Entry::Result {
            call_id: id,
            name: name.map(str::to_owned),
            output,
            time,
        });
    }
}

fn message(entries: &mut Vec<Entry>, role: &str, text: Option<String>, time: Option<String>) {
    if let Some(text) = text {
        push_message(entries, role, text, time, Vec::new());
    }
}

fn push_message(
    entries: &mut Vec<Entry>,
    role: &str,
    text: String,
    time: Option<String>,
    images: Vec<MessageImage>,
) {
    let role = match role {
        "user" => "user",
        "assistant" => "assistant",
        _ => return,
    };
    entries.push(Entry::Message {
        role,
        text,
        time,
        images,
    });
}

#[derive(Default)]
struct MessageBody {
    text: String,
    last_start: Option<usize>,
    images: Vec<MessageImage>,
}

impl MessageBody {
    fn push(&mut self, part: &str) -> usize {
        if self.last_start.is_some() {
            self.text.push('\n');
        }
        let start = self.text.len();
        self.last_start = Some(start);
        self.text.push_str(part);
        start
    }

    fn flush(&mut self, entries: &mut Vec<Entry>, role: &str, time: Option<String>) {
        if self.last_start.is_some() {
            let body = std::mem::take(self);
            push_message(entries, role, body.text, time, body.images);
        }
    }
}

fn image_name(opening: &str) -> Option<&str> {
    let (name, path) = opening
        .strip_prefix("<image name=[")?
        .split_once("] path=\"")?;
    path.strip_suffix("\">")?;
    Some(name)
}

fn image_data(part: &Value) -> Option<(&'static str, &str)> {
    let source = &part["source"];
    let (mime, encoded) = if kind(source) == "base64" {
        (source["media_type"].as_str()?, source["data"].as_str()?)
    } else {
        let url = part["image_url"]
            .as_str()
            .or_else(|| part["image_url"]["url"].as_str())?;
        url.strip_prefix("data:")?.split_once(";base64,")?
    };
    // Limit on-demand transfers to roughly 8 MiB of decoded image data.
    if encoded.is_empty() || encoded.len() > 11 * 1024 * 1024 {
        return None;
    }
    let mime = match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/avif" => "image/avif",
        _ => return None,
    };
    Some((mime, encoded))
}

fn message_content(role: &str, value: &Value, time: Option<String>, entries: &mut Vec<Entry>) {
    let Some(parts) = value.as_array() else {
        message(entries, role, content(value), time);
        return;
    };
    let mut body = MessageBody::default();
    let mut parts = parts.iter().enumerate().peekable();
    let mut previous: Option<(usize, usize, &str)> = None;
    while let Some((index, part)) = parts.next() {
        match kind(part) {
            "tool_use" | "server_tool_use" | "tool_result" => {
                body.flush(entries, role, time.clone());
                if kind(part) == "tool_result" {
                    result(
                        entries,
                        id(part),
                        None,
                        content(&part["content"]),
                        time.clone(),
                    );
                } else {
                    call(
                        entries,
                        id(part),
                        string(&part["name"]),
                        call_input(&part["input"]),
                        time.clone(),
                    );
                }
            }
            "input_image" | "image" | "Image" | "image_url" => {
                let Some(raw) = pretty(part) else { continue };
                let wrapper = previous
                    .filter(|(prev, _, _)| *prev + 1 == index)
                    .filter(|_| {
                        parts.peek().and_then(|(_, next)| text_part(next)) == Some("</image>")
                    });
                let start = body.push(&raw);
                let raster = image_data(part).and_then(|(mime, encoded)| {
                    let offset = start + raw.find(encoded)?;
                    Some(Raster {
                        mime,
                        encoded: offset..offset + encoded.len(),
                    })
                });
                let (span_start, name) = if let Some((_, opening, name)) = wrapper {
                    body.push("</image>");
                    parts.next();
                    (opening, Some(name.chars().take(120).collect()))
                } else {
                    (start, None)
                };
                body.images.push(MessageImage {
                    span: span_start.saturating_sub(1)..body.text.len(),
                    name,
                    raster,
                });
                previous = None;
            }
            "thinking" | "redacted_thinking" | "reasoning" | "tool-call" => (),
            _ => {
                if let Some(text) = text_part(part) {
                    let start = body.push(text);
                    previous = image_name(text).map(|name| (index, start, name));
                } else if let Some(raw) = pretty(part) {
                    body.push(&raw);
                }
            }
        }
    }
    body.flush(entries, role, time);
}

fn codex_entries(events: &[Value], entries: &mut Vec<Entry>) {
    let mut message_ids = HashSet::new();
    let mut response_roles = HashSet::new();
    for event in events {
        let payload = &event["payload"];
        if kind(event) == "response_item" && kind(payload) == "message" {
            if let Some(role) = payload["role"].as_str() {
                response_roles.insert(role);
            }
            if let Some(id) = string(&payload["id"]) {
                message_ids.insert(id);
            }
        }
    }
    let mut completion_ids = HashSet::new();
    for event in events {
        let payload = &event["payload"];
        let time = timestamp(&event["timestamp"]);
        match (kind(event), kind(payload)) {
            ("response_item", "message") => {
                if let Some(role) = payload["role"].as_str() {
                    message_content(role, &payload["content"], time, entries);
                }
            }
            (
                "response_item",
                "function_call" | "custom_tool_call" | "local_shell_call" | "mcp_tool_call",
            ) => {
                call(
                    entries,
                    id(payload),
                    string(&payload["name"]).or(Some(kind(payload))),
                    call_input(
                        payload
                            .get("arguments")
                            .or_else(|| payload.get("input"))
                            .unwrap_or(&payload["action"]),
                    ),
                    time,
                );
            }
            (
                "response_item",
                "function_call_output"
                | "custom_tool_call_output"
                | "local_shell_call_output"
                | "mcp_tool_call_output",
            ) => {
                result(
                    entries,
                    id(payload),
                    None,
                    content(&payload["output"]),
                    time,
                );
            }
            ("event_msg", "item_completed") => {
                let item = &payload["item"];
                if string(&item["id"])
                    .is_some_and(|id| message_ids.contains(id) || !completion_ids.insert(id))
                {
                    continue;
                }
                if matches!(kind(item), "UserMessage" | "userMessage")
                    && response_roles.contains("user")
                {
                    continue;
                }
                codex_item(item, time, entries);
            }
            ("event_msg", "user_message") if !response_roles.contains("user") => {
                message(entries, "user", pretty(&payload["message"]), time);
            }
            ("event_msg", "agent_message") if !response_roles.contains("assistant") => {
                message(entries, "assistant", pretty(&payload["message"]), time);
            }
            ("event_msg", "exec_command_begin") => {
                call(
                    entries,
                    id(payload),
                    Some("exec_command"),
                    pretty(&payload["command"]),
                    time,
                );
            }
            ("event_msg", "exec_command_end") => {
                result(
                    entries,
                    id(payload),
                    Some("exec_command"),
                    pretty(&payload["aggregated_output"]),
                    time,
                );
            }
            _ => (),
        }
    }
}

fn codex_item(item: &Value, time: Option<String>, entries: &mut Vec<Entry>) {
    let (name, input, output) = match kind(item) {
        "UserMessage" | "userMessage" => {
            return message_content("user", &item["content"], time, entries);
        }
        "AgentMessage" | "agentMessage" => {
            return message_content(
                "assistant",
                item.get("text").unwrap_or(&item["content"]),
                time,
                entries,
            );
        }
        "CommandExecution" | "commandExecution" => (
            "exec_command",
            pretty(&item["command"]),
            pretty(
                item.get("aggregatedOutput")
                    .unwrap_or(&item["aggregated_output"]),
            ),
        ),
        "McpToolCall" | "mcpToolCall" | "DynamicToolCall" | "dynamicToolCall" => (
            string(&item["tool"]).unwrap_or("MCP"),
            call_input(&item["arguments"]),
            content(
                item.get("result")
                    .or_else(|| item.get("contentItems"))
                    .unwrap_or(&item["error"]),
            ),
        ),
        "FileChange" | "fileChange" => (
            "file_change",
            pretty(&item["changes"]),
            value_fields(item, &["stdout", "stderr"]),
        ),
        "WebSearch" | "webSearch" | "Extension" => (
            "web_search",
            pretty(item.get("query").unwrap_or(&item["action"])),
            content(&item["results"]),
        ),
        "ImageView" | "imageView" => ("view_image", pretty(&item["path"]), None),
        "ImageGeneration" | "imageGeneration" => (
            "image_generation",
            pretty(&item["revisedPrompt"]),
            pretty(item.get("result").unwrap_or(&item["savedPath"])),
        ),
        _ => return,
    };
    let id = id(item);
    call(entries, id.clone(), Some(name), input, time.clone());
    result(entries, id, Some(name), output, time);
}

fn value_fields(value: &Value, keys: &[&str]) -> Option<String> {
    let values = keys
        .iter()
        .filter_map(|key| pretty(&value[*key]))
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("\n"))
}

fn harness_entries(events: &[Value], entries: &mut Vec<Entry>) {
    let explicit_calls = events
        .iter()
        .filter(|event| kind(event) == "tool/call")
        .filter_map(|event| id(&event["data"]))
        .collect::<HashSet<_>>();
    for event in events {
        let data = &event["data"];
        let time = timestamp(event.get("time").unwrap_or(&event["timestamp"]));
        match kind(event) {
            "user/message"
                if data["source"]["kind"].is_null() || data["source"]["kind"] == "user" =>
            {
                message_content("user", &data["content"], time, entries);
            }
            "assistant/message" => {
                let value = data.get("message").unwrap_or(data);
                message_content("assistant", &value["content"], time.clone(), entries);
                for part in array(&value["content"])
                    .iter()
                    .filter(|part| kind(part) == "tool-call")
                {
                    let id = id(part);
                    if id.as_ref().is_none_or(|id| !explicit_calls.contains(id)) {
                        call(
                            entries,
                            id,
                            string(&part["name"]),
                            call_input(&part["arguments"]),
                            time.clone(),
                        );
                    }
                }
            }
            "tool/call" => {
                call(
                    entries,
                    id(data),
                    string(&data["name"]),
                    call_input(data.get("arguments").unwrap_or(&data["input"])),
                    time,
                );
            }
            "tool/result" => {
                let value = data.get("message").unwrap_or(data);
                result(
                    entries,
                    id(value).or_else(|| id(data)),
                    string(&data["name"]),
                    content(value.get("content").unwrap_or(&data["output"])),
                    time,
                );
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_wrapped_and_composite_messages_without_changing_tool_results() {
        let mut developer = json!({"type":"response_item","payload":{"type":"message","role":"developer","content":"hidden"}});
        assert!(!sanitize(&mut developer));

        let mut wrapped = json!({"type":"compacted","payload":{"replacement_history":[
            {"type":"message","role":"system","content":"secret"},
            {"type":"message","role":"user","content":"question"},
            {"type":"function_call_output","call_id":"a","output":{"role":"system","value":"opaque output"}},
            {"type":"message","role":"assistant","content":"answer"}
        ]}});
        assert!(sanitize(&mut wrapped));
        assert_eq!(
            wrapped["payload"]["replacement_history"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            wrapped["payload"]["replacement_history"][1]["output"]["value"],
            "opaque output"
        );

        let mut claude = json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"a","content":[{"type":"system","text":"opaque"}]}
        ]}});
        let before = claude.clone();
        assert!(sanitize(&mut claude));
        assert_eq!(claude, before);
    }

    #[test]
    fn rollout_retains_source_order_and_native_call_ids() {
        let events = vec![
            json!({"type":"response_item","timestamp":"2026-11-18T23:58:00+08:00","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"first"}]}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"1","name":"exec","arguments":"{\"cmd\":\"one\"}"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"2","name":"exec","arguments":"{\"cmd\":\"two\"}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"2","output":"second\nline"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"1","output":"first result"}}),
            json!({"type":"response_item","timestamp":"2026-11-19T00:02:00+08:00","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"last"}]}}),
        ];
        let mut entries = Vec::new();
        codex_entries(&events, &mut entries);
        assert_eq!(entries.len(), 6);
        assert!(
            matches!(&entries[1], Entry::Call { call_id: Some(id), time: None, input: Some(input), .. } if id == "1" && input == "one")
        );
        assert!(
            matches!(&entries[3], Entry::Result { call_id: Some(id), output, .. } if id == "2" && output == "second\nline")
        );
        assert!(
            matches!(&entries[5], Entry::Message { text, time: Some(time), .. } if text == "last" && time.starts_with("2026-11-19"))
        );
    }
}
