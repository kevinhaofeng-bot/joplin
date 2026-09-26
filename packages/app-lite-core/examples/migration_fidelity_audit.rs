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
                *failures.entry(error.reason.into()).or_default() += 1;
                // Structural categories only, never source text or titles.
                if markup == 1 {
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
    Ok(())
}
