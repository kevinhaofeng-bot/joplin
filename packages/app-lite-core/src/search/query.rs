use crate::{NoteProjection, ResourceId};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRange {
    pub start: Option<i64>,
    pub end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchFilter {
    Notebook(String),
    Stack(String),
    Tag(String),
    Created(DateRange),
    Updated(DateRange),
    Trash(bool),
    HasAttachment(bool),
    Filename(String),
    Mime(String),
    Not(Box<SearchFilter>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTerm {
    Text(String),
    Phrase(String),
    NegatedText(String),
    NegatedPhrase(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub terms: Vec<SearchTerm>,
    pub filters: Vec<SearchFilter>,
    offset: usize,
    limit: usize,
}
impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            terms: vec![],
            filters: vec![],
            offset: 0,
            limit: 50,
        }
    }
}
impl SearchQuery {
    pub const MAX_PAGE_SIZE: usize = 500;
    pub fn parse(input: &str) -> Self {
        let mut query = Self::default();
        for (raw, quoted) in tokens(input) {
            let (negated, value) = raw
                .strip_prefix('-')
                .map_or((false, raw.as_str()), |v| (true, v));
            if let Some(filter) = parse_filter(value) {
                if !negated || matches!(filter, SearchFilter::Tag(_) | SearchFilter::Notebook(_)) {
                    query.filters.push(if negated {
                        SearchFilter::Not(Box::new(filter))
                    } else {
                        filter
                    });
                    continue;
                }
            }
            let term = match (quoted, negated) {
                (true, true) => SearchTerm::NegatedPhrase(value.into()),
                (true, false) => SearchTerm::Phrase(value.into()),
                (false, true) => SearchTerm::NegatedText(value.into()),
                (false, false) => SearchTerm::Text(value.into()),
            };
            if !matches!(&term, SearchTerm::Text(v) | SearchTerm::Phrase(v) | SearchTerm::NegatedText(v) | SearchTerm::NegatedPhrase(v) if v.is_empty())
            {
                query.terms.push(term);
            }
        }
        query
    }
    pub fn set_page(&mut self, offset: usize, limit: usize) -> Result<(), SearchQueryError> {
        if limit == 0 || limit > Self::MAX_PAGE_SIZE {
            return Err(SearchQueryError::PageLimitOutOfRange { limit });
        }
        self.offset = offset;
        self.limit = limit;
        Ok(())
    }
    pub const fn offset(&self) -> usize {
        self.offset
    }
    pub const fn limit(&self) -> usize {
        self.limit
    }
}
fn tokens(input: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    let mut current = String::new();
    let mut quoted = false;
    let mut token_quoted = false;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            token_quoted = true;
            quoted = !quoted;
            continue;
        }
        if ch.is_whitespace() && !quoted {
            if !current.is_empty() {
                out.push((std::mem::take(&mut current), token_quoted));
                token_quoted = false;
            }
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        out.push((current, token_quoted || quoted));
    }
    out
}
fn parse_filter(value: &str) -> Option<SearchFilter> {
    let Some((name, raw)) = value.split_once(':') else {
        return None;
    };
    if raw.is_empty() {
        return None;
    }
    let filter = match name.to_ascii_lowercase().as_str() {
        "notebook" => SearchFilter::Notebook(raw.into()),
        "stack" => SearchFilter::Stack(raw.into()),
        "tag" => SearchFilter::Tag(raw.into()),
        "trash" => match raw {
            "true" => SearchFilter::Trash(true),
            "false" => SearchFilter::Trash(false),
            _ => return None,
        },
        "hasattachment" => match raw {
            "true" => SearchFilter::HasAttachment(true),
            "false" => SearchFilter::HasAttachment(false),
            _ => return None,
        },
        "created" => match date_range(raw) {
            Some(range) => SearchFilter::Created(range),
            None => return None,
        },
        "updated" => match date_range(raw) {
            Some(range) => SearchFilter::Updated(range),
            None => return None,
        },
        "filename" => SearchFilter::Filename(raw.into()),
        "mime" => SearchFilter::Mime(raw.into()),
        _ => return None,
    };
    Some(filter)
}
fn date_range(raw: &str) -> Option<DateRange> {
    if let Some((start, end)) = raw.split_once("..") {
        let start = if start.is_empty() {
            Some(None)
        } else {
            start.parse().ok().map(Some)
        }?;
        let end = if end.is_empty() {
            Some(None)
        } else {
            end.parse().ok().map(Some)
        }?;
        if start.is_none() && end.is_none() || matches!((start, end), (Some(a), Some(b)) if a > b) {
            return None;
        }
        return Some(DateRange { start, end });
    }
    raw.parse().ok().map(|value| DateRange {
        start: Some(value),
        end: Some(value),
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub note: NoteProjection,
    pub snippet: String,
    pub matched_resource: Option<ResourceId>,
}
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SearchQueryError {
    #[error("search page limit {limit} is outside 1..=500")]
    PageLimitOutOfRange { limit: usize },
    #[error("search offset exceeds SQLite's signed range")]
    SqlIntegerOverflow,
}
