//! Read-only re-conversion of retained import source bytes; emits counts only.
use app_lite_core::{JexVerifiedResource, ResourceId, convert_jex_note_body};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("expected imported library.sqlite")?;
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut resources = BTreeMap::new();
    let mut stmt = db.prepare("SELECT a.source_id,r.id,r.mime,r.title FROM jex_stage_resource_audit a JOIN resources r ON r.id=a.resource_id")?;
    for row in stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (source, id, mime, filename) = row?;
        resources.insert(
            source,
            JexVerifiedResource {
                destination_id: ResourceId::new(id)?,
                mime,
                filename,
            },
        );
    }
    let mut stmt = db.prepare(
        "SELECT source_id,source_path,markup_language,raw_body_bytes FROM jex_stage_note_audit",
    )?;
    let mut failures = BTreeMap::<String, usize>::new();
    let mut unsupported = BTreeMap::<String, usize>::new();
    let mut kinds_by_reason = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut feature_sets = BTreeMap::<Vec<&'static str>, usize>::new();
    let mut html_tags = BTreeMap::<String, usize>::new();
    let mut unflagged_reasons = BTreeMap::<String, usize>::new();
    let mut html_block_shapes = BTreeMap::<String, usize>::new();
    let mut passed = 0;
    for row in stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })? {
        let (id, path, markup, bytes) = row?;
        let body = std::str::from_utf8(&bytes)?;
        match convert_jex_note_body(&id, &path, markup, body, &resources) {
            Ok(_) => passed += 1,
            Err(error) => {
                let reason: String = error.reason.into();
                if std::env::var_os("JOPLIN_LITE_AUDIT_HTML_SKELETON").is_some()
                    && reason == "Unclosed HTML element"
                {
                    // Tag names only; text and attribute values are dropped.
                    let mut skeleton = String::new();
                    for event in
                        pulldown_cmark::Parser::new_ext(body, pulldown_cmark::Options::all())
                    {
                        match event {
                            pulldown_cmark::Event::Html(html)
                            | pulldown_cmark::Event::InlineHtml(html) => {
                                for part in html.split('<').skip(1) {
                                    let closing = part.starts_with('/');
                                    let name: String = part
                                        .trim_start_matches('/')
                                        .chars()
                                        .take_while(|c| c.is_ascii_alphanumeric())
                                        .collect();
                                    let attrs =
                                        part.split('>').next().is_some_and(|tag| tag.contains('='));
                                    skeleton.push_str(&format!(
                                        "<{}{}{}>",
                                        if closing { "/" } else { "" },
                                        name.to_ascii_lowercase(),
                                        if attrs { " …" } else { "" }
                                    ));
                                }
                            }
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock) => {
                                skeleton.push('[')
                            }
                            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::HtmlBlock) => {
                                skeleton.push(']')
                            }
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Paragraph) => {
                                skeleton.push('P')
                            }
                            _ => {}
                        }
                    }
                    println!(
                        "skeleton\t{}",
                        skeleton.chars().take(400).collect::<String>()
                    );
                }
                *failures.entry(reason.clone()).or_default() += 1;
                // Structural categories only, never source text or titles.
                if markup == 1 {
                    let mut features = std::collections::BTreeSet::new();
                    let mut in_link = false;
                    for event in
                        pulldown_cmark::Parser::new_ext(body, pulldown_cmark::Options::all())
                    {
                        use pulldown_cmark::{Event as E, Tag as T, TagEnd};
                        let feature = match &event {
                            E::Start(T::Table(_)) => Some("table"),
                            E::InlineMath(_) | E::DisplayMath(_) => Some("math"),
                            E::Html(_) | E::InlineHtml(_) => Some("html"),
                            E::FootnoteReference(_) | E::Start(T::FootnoteDefinition(_)) => {
                                Some("footnote")
                            }
                            E::Start(T::Heading { level, .. }) if *level as usize > 3 => {
                                Some("heading4plus")
                            }
                            E::Start(T::List(Some(start))) if *start != 1 => Some("list_start"),
                            E::Start(T::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(lang)))
                                if !lang.is_empty() =>
                            {
                                Some("code_language")
                            }
                            E::Start(T::Image { dest_url, .. }) if in_link => {
                                let _ = dest_url;
                                Some("linked_image")
                            }
                            E::Start(T::Image { dest_url, .. }) if !dest_url.starts_with(":/") => {
                                Some("external_image")
                            }
                            E::Start(T::Link { dest_url, .. }) if dest_url.starts_with(":/") => {
                                Some("resource_link")
                            }
                            E::Start(T::Link { title, .. }) | E::Start(T::Image { title, .. })
                                if !title.is_empty() =>
                            {
                                Some("title_attr")
                            }
                            _ => None,
                        };
                        match &event {
                            E::Start(T::Link { .. }) => in_link = true,
                            E::End(TagEnd::Link) => in_link = false,
                            _ => {}
                        }
                        if let Some(feature) = feature {
                            features.insert(feature);
                        }
                        if let E::Html(html) | E::InlineHtml(html) = &event {
                            // Tag names only, never attribute values or text.
                            for part in html.split('<').skip(1) {
                                let name: String = part
                                    .trim_start_matches('/')
                                    .chars()
                                    .take_while(|c| c.is_ascii_alphanumeric())
                                    .collect::<String>()
                                    .to_ascii_lowercase();
                                let key = if name.is_empty() {
                                    "<other>".into()
                                } else {
                                    name
                                };
                                *html_tags.entry(key).or_default() += 1;
                            }
                        }
                    }
                    if features.is_empty() {
                        *unflagged_reasons.entry(reason.clone()).or_default() += 1;
                    }
                    *feature_sets
                        .entry(features.into_iter().collect())
                        .or_default() += 1;
                    // Shape of each HTML block: its first tag and whether it is
                    // a lone opening/closing wrapper line. Never text.
                    let mut block = None::<String>;
                    for event in
                        pulldown_cmark::Parser::new_ext(body, pulldown_cmark::Options::all())
                    {
                        match event {
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock) => {
                                block = Some(String::new())
                            }
                            pulldown_cmark::Event::Html(html) => {
                                if let Some(block) = block.as_mut() {
                                    block.push_str(&html);
                                }
                            }
                            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::HtmlBlock) => {
                                let html = block.take().unwrap_or_default();
                                let trimmed = html.trim();
                                let closing = trimmed.starts_with("</");
                                let name: String = trimmed
                                    .trim_start_matches('<')
                                    .trim_start_matches('/')
                                    .chars()
                                    .take_while(|c| c.is_ascii_alphanumeric())
                                    .collect::<String>()
                                    .to_ascii_lowercase();
                                let lone = trimmed.matches('<').count() == 1;
                                let attrs = trimmed.contains('=');
                                let shape = match (lone, closing) {
                                    (true, true) => format!("lone_close:{name}"),
                                    (true, false) => format!(
                                        "lone_open:{name}{}",
                                        if attrs { "+attrs" } else { "" }
                                    ),
                                    _ => format!(
                                        "fragment:{name}{}",
                                        if attrs { "+attrs" } else { "" }
                                    ),
                                };
                                *html_block_shapes.entry(shape).or_default() += 1;
                            }
                            _ => {}
                        }
                    }
                    let mut events =
                        pulldown_cmark::Parser::new_ext(body, pulldown_cmark::Options::all())
                            .peekable();
                    while let Some(event) = events.next() {
                        if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link {
                            dest_url,
                            ..
                        }) = &event
                        {
                            if let Some(pulldown_cmark::Event::Start(
                                pulldown_cmark::Tag::Image {
                                    dest_url: image, ..
                                },
                            )) = events.peek()
                            {
                                let category = if dest_url == image {
                                    "self_linked_image"
                                } else {
                                    "different_linked_image"
                                };
                                *unsupported.entry(category.into()).or_default() += 1;
                            }
                        }
                        // Variant names only (e.g. Table, FootnoteReference, InlineHtml).
                        let kind = match &event {
                            pulldown_cmark::Event::Start(tag) => Some(format!("{tag:?}")),
                            pulldown_cmark::Event::Html(_) => Some("Html".into()),
                            pulldown_cmark::Event::InlineHtml(_) => Some("InlineHtml".into()),
                            pulldown_cmark::Event::FootnoteReference(_) => {
                                Some("FootnoteReference".into())
                            }
                            pulldown_cmark::Event::InlineMath(_) => Some("InlineMath".into()),
                            pulldown_cmark::Event::DisplayMath(_) => Some("DisplayMath".into()),
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            let name = kind
                                .split(|c: char| !c.is_alphanumeric())
                                .next()
                                .unwrap_or_default()
                                .to_owned();
                            *kinds_by_reason
                                .entry(reason.clone())
                                .or_default()
                                .entry(name)
                                .or_default() += 1;
                        }
                        let category = match event {
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Table(_)) => {
                                Some("table")
                            }
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(_)) => {
                                Some("code")
                            }
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::BlockQuote(_)) => {
                                Some("quote")
                            }
                            _ => None,
                        };
                        if let Some(category) = category {
                            *unsupported.entry(category.into()).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    if std::env::var_os("JOPLIN_LITE_AUDIT_TABLES").is_some() {
        table_inventory(&db)?;
    }
    println!(
        "strict_passed={passed} warnings={}",
        failures.values().sum::<usize>()
    );
    for (reason, count) in failures {
        println!("{count}\t{reason}");
    }
    println!("structures_in_warning_notes={unsupported:?}");
    if std::env::var_os("JOPLIN_LITE_AUDIT_KINDS").is_some() {
        let mut sets: Vec<_> = feature_sets.into_iter().collect();
        sets.sort_by(|a, b| b.1.cmp(&a.1));
        for (set, count) in sets {
            println!("features\t{count}\t{set:?}");
        }
        println!("html_tags\t{html_tags:?}");
        println!("unflagged_reasons\t{unflagged_reasons:?}");
        println!("html_block_shapes\t{html_block_shapes:?}");
        for (reason, kinds) in kinds_by_reason {
            println!("kinds\t{reason}\t{kinds:?}");
        }
    }
    Ok(())
}

/// Structural inventory of Markdown tables across all notes: counts only.
fn table_inventory(db: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    use pulldown_cmark::{Alignment, Event as E, Options, Parser, Tag as T, TagEnd};
    let mut notes_with_tables = 0;
    let mut tables = 0;
    let mut columns = BTreeMap::<usize, usize>::new();
    let mut rows = BTreeMap::<&'static str, usize>::new();
    let mut alignments = BTreeMap::<&'static str, usize>::new();
    let mut in_cell = BTreeMap::<String, usize>::new();
    let mut statement = db.prepare("SELECT raw_body_bytes FROM jex_stage_note_audit")?;
    for bytes in statement.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
        let bytes = bytes?;
        let body = std::str::from_utf8(&bytes)?;
        let mut note_has_table = false;
        let mut cell = false;
        let mut row_count = 0usize;
        for event in Parser::new_ext(body, Options::all()) {
            match &event {
                E::Start(T::Table(aligns)) => {
                    note_has_table = true;
                    tables += 1;
                    row_count = 0;
                    *columns.entry(aligns.len().min(12)).or_default() += 1;
                    for align in aligns {
                        *alignments
                            .entry(match align {
                                Alignment::None => "none",
                                Alignment::Left => "left",
                                Alignment::Center => "center",
                                Alignment::Right => "right",
                            })
                            .or_default() += 1;
                    }
                }
                E::End(TagEnd::Table) => {
                    let bucket = match row_count {
                        0..=5 => "1-5",
                        6..=20 => "6-20",
                        21..=100 => "21-100",
                        _ => ">100",
                    };
                    *rows.entry(bucket).or_default() += 1;
                }
                E::Start(T::TableRow) | E::Start(T::TableHead) => row_count += 1,
                E::Start(T::TableCell) => cell = true,
                E::End(TagEnd::TableCell) => cell = false,
                _ => {}
            }
            if cell {
                let kind = match &event {
                    E::Start(tag) => format!("{tag:?}"),
                    E::InlineHtml(html) => format!(
                        "InlineHtml<{}>",
                        html.trim_start_matches('<')
                            .trim_start_matches('/')
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric())
                            .collect::<String>()
                            .to_ascii_lowercase()
                    ),
                    E::Code(_) => "Code".into(),
                    E::InlineMath(_) => "InlineMath".into(),
                    E::SoftBreak | E::HardBreak => "Break".into(),
                    _ => continue,
                };
                let name = kind
                    .split(|c: char| c == '(' || c == ' ' || c == '{')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                *in_cell.entry(name).or_default() += 1;
            }
        }
        if note_has_table {
            notes_with_tables += 1;
        }
    }
    table_source_checks(db)?;
    println!("tables notes={notes_with_tables} tables={tables}");
    println!("tables columns(capped 12)={columns:?}");
    println!("tables rows={rows:?}");
    println!("tables column_alignments={alignments:?}");
    println!("tables in_cell={in_cell:?}");
    Ok(())
}

/// Source-level checks the table model depends on: GFM rows wider than the
/// header (the parser drops extra cells), escaped pipes, HTML entities, cell
/// image targets, size limits, and raw HTML tables with spans, nesting or
/// block content. Counts only.
fn table_source_checks(db: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    use pulldown_cmark::{Event as E, Options, Parser, Tag as T, TagEnd};
    let mut counts = BTreeMap::<&'static str, usize>::new();
    let mut add = |key: &'static str, value: usize| *counts.entry(key).or_default() += value;
    let mut statement = db.prepare("SELECT raw_body_bytes FROM jex_stage_note_audit")?;
    for bytes in statement.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
        let bytes = bytes?;
        let body = std::str::from_utf8(&bytes)?;
        let mut in_cell = false;
        let mut rows = 0usize;
        let mut cells = 0usize;
        let mut header_cells = 0usize;
        let mut row = 0..0;
        let mut html = String::new();
        for (event, range) in Parser::new_ext(body, Options::all()).into_offset_iter() {
            match &event {
                E::Start(T::Table(aligns)) => {
                    rows = 0;
                    if aligns.len() > 64 {
                        add("md_table_over_64_columns", 1);
                    }
                    let source = &body[range.clone()];
                    add("md_escaped_pipes", source.matches("\\|").count());
                    let entity = |text: &str| {
                        text.split('&')
                            .skip(1)
                            .filter(|rest| {
                                let name: String = rest
                                    .chars()
                                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '#')
                                    .collect();
                                !name.is_empty() && rest[name.len()..].starts_with(';')
                            })
                            .count()
                    };
                    add("md_table_html_entities", entity(source));
                }
                E::Start(T::TableHead) | E::Start(T::TableRow) => {
                    rows += 1;
                    cells = 0;
                    row = range.clone();
                }
                E::End(TagEnd::TableHead) => header_cells = cells,
                E::End(TagEnd::TableRow) => {
                    let lost = nonempty_cells_beyond(&body[row.clone()], header_cells);
                    if lost > 0 {
                        add("md_rows_losing_nonempty_cells_beyond_header", 1);
                    }
                }
                E::End(TagEnd::Table) if rows > 1000 => add("md_table_over_1000_rows", 1),
                E::Start(T::TableCell) => {
                    in_cell = true;
                    cells += 1;
                }
                E::End(TagEnd::TableCell) => in_cell = false,
                E::Start(T::Image { dest_url, .. }) if in_cell => {
                    if dest_url.starts_with(":/") {
                        add("md_cell_images_resource", 1);
                    } else {
                        add("md_cell_images_external", 1);
                    }
                }
                E::Html(text) | E::InlineHtml(text) => html.push_str(text),
                _ => {}
            }
        }
        let lower = html.to_ascii_lowercase();
        let html_tables = lower.matches("<table").count();
        add("html_tables", html_tables);
        if html_tables > 0 {
            add("html_table_colspan", lower.matches("colspan").count());
            add("html_table_rowspan", lower.matches("rowspan").count());
            let mut depth = 0usize;
            let mut in_cell_depth = 0usize;
            let mut rest = lower.as_str();
            while let Some(start) = rest.find('<') {
                rest = &rest[start + 1..];
                let closing = rest.starts_with('/');
                let name: String = rest
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                match (name.as_str(), closing) {
                    ("table", false) => {
                        if depth > 0 {
                            add("html_nested_tables", 1);
                        }
                        depth += 1;
                    }
                    ("table", true) => depth = depth.saturating_sub(1),
                    ("td" | "th", false) => in_cell_depth = depth,
                    ("td" | "th", true) => in_cell_depth = 0,
                    (
                        "p" | "div" | "ul" | "ol" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                        | "blockquote" | "pre",
                        false,
                    ) if in_cell_depth > 0 => add("html_cell_block_elements", 1),
                    _ => {}
                }
            }
        }
    }
    for (key, value) in counts {
        println!("table_check {key}={value}");
    }
    Ok(())
}

/// GFM pads short rows but drops cells beyond the header. Re-parse the row
/// under an 80-column header and count non-empty cells past `header`.
fn nonempty_cells_beyond(row: &str, header: usize) -> usize {
    use pulldown_cmark::{Event as E, Options, Parser, Tag as T, TagEnd};
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = row.strip_suffix('|').unwrap_or(row);
    let wide = format!("|{}|\n|{}|\n|{}|\n", "a|".repeat(80), "-|".repeat(80), row);
    let (mut in_body, mut in_cell, mut index, mut lost) = (false, false, 0usize, 0usize);
    let mut visible = false;
    for event in Parser::new_ext(&wide, Options::all()) {
        match event {
            E::End(TagEnd::TableHead) => in_body = true,
            E::Start(T::TableCell) if in_body => {
                in_cell = true;
                visible = false;
            }
            E::End(TagEnd::TableCell) if in_body => {
                in_cell = false;
                index += 1;
                if index > header && visible {
                    lost += 1;
                }
            }
            E::Text(text) | E::Code(text) if in_cell => visible |= !text.trim().is_empty(),
            E::Start(T::Image { .. }) | E::Start(T::Link { .. }) if in_cell => visible = true,
            _ => {}
        }
    }
    lost
}
