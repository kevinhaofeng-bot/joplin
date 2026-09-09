use std::io::Cursor;

use gpui::ImageFormat;
use image::{ImageBuffer, Rgba};

use super::core::EditorCore;
use super::images::ImagePayload;
use super::model::{Affinity, BlockKind, DocPoint, Document, Selection};
use super::transaction::Transaction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureKind {
    Empty,
    Typical,
    Long,
}

impl FixtureKind {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "empty" => Ok(Self::Empty),
            "typical" => Ok(Self::Typical),
            "long" => Ok(Self::Long),
            other => Err(format!(
                "unknown fixture '{other}' (expected empty|typical|long)"
            )),
        }
    }
}

pub fn build_document(kind: FixtureKind) -> Document {
    let count = match kind {
        FixtureKind::Empty => 1,
        FixtureKind::Typical => 200,
        FixtureKind::Long => 10_000,
    };
    let mut document = Document::from_paragraphs((0..count).map(|index| {
        if kind == FixtureKind::Empty {
            String::new()
        } else if kind == FixtureKind::Long {
            // Keep the 10,000-block stress case focused on viewport work and
            // height indexing rather than retaining 10,000 repeated strings.
            "x".to_owned()
        } else {
            format!("task7 deterministic block {index:05}")
        }
    }));
    if kind == FixtureKind::Typical {
        let blocks = document.blocks().collect_range(0..document.block_count());
        for (index, block) in blocks.into_iter().enumerate() {
            let text_len = block.content.as_text().map_or(0, str::len);
            let kind = match index % 3 {
                0 => BlockKind::Paragraph,
                1 => BlockKind::BulletItem { depth: 0 },
                _ => BlockKind::OrderedItem { depth: 0 },
            };
            if kind != BlockKind::Paragraph {
                document
                    .apply(Transaction::SetBlockKind {
                        selection: Selection::new(
                            DocPoint::with_affinity(block.id, 0, Affinity::Before),
                            DocPoint::with_affinity(block.id, text_len, Affinity::After),
                        ),
                        kind,
                    })
                    .expect("deterministic list fixture should be valid");
            }
        }
    }
    document
}

pub const fn typical_image_count() -> usize {
    10
}

pub fn typical_image_payload(index: usize) -> ImagePayload {
    assert!(index < typical_image_count());
    let color = [
        (index as u8).wrapping_mul(23).wrapping_add(17),
        (index as u8).wrapping_mul(41).wrapping_add(29),
        (index as u8).wrapping_mul(67).wrapping_add(43),
        0xff,
    ];
    // Ten real image resources exercise the production cache and paint path;
    // keep the deterministic benchmark fixture representative without making
    // the fixed-capacity RSS gate depend on ten full-size photo proxies.
    let frame = ImageBuffer::from_pixel(320, 180, Rgba(color));
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(frame)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("deterministic PNG fixture should encode");
    ImagePayload::new(ImageFormat::Png, encoded.into_inner())
}

pub fn populate_typical_images(editor: &mut EditorCore) -> Result<(), super::model::DocumentError> {
    for index in 0..typical_image_count() {
        editor.insert_image_payload(typical_image_payload(index))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_modes_are_strict_and_have_contract_counts() {
        assert_eq!(FixtureKind::parse("empty").unwrap(), FixtureKind::Empty);
        assert_eq!(FixtureKind::parse("typical").unwrap(), FixtureKind::Typical);
        assert_eq!(FixtureKind::parse("long").unwrap(), FixtureKind::Long);
        assert!(FixtureKind::parse("unknown").is_err());
        assert_eq!(build_document(FixtureKind::Empty).block_count(), 1);
        assert_eq!(build_document(FixtureKind::Typical).block_count(), 200);
        assert_eq!(build_document(FixtureKind::Long).block_count(), 10_000);
        assert_eq!(typical_image_count(), 10);
        assert!(typical_image_payload(0).bytes != typical_image_payload(1).bytes);
    }
}
