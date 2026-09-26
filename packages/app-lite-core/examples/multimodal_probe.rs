//! Multimodal acceptance probe.
//!
//! `multimodal_probe seed <profile> <png> <pdf>`: new library with one note
//! per attachment kind (screenshot, PDF, audio, video, plain file).
//! `multimodal_probe status <profile>`: derived-text job states.
//! `multimodal_probe search <profile> <query>`: matching note titles.

use std::path::Path;

use app_lite_core::document::{Block, BlockStyle, Inline};
use app_lite_core::{CanonicalDocument, CreateNote, LibraryRepository, SearchQuery};

fn note(repo: &LibraryRepository, title: &str, bytes: &[u8], name: &str, mime: &str, ext: &str) {
    let id = repo.import_resource(bytes, name, mime, ext).unwrap();
    let block = if mime.starts_with("image/") {
        Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Image {
                resource_id: id,
                alt: name.into(),
                display_width: None,
                link: None,
            }],
        }
    } else {
        Block::Attachment {
            resource_id: id,
            filename: name.into(),
            media_type: mime.into(),
        }
    };
    repo.create_note(CreateNote {
        title: title.into(),
        notebook_id: None,
        document: CanonicalDocument::from_blocks(vec![block]),
    })
    .unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let profile = Path::new(&args[1]);
    let db = profile.join("library.sqlite");
    match args[0].as_str() {
        "seed" => {
            std::fs::create_dir_all(profile).unwrap();
            let repo = LibraryRepository::open(&db).unwrap();
            let png = std::fs::read(&args[2]).unwrap();
            let pdf = std::fs::read(&args[3]).unwrap();
            note(
                &repo,
                "截图笔记",
                &png,
                "screenshot.png",
                "image/png",
                "png",
            );
            note(
                &repo,
                "PDF 笔记",
                &pdf,
                "report.pdf",
                "application/pdf",
                "pdf",
            );
            note(
                &repo,
                "音频笔记",
                b"ID3 fake audio",
                "memo.mp3",
                "audio/mpeg",
                "mp3",
            );
            note(
                &repo,
                "视频笔记",
                b"\0\0\0\x18ftypmp42",
                "clip.mp4",
                "video/mp4",
                "mp4",
            );
            note(
                &repo,
                "文件笔记",
                b"plain text file",
                "notes.txt",
                "text/plain",
                "txt",
            );
            println!("seeded {}", profile.display());
        }
        "status" => {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let mut stmt = conn
                .prepare("SELECT state, COALESCE(failure,''), COUNT(*) FROM derived_text_jobs GROUP BY 1,2")
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .unwrap();
            for row in rows {
                println!("{:?}", row.unwrap());
            }
        }
        "search" => {
            let repo = LibraryRepository::open(&db).unwrap();
            repo.process_search_jobs().unwrap();
            for hit in repo.search(SearchQuery::parse(&args[2])).unwrap() {
                println!(
                    "{} | matched_resource={}",
                    hit.note.title_prefix,
                    hit.matched_resource.is_some()
                );
            }
        }
        other => panic!("unknown command {other}"),
    }
}
