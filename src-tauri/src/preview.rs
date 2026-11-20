use crate::{
    catalog::Failure,
    fences::{self, Fence, TextSection},
    search,
    transcript::{Entry, MessageImage, Snapshot},
};
use base64::{engine::general_purpose::STANDARD_NO_PAD_INDIFFERENT, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::OnceLock,
};

// Keep each IPC response and WebKit layout bounded, even for one long line.
const PAGE_BYTES: usize = 32 * 1024;
const PAGE_ROWS: usize = 60;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Format {
    Md,
    Jsonl,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum View {
    Conversation,
    Md,
    Jsonl,
}

#[derive(Serialize)]
pub struct Page<T> {
    content: T,
    total: usize,
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Row<'a> {
    Message {
        role: &'static str,
        text: Vec<TextSection<'a>>,
        time: Option<&'a str>,
        index: usize,
        continued: bool,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageCard<'a>>,
    },
    Tool {
        index: usize,
        name: Option<&'a str>,
        hint: Option<&'a str>,
        call_id: Option<&'a str>,
        time: Option<&'a str>,
    },
}

#[derive(Serialize)]
pub struct ImageCard<'a> {
    name: Option<&'a str>,
    mime: Option<&'static str>,
}

#[derive(Serialize)]
pub struct ToolPart<'a> {
    index: usize,
    command: bool,
    text: &'a str,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ViewPage<'a> {
    Conversation(Page<Vec<Row<'a>>>),
    Source(Page<Vec<TextSection<'a>>>),
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CodeBlock {
    Markdown { block: usize },
    Message { index: usize, block: usize },
    Tool { index: usize },
}

type EntryPages = Vec<Vec<(usize, Range<usize>)>>;

pub struct Preview {
    snapshot: Snapshot,
    markdown: OnceLock<String>,
    markdown_pages: OnceLock<Vec<Range<usize>>>,
    markdown_fences: OnceLock<Vec<Fence>>,
    message_fences: Vec<OnceLock<Vec<Fence>>>,
    jsonl_pages: OnceLock<Vec<Range<usize>>>,
    conversation: EntryPages,
    image_messages: HashMap<usize, String>,
    tool_pages: Vec<OnceLock<EntryPages>>,
    // Entry indices keep tool output out of the conversation response.
    outputs: HashMap<String, Vec<usize>>,
    pub message_count: usize,
}

impl Preview {
    pub fn new(snapshot: Snapshot) -> Self {
        let mut calls = HashSet::new();
        let mut outputs: HashMap<_, Vec<_>> = HashMap::new();
        let mut message_count = 0;
        let mut image_messages = HashMap::new();
        for (index, entry) in snapshot.entries.iter().enumerate() {
            match entry {
                Entry::Message { text, images, .. } => {
                    message_count += 1;
                    if !images.is_empty() {
                        image_messages.insert(index, text_without_images(text, images));
                    }
                }
                Entry::Call {
                    call_id: Some(id), ..
                } if !id.is_empty() => {
                    calls.insert(id);
                }
                Entry::Result {
                    call_id: Some(id), ..
                } if !id.is_empty() => {
                    outputs.entry(id.clone()).or_default().push(index);
                }
                _ => (),
            }
        }
        let conversation = entry_pages(snapshot.entries.iter().enumerate().filter_map(
            |(index, entry)| match entry {
                Entry::Result {
                    call_id: Some(id), ..
                } if calls.contains(id) => None,
                Entry::Message { text, .. } => {
                    Some((index, image_messages.get(&index).unwrap_or(text).as_str()))
                }
                _ => Some((index, "")),
            },
        ));
        Self {
            tool_pages: snapshot.entries.iter().map(|_| OnceLock::new()).collect(),
            message_fences: snapshot.entries.iter().map(|_| OnceLock::new()).collect(),
            snapshot,
            markdown: OnceLock::new(),
            markdown_pages: OnceLock::new(),
            markdown_fences: OnceLock::new(),
            jsonl_pages: OnceLock::new(),
            conversation,
            image_messages,
            outputs,
            message_count,
        }
    }

    pub fn source(&self, format: Format) -> &str {
        match format {
            Format::Md => self.markdown.get_or_init(|| self.snapshot.markdown()),
            Format::Jsonl => &self.snapshot.jsonl,
        }
    }

    pub fn view(&self, view: View, page: usize) -> Result<ViewPage<'_>, Failure> {
        let (format, pages) = match view {
            View::Conversation => {
                let rows = page_at(&self.conversation, page)?;
                return Ok(ViewPage::Conversation(Page {
                    content: rows
                        .iter()
                        .map(|(index, range)| self.row(*index, range))
                        .collect(),
                    total: self.conversation.len(),
                }));
            }
            View::Md => (Format::Md, &self.markdown_pages),
            View::Jsonl => (Format::Jsonl, &self.jsonl_pages),
        };
        let source = self.source(format);
        let pages = pages.get_or_init(|| text_ranges(source).collect());
        let blocks = match format {
            Format::Md => self
                .markdown_fences
                .get_or_init(|| fences::blocks(source))
                .as_slice(),
            Format::Jsonl => &[],
        };
        Ok(ViewPage::Source(Page {
            content: fences::sections(source, page_at(pages, page)?.clone(), blocks),
            total: pages.len(),
        }))
    }

    fn row(&self, index: usize, range: &Range<usize>) -> Row<'_> {
        match &self.snapshot.entries[index] {
            Entry::Message {
                role,
                time,
                text,
                images,
            } => Row::Message {
                role,
                text: self.message_sections(index, text, range),
                time: time.as_deref(),
                index,
                continued: range.start != 0,
                images: if range.start == 0 {
                    images
                        .iter()
                        .map(|image| ImageCard {
                            name: image.name.as_deref(),
                            mime: image.raster.as_ref().map(|raster| raster.mime),
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            },
            Entry::Call {
                call_id,
                name,
                input,
                time,
            } => Row::Tool {
                index,
                name: Some(name),
                hint: input.as_deref().map(command_hint),
                call_id: call_id.as_deref(),
                time: time.as_deref(),
            },
            Entry::Result {
                call_id,
                name,
                time,
                ..
            } => Row::Tool {
                index,
                name: name.as_deref(),
                hint: None,
                call_id: call_id.as_deref(),
                time: time.as_deref(),
            },
        }
    }

    fn message_text<'a>(&'a self, index: usize, text: &'a str) -> &'a str {
        self.image_messages.get(&index).map_or(text, String::as_str)
    }

    fn message_sections<'a>(
        &'a self,
        index: usize,
        text: &'a str,
        range: &Range<usize>,
    ) -> Vec<TextSection<'a>> {
        let text = self.message_text(index, text);
        let blocks = self.message_fences[index].get_or_init(|| fences::blocks(text));
        fences::sections(text, range.clone(), blocks)
    }

    pub fn code_block(&self, target: CodeBlock) -> Result<&str, Failure> {
        match target {
            CodeBlock::Markdown { block } => {
                let text = self.source(Format::Md);
                let blocks = self.markdown_fences.get_or_init(|| fences::blocks(text));
                Ok(&text[page_at(blocks, block)?.body.clone()])
            }
            CodeBlock::Message { index, block } => {
                let Some(Entry::Message { text, .. }) = self.snapshot.entries.get(index) else {
                    return Err(Failure::Missing);
                };
                let text = self.message_text(index, text);
                let blocks = self.message_fences[index].get_or_init(|| fences::blocks(text));
                Ok(&text[page_at(blocks, block)?.body.clone()])
            }
            CodeBlock::Tool { index } => match self.snapshot.entries.get(index) {
                Some(Entry::Call {
                    input: Some(input), ..
                }) => Ok(input),
                Some(Entry::Result { output, .. }) => Ok(output),
                _ => Err(Failure::Missing),
            },
        }
    }

    pub fn search(
        &self,
        view: View,
        query: &str,
        cursor: Option<search::Cursor>,
    ) -> Result<search::Results<'_>, Failure> {
        let format =
            match view {
                View::Conversation => {
                    let texts =
                        self.snapshot.entries.iter().enumerate().map(
                            |(index, entry)| match entry {
                                Entry::Message { role, text, .. } => {
                                    Some((*role, self.message_text(index, text)))
                                }
                                Entry::Call { input, name, .. } => {
                                    Some(("command", input.as_deref().unwrap_or(name)))
                                }
                                Entry::Result { output, .. } => Some(("output", output.as_str())),
                            },
                        );
                    return search::find(query, cursor, texts);
                }
                View::Md => Format::Md,
                View::Jsonl => Format::Jsonl,
            };
        search::find(
            query,
            cursor,
            std::iter::once(Some(("source", self.source(format)))),
        )
    }

    pub fn image(&self, index: usize, image: usize) -> Result<Vec<u8>, Failure> {
        let Some(Entry::Message { text, images, .. }) = self.snapshot.entries.get(index) else {
            return Err(Failure::Missing);
        };
        let raster = images
            .get(image)
            .and_then(|image| image.raster.as_ref())
            .ok_or(Failure::Missing)?;
        STANDARD_NO_PAD_INDIFFERENT
            .decode(&text[raster.encoded.clone()])
            .map_err(|_| Failure::InvalidRecord)
    }

    pub fn tool(&self, index: usize, page: usize) -> Result<Page<Vec<ToolPart<'_>>>, Failure> {
        let entries = &self.snapshot.entries;
        let mut parts = Vec::new();
        match entries.get(index) {
            Some(Entry::Call { call_id, input, .. }) => {
                if let Some(input) = input {
                    parts.push((index, true, input.as_str()));
                }
                for result in call_id
                    .as_ref()
                    .and_then(|id| self.outputs.get(id))
                    .into_iter()
                    .flatten()
                {
                    if let Entry::Result { output, .. } = &entries[*result] {
                        parts.push((*result, false, output.as_str()));
                    }
                }
            }
            Some(Entry::Result { output, .. }) => parts.push((index, false, output.as_str())),
            _ => return Err(Failure::Missing),
        }
        let pages = self.tool_pages[index].get_or_init(|| {
            entry_pages(
                parts
                    .iter()
                    .enumerate()
                    .map(|(index, (_, _, text))| (index, *text)),
            )
        });
        Ok(Page {
            content: page_at(pages, page)?
                .iter()
                .map(|(index, range)| {
                    let (entry, command, text) = parts[*index];
                    ToolPart {
                        index: entry,
                        command,
                        text: &text[range.clone()],
                    }
                })
                .collect(),
            total: pages.len(),
        })
    }
}

fn text_without_images(text: &str, images: &[MessageImage]) -> String {
    let mut content = String::new();
    let mut start = 0;
    for image in images {
        content.push_str(&text[start..image.span.start]);
        start = image.span.end;
    }
    content.push_str(&text[start..]);
    if images[0].span.start == 0 && content.starts_with('\n') {
        content.remove(0);
    }
    content
}

fn page_at<T>(pages: &[T], page: usize) -> Result<&T, Failure> {
    pages.get(page).ok_or(Failure::Missing)
}

fn command_hint(text: &str) -> &str {
    let mut end = text.len().min(256);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].split('\n').next().unwrap_or_default()
}

fn text_ranges(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut next = Some(0usize);
    std::iter::from_fn(move || {
        let start = next?;
        let mut end = start.saturating_add(PAGE_BYTES).min(text.len());
        if end < text.len() {
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if let Some(line) = text.as_bytes()[start + PAGE_BYTES / 2..end]
                .iter()
                .rposition(|&byte| byte == b'\n')
            {
                end = start + PAGE_BYTES / 2 + line + 1;
            } else if text.as_bytes()[end - 1] == b'\r' && text.as_bytes()[end] == b'\n' {
                end -= 1;
            }
        }
        // One empty range also represents a blank source or empty message.
        next = (end < text.len()).then_some(end);
        Some(start..end)
    })
}

fn entry_pages<'a>(entries: impl Iterator<Item = (usize, &'a str)>) -> EntryPages {
    let mut pages = Vec::new();
    let mut rows = Vec::new();
    let mut bytes = 0;
    for (index, text) in entries {
        for range in text_ranges(text) {
            if range.start != 0 || rows.len() == PAGE_ROWS || bytes + range.len() > PAGE_BYTES {
                pages.push(std::mem::take(&mut rows));
                bytes = 0;
            }
            bytes += range.len();
            rows.push((index, range));
        }
    }
    if !rows.is_empty() || pages.is_empty() {
        pages.push(rows);
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Indexed, Origin, Record, Session, Source};
    use serde_json::json;

    #[test]
    fn conversation_shows_embedded_images_as_cards_without_changing_source() {
        let payload = vec![42; PAGE_BYTES * 3];
        let encoded = STANDARD_NO_PAD_INDIFFERENT.encode(&payload);
        let event = json!({"type":"response_item","payload":{
            "type":"message","role":"user","content":[
                {"type":"input_text","text":"<image name=[Image #1] path=\"/deleted/test.png\">"},
                {"type":"input_image","image_url":format!("data:image/png;base64,{encoded}")},
                {"type":"input_text","text":"</image>"},
                {"type":"input_text","text":"第一行\n第二行"},
                {"type":"input_image","image_url":"https://example.invalid/image.png"},
                {"type":"input_text","text":"后续正文"},
            ]
        }});
        let jsonl = format!("{event}\n");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("record.jsonl");
        std::fs::write(&file, &jsonl).unwrap();
        let preview = Preview::new(
            crate::transcript::read(Indexed {
                session: Session {
                    source: Source::Codex,
                    origin: Origin::Codex,
                    ..fixture_session()
                },
                record: Record::File(file),
            })
            .unwrap(),
        );

        let ViewPage::Conversation(page) = preview.view(View::Conversation, 0).unwrap() else {
            panic!("expected conversation")
        };
        assert_eq!(page.total, 1);
        let [Row::Message {
            text,
            index,
            images,
            ..
        }] = page.content.as_slice()
        else {
            panic!("expected one message")
        };
        assert_eq!(
            text.iter().map(|part| part.text).collect::<String>(),
            "第一行\n第二行\n后续正文"
        );
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].name, Some("Image #1"));
        assert_eq!(images[0].mime, Some("image/png"));
        assert_eq!(preview.image(*index, 0).unwrap(), payload);
        assert_eq!(images[1].mime, None);
        assert_eq!(preview.image(*index, 1), Err(Failure::Missing));
        assert_eq!(preview.source(Format::Jsonl), jsonl);
        let markdown = preview.source(Format::Md);
        assert!(markdown.contains(&format!("data:image/png;base64,{encoded}")));
        assert!(markdown.contains("<image name=[Image #1] path=\"/deleted/test.png\">"));
    }

    fn fixture_session() -> Session {
        Session {
            key: "fixture".into(),
            id: "fixture".into(),
            source: Source::Claude,
            origin: Origin::ClaudeDesktop,
            title: "原文\r\n标题".into(),
            project: Some("/deleted/fixture".into()),
            project_exists: false,
            repository: None,
            worktree: None,
            worktree_repository: None,
            updated_at: 1_700_000_000_000,
        }
    }

    #[test]
    fn pages_preserve_large_messages_tool_results_and_both_exports() {
        let message = format!(
            "{}\r\n{}",
            "a".repeat(PAGE_BYTES - 1),
            "你好🙂".repeat(3_500)
        );
        let output = format!("{}\n", "工具输出<&>🙂```".repeat(230_000));
        let jsonl = serde_json::json!({"output": &output}).to_string() + "\n";
        let preview = Preview::new(Snapshot {
            session: fixture_session(),
            entries: vec![
                Entry::Message {
                    role: "user",
                    text: message.clone(),
                    time: None,
                    images: vec![],
                },
                Entry::Call {
                    name: "Bash".into(),
                    call_id: Some("a".into()),
                    input: Some("cat <<EOF\n```\nEOF".into()),
                    time: None,
                },
                Entry::Result {
                    name: None,
                    call_id: Some("a".into()),
                    output: output.clone(),
                    time: None,
                },
                Entry::Message {
                    role: "assistant",
                    text: "结束".into(),
                    time: Some("2026-11-19T00:01:00+08:00".into()),
                    images: vec![],
                },
            ],
            jsonl: jsonl.clone(),
        });

        let markdown = format!(
            "# 原文 标题\n\n- Tool: Claude Code (Desktop)\n- Session: fixture\n- Project: /deleted/fixture\n- Updated: 2023-11-14T22:13:20.000Z\n\n## User\n\n{message}\n\n### Tool call · Bash (a)\n\n````\ncat <<EOF\n```\nEOF\n````\n\n### Tool result · Bash (a)\n\n````\n{output}````\n\n## Assistant · 2026-11-19T00:01:00+08:00\n\n结束\n"
        );
        for (view, format, full) in [
            (View::Md, Format::Md, &markdown),
            (View::Jsonl, Format::Jsonl, &jsonl),
        ] {
            assert_eq!(preview.source(format), full);
            let mut joined = String::new();
            for page in 0.. {
                let ViewPage::Source(page_data) = preview.view(view, page).unwrap() else {
                    panic!("expected source")
                };
                let page_text = page_data
                    .content
                    .iter()
                    .map(|part| part.text)
                    .collect::<String>();
                assert!(page_text.len() <= PAGE_BYTES);
                joined.push_str(&page_text);
                if page + 1 == page_data.total {
                    break;
                }
            }
            assert_eq!(joined, *full);
        }

        assert_eq!(preview.message_count, 2);
        let mut messages = String::new();
        let mut tools = Vec::new();
        for page in 0..preview.conversation.len() {
            let ViewPage::Conversation(page_data) = preview.view(View::Conversation, page).unwrap()
            else {
                panic!("expected conversation")
            };
            for row in page_data.content {
                match row {
                    Row::Message { text, .. } => messages.extend(text.iter().map(|part| part.text)),
                    Row::Tool { index, hint, .. } => tools.push((index, hint)),
                }
            }
        }
        assert_eq!(messages, message + "结束");
        assert_eq!(tools, [(1, Some("cat <<EOF"))]);

        assert_eq!(
            preview.code_block(CodeBlock::Tool { index: 2 }).unwrap(),
            output
        );
        assert!(preview
            .code_block(CodeBlock::Markdown { block: 1 })
            .unwrap()
            .contains(&output));
        let hits = preview
            .search(View::Conversation, "工具输出", None)
            .unwrap();
        assert!(serde_json::to_value(hits).unwrap()["hits"][0]["matched"] == "工具输出");

        let mut result = String::new();
        for page in 0.. {
            let page_data = preview.tool(1, page).unwrap();
            for part in page_data.content {
                if part.command {
                    assert_eq!(part.text, "cat <<EOF\n```\nEOF");
                } else {
                    result.push_str(part.text);
                }
            }
            if page + 1 == page_data.total {
                break;
            }
        }
        assert_eq!(result, output);
    }
}
