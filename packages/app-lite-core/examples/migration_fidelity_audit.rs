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
                    }
                    *feature_sets
                        .entry(features.into_iter().collect())
                        .or_default() += 1;
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
        for (reason, kinds) in kinds_by_reason {
            println!("kinds\t{reason}\t{kinds:?}");
        }
    }
    Ok(())
}
