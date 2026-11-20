use serde::Serialize;
use std::ops::Range;

#[derive(Debug)]
pub struct Fence {
    span: Range<usize>,
    pub body: Range<usize>,
}

pub fn blocks(text: &str) -> Vec<Fence> {
    let mut blocks = Vec::new();
    let mut open = None;
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        let line = line.trim_end_matches(['\n', '\r']);
        let marker = line.trim_start_matches(' ');
        if line.len() - marker.len() <= 3 && marker.starts_with(['`', '~']) {
            let byte = marker.as_bytes()[0];
            let count = marker.bytes().take_while(|value| *value == byte).count();
            let rest = &marker[count..];
            if let Some((kind, width, opening, body)) = open {
                if byte == kind && count >= width && rest.trim_matches([' ', '\t']).is_empty() {
                    blocks.push(Fence {
                        span: opening..end,
                        body: body..start,
                    });
                    open = None;
                }
            } else if count >= 3 && (byte != b'`' || !rest.contains('`')) {
                open = Some((byte, count, start, end));
            }
        }
        start = end;
    }
    if let Some((_, _, opening, body)) = open {
        blocks.push(Fence {
            span: opening..text.len(),
            body: body..text.len(),
        });
    }
    blocks
}

#[derive(Serialize)]
pub struct TextSection<'a> {
    pub text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<usize>,
}

pub fn sections<'a>(text: &'a str, range: Range<usize>, fences: &[Fence]) -> Vec<TextSection<'a>> {
    let mut parts = Vec::new();
    let mut start = range.start;
    for (block, fence) in fences
        .iter()
        .enumerate()
        .skip(fences.partition_point(|f| f.span.end <= start))
    {
        if fence.span.start >= range.end {
            break;
        }
        if start < fence.span.start {
            parts.push(TextSection {
                text: &text[start..fence.span.start],
                block: None,
            });
            start = fence.span.start;
        }
        let end = fence.span.end.min(range.end);
        parts.push(TextSection {
            text: &text[start..end],
            block: Some(block),
        });
        start = end;
    }
    if start < range.end {
        parts.push(TextSection {
            text: &text[start..range.end],
            block: None,
        });
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_keep_exact_source_and_copy_the_complete_body() {
        let text = "intro\n````md\n```\n中文\n````\n~~~\nlast";
        let blocks = blocks(text);
        assert_eq!(&text[blocks[0].body.clone()], "```\n中文\n");
        assert_eq!(&text[blocks[1].body.clone()], "last");
        let split = text.find("中文").unwrap();
        let left = sections(text, 0..split, &blocks);
        let right = sections(text, split..text.len(), &blocks);
        assert_eq!(left.last().unwrap().block, right[0].block);
        assert_eq!(
            left.iter()
                .chain(&right)
                .map(|part| part.text)
                .collect::<String>(),
            text
        );
    }
}
