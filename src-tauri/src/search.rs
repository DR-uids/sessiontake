use crate::catalog::Failure;
use regex::{escape, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::ops::Range;

const PAGE_MATCHES: usize = 40;

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub struct Cursor {
    entry: usize,
    offset: usize,
}

#[derive(Serialize)]
pub struct Hit<'a> {
    kind: &'static str,
    before: &'a str,
    matched: &'a str,
    after: &'a str,
}

#[derive(Serialize)]
pub struct Results<'a> {
    hits: Vec<Hit<'a>>,
    next: Option<Cursor>,
}

pub fn find<'a>(
    query: &str,
    cursor: Option<Cursor>,
    texts: impl Iterator<Item = Option<(&'static str, &'a str)>>,
) -> Result<Results<'a>, Failure> {
    let query = query.trim();
    let mut hits = Vec::new();
    if query.is_empty() || query.len() > 1024 {
        return Ok(Results { hits, next: None });
    }
    let regex = RegexBuilder::new(&escape(query))
        .case_insensitive(true)
        .build()
        .map_err(|_| Failure::InvalidRecord)?;
    let cursor = cursor.unwrap_or_default();
    for (entry, text) in texts.enumerate().skip(cursor.entry) {
        let Some((kind, text)) = text else { continue };
        let offset = if entry == cursor.entry {
            cursor.offset
        } else {
            0
        };
        if !text.is_char_boundary(offset) {
            continue;
        }
        for matched in regex.find_iter(&text[offset..]) {
            let range = offset + matched.start()..offset + matched.end();
            if hits.len() == PAGE_MATCHES {
                return Ok(Results {
                    hits,
                    next: Some(Cursor {
                        entry,
                        offset: range.start,
                    }),
                });
            }
            hits.push(excerpt(kind, text, range));
        }
    }
    Ok(Results { hits, next: None })
}

fn excerpt<'a>(kind: &'static str, text: &'a str, range: Range<usize>) -> Hit<'a> {
    let mut start = range.start.saturating_sub(96);
    let mut end = text.len().min(range.end + 120);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Hit {
        kind,
        before: &text[start..range.start],
        matched: &text[range.clone()],
        after: &text[range.end..end],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_search_is_unicode_safe_and_resumes_without_skipping() {
        let text = "🙂 å[1]\n".repeat(PAGE_MATCHES + 2);
        let texts = || [Some(("source", text.as_str()))].into_iter();
        let first = find("Å[1]", None, texts()).unwrap();
        assert_eq!(first.hits.len(), PAGE_MATCHES);
        assert_eq!(first.hits[0].matched, "å[1]");
        let next = find("Å[1]", first.next, texts()).unwrap();
        assert_eq!(next.hits.len(), 2);
        assert!(next.next.is_none());
    }
}
