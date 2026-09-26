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
    /// Parses with the system time zone for calendar-day date filters.
    pub fn parse(input: &str) -> Self {
        Self::parse_at(input, local_utc_offset_seconds())
    }
    /// Parses with an explicit UTC offset (seconds east of UTC) for
    /// `created:YYYYMMDD` / `updated:YYYYMMDD..YYYYMMDD` / `day-N`.
    pub fn parse_at(input: &str, utc_offset_seconds: i64) -> Self {
        let mut query = Self::default();
        for (raw, quoted) in tokens(input) {
            let (negated, value) = raw
                .strip_prefix('-')
                .map_or((false, raw.as_str()), |v| (true, v));
            if let Some(filter) = parse_filter(value, utc_offset_seconds) {
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
fn parse_filter(value: &str, utc_offset_seconds: i64) -> Option<SearchFilter> {
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
        // Spec syntax `is:trash`; Evernote's own `intrash:` prefix.
        "is" if raw.eq_ignore_ascii_case("trash") => SearchFilter::Trash(true),
        "intrash" => SearchFilter::Trash(!raw.eq_ignore_ascii_case("false")),
        // Spec `has:attachment`; Evernote `contains:attachment`.
        "has" | "contains" if raw.eq_ignore_ascii_case("attachment") => {
            SearchFilter::HasAttachment(true)
        }
        "hasattachment" => match raw {
            "true" => SearchFilter::HasAttachment(true),
            "false" => SearchFilter::HasAttachment(false),
            _ => return None,
        },
        "created" => match date_range(raw, utc_offset_seconds) {
            Some(range) => SearchFilter::Created(range),
            None => return None,
        },
        "updated" => match date_range(raw, utc_offset_seconds) {
            Some(range) => SearchFilter::Updated(range),
            None => return None,
        },
        "filename" => SearchFilter::Filename(raw.into()),
        "mime" => SearchFilter::Mime(raw.into()),
        _ => return None,
    };
    Some(filter)
}
/// Start of a local calendar day in epoch milliseconds, for `YYYYMMDD`.
fn local_day_start(raw: &str, utc_offset_seconds: i64) -> Option<i64> {
    if raw.len() != 8 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, month, day): (i64, i64, i64) = (
        raw[..4].parse().ok()?,
        raw[4..6].parse().ok()?,
        raw[6..].parse().ok()?,
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if year < 1 || !(1..=days_in_month).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 - utc_offset_seconds) * 1000)
}

/// One bound of a date range: `YYYYMMDD` (local day), `day-N` (start of
/// the local day N days ago) or raw epoch milliseconds. `end` selects the
/// last millisecond of a calendar day.
fn date_bound(raw: &str, utc_offset_seconds: i64, end: bool) -> Option<i64> {
    const DAY_MS: i64 = 86_400_000;
    if let Some(start) = local_day_start(raw, utc_offset_seconds) {
        return Some(if end { start + DAY_MS - 1 } else { start });
    }
    if let Some(days) = raw.strip_prefix("day-").and_then(|n| n.parse::<i64>().ok()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as i64;
        let offset_ms = utc_offset_seconds * 1000;
        let today = (now + offset_ms).div_euclid(DAY_MS) * DAY_MS - offset_ms;
        let start = today - days * DAY_MS;
        return Some(if end { start + DAY_MS - 1 } else { start });
    }
    raw.parse().ok()
}

fn date_range(raw: &str, utc_offset_seconds: i64) -> Option<DateRange> {
    if let Some((start, end)) = raw.split_once("..") {
        let start = if start.is_empty() {
            Some(None)
        } else {
            date_bound(start, utc_offset_seconds, false).map(Some)
        }?;
        let end = if end.is_empty() {
            Some(None)
        } else {
            date_bound(end, utc_offset_seconds, true).map(Some)
        }?;
        if start.is_none() && end.is_none() || matches!((start, end), (Some(a), Some(b)) if a > b) {
            return None;
        }
        return Some(DateRange { start, end });
    }
    if raw.starts_with("day-") {
        // Evernote `created:day-7`: from that day onward.
        return Some(DateRange {
            start: Some(date_bound(raw, utc_offset_seconds, false)?),
            end: None,
        });
    }
    Some(DateRange {
        start: Some(date_bound(raw, utc_offset_seconds, false)?),
        end: Some(date_bound(raw, utc_offset_seconds, true)?),
    })
}

fn local_utc_offset_seconds() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or_default();
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes only into the provided struct.
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i64
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
