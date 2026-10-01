//! One measured layout per visible table. Evernote's table cells hold rich
//! content (`tablecontent+`, table/schema.ts 219), so a cell here is shaped
//! text with its marks, line breaks, images and attachment labels, stacked
//! in document order. The layout registry measures it once with the real
//! text system; row heights, painting and hit-testing all read that result.

use app_lite_core::document::{Inline, Marks};
use gpui::{
    Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Point, SharedString, StrikethroughStyle,
    TextRun, UnderlineStyle, WrappedLine, px, rgba,
};

use super::images::image_layout_size;
use super::layout::{TABLE_CELL_PADDING_X, TABLE_LINE_HEIGHT, TABLE_ROW_PADDING};
use super::model::TableContent;

pub(crate) const TABLE_FONT_SIZE: f32 = 14.0;
/// Space between an image or attachment label and what follows it.
const PIECE_GAP: f32 = 4.0;
const ATTACHMENT_LABEL_PADDING: f32 = 6.0;
/// Columns never get narrower than this; a table that then outgrows the
/// note can be scrolled horizontally inside its clipped viewport.
pub(crate) const MIN_TABLE_COLUMN_WIDTH: f32 = 64.0;
/// Size an image is laid out at until its real size is known.
const IMAGE_PLACEHOLDER: (u32, u32) = (160, 90);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RunMarks {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strikethrough: bool,
    pub(crate) highlight: bool,
    pub(crate) link: bool,
    pub(crate) code: bool,
    pub(crate) script: Option<app_lite_core::Script>,
    pub(crate) color: Option<app_lite_core::TextColor>,
}

#[derive(Clone, Debug)]
pub(crate) enum CellPiece {
    Text {
        top: f32,
        lines: Vec<WrappedLine>,
        /// Each source run's text and marks, for what the cell says.
        runs: Vec<(String, RunMarks)>,
        links: Vec<(std::ops::Range<usize>, String)>,
    },
    Image {
        top: f32,
        width: f32,
        height: f32,
        resource_id: String,
    },
    Attachment {
        top: f32,
        width: f32,
        height: f32,
        resource_id: String,
        lines: Vec<WrappedLine>,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CellLayout {
    pub(crate) pieces: Vec<CellPiece>,
    pub(crate) height: f32,
    /// Where each piece starts in the cell's source offsets (see
    /// [`MEDIA_SOURCE_LEN`]), and the offset of the cell's end.
    pub(crate) sources: Vec<usize>,
    pub(crate) source_len: usize,
}

/// A cell's source offsets count its text bytes, one per line break and
/// this many for an image or file, so that a point in the laid-out table and
/// a caret in the cell's editor name the same place.
pub(crate) const MEDIA_SOURCE_LEN: usize = '\u{FFFC}'.len_utf8();

/// What `TableLayout` was measured from; a different key means measure
/// again.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableLayoutKey {
    pub(crate) revision: u64,
    pub(crate) width: f32,
    pub(crate) image_generation: u64,
    pub(crate) font: Font,
}

#[derive(Clone, Debug)]
pub(crate) struct TableLayout {
    pub(crate) key: TableLayoutKey,
    pub(crate) column_width: f32,
    pub(crate) row_heights: Vec<f32>,
    pub(crate) cells: Vec<Vec<CellLayout>>,
}

/// Where a point in a table falls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableHit {
    pub(crate) row: usize,
    pub(crate) column: usize,
    /// The attachment label under the point, if any.
    pub(crate) attachment: Option<String>,
}

impl TableLayout {
    /// Resolve only actual glyphs, not the whitespace following a link.
    pub(crate) fn link_at(&self, bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<String> {
        let hit = self.hit(bounds, position)?;
        let (left, top) = self.content_origin(hit.row, hit.column);
        let x = position.x - bounds.left() - px(left);
        let y = position.y - bounds.top() - px(top);
        if x < px(0.0) || y < px(0.0) { return None; }
        for piece in &self.cells[hit.row][hit.column].pieces {
            if let CellPiece::Text { top, lines, links, .. } = piece {
                let mut line_top = px(*top);
                let mut byte_start = 0;
                for line in lines {
                    let height = line.size(px(TABLE_LINE_HEIGHT)).height;
                    if y >= line_top && y < line_top + height {
                        let index = line.index_for_position(gpui::point(x, y - line_top), px(TABLE_LINE_HEIGHT)).ok()?;
                        return links.iter().find(|(range, _)| range.contains(&(byte_start + index)))
                            .map(|(_, url)| url.clone());
                    }
                    line_top += height;
                    byte_start += line.len() + 1; // shape_text separates hard newlines.
                }
            }
        }
        None
    }

    /// The cell under `position` and the source offset (see
    /// [`MEDIA_SOURCE_LEN`]) of the caret there: the glyph boundary nearest
    /// on its line, before or after an image by which half is hit, the
    /// start of the piece below a gap and the cell's end below its content.
    pub(crate) fn source_offset_at(
        &self,
        bounds: Bounds<Pixels>,
        position: Point<Pixels>,
    ) -> Option<(usize, usize, usize)> {
        let hit = self.hit(bounds, position)?;
        let cell = self.cells.get(hit.row)?.get(hit.column)?;
        let (left, top) = self.content_origin(hit.row, hit.column);
        let x = (position.x - bounds.left() - px(left)).max(px(0.0));
        let y = position.y - bounds.top() - px(top);
        for (piece, &source) in cell.pieces.iter().zip(&cell.sources) {
            match piece {
                CellPiece::Text { top, lines, .. } => {
                    let mut line_top = px(*top);
                    let mut byte_start = 0;
                    if y < line_top {
                        return Some((hit.row, hit.column, source));
                    }
                    for line in lines {
                        let height = line.size(px(TABLE_LINE_HEIGHT)).height;
                        if y < line_top + height {
                            let index = line
                                .closest_index_for_position(
                                    gpui::point(x, y - line_top),
                                    px(TABLE_LINE_HEIGHT),
                                )
                                .unwrap_or_else(|index| index);
                            return Some((hit.row, hit.column, source + byte_start + index));
                        }
                        line_top += height;
                        byte_start += line.len() + 1;
                    }
                }
                CellPiece::Image { top, height, .. }
                | CellPiece::Attachment { top, height, .. } => {
                    if y < px(top + height) {
                        let before = y < px(top + height / 2.0);
                        let offset = if before {
                            source
                        } else {
                            source + MEDIA_SOURCE_LEN
                        };
                        return Some((hit.row, hit.column, offset));
                    }
                }
            }
        }
        Some((hit.row, hit.column, cell.source_len))
    }

    pub(crate) fn height(&self) -> f32 {
        self.row_heights
            .iter()
            .sum::<f32>()
            .max(TABLE_LINE_HEIGHT + TABLE_ROW_PADDING)
    }

    /// Top-left of a cell's content, relative to the table.
    pub(crate) fn content_origin(&self, row: usize, column: usize) -> (f32, f32) {
        let top: f32 = self.row_heights.iter().take(row).sum();
        (
            self.column_width * column as f32 + TABLE_CELL_PADDING_X,
            top + TABLE_ROW_PADDING / 2.0,
        )
    }

    pub(crate) fn hit(&self, bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<TableHit> {
        let x = f32::from(position.x - bounds.left());
        let y = f32::from(position.y - bounds.top());
        if x < 0.0 || y < 0.0 || x >= f32::from(bounds.size.width) {
            return None;
        }
        let columns = self.cells.first().map_or(1, Vec::len).max(1);
        let column = ((x / self.column_width.max(1.0)).floor() as usize).min(columns - 1);
        let mut top = 0.0;
        for (row, height) in self.row_heights.iter().enumerate() {
            if y < top + height {
                let (left, content_top) = self.content_origin(row, column);
                let attachment = self.cells[row].get(column).and_then(|cell| {
                    cell.pieces.iter().find_map(|piece| match piece {
                        CellPiece::Attachment {
                            top,
                            width,
                            height,
                            resource_id,
                            ..
                        } if x >= left
                            && x < left + width
                            && y >= content_top + top
                            && y < content_top + top + height =>
                        {
                            Some(resource_id.clone())
                        }
                        _ => None,
                    })
                });
                return Some(TableHit {
                    row,
                    column,
                    attachment,
                });
            }
            top += height;
        }
        None
    }

    /// Every image in the table, with where it is drawn relative to the
    /// table.
    pub(crate) fn images(&self) -> impl Iterator<Item = (&str, Bounds<Pixels>)> + '_ {
        self.cells.iter().enumerate().flat_map(move |(row, cells)| {
            cells.iter().enumerate().flat_map(move |(column, cell)| {
                let (left, top) = self.content_origin(row, column);
                cell.pieces.iter().filter_map(move |piece| match piece {
                    CellPiece::Image {
                        top: piece_top,
                        width,
                        height,
                        resource_id,
                    } => Some((
                        resource_id.as_str(),
                        Bounds::new(
                            gpui::point(px(left), px(top + piece_top)),
                            gpui::size(px(*width), px(*height)),
                        ),
                    )),
                    _ => None,
                })
            })
        })
    }
}

/// The cell's content: its canonical inlines, or (for a table built without
/// them) its display text.
fn cell_inlines(table: &TableContent, row: usize, column: usize) -> Vec<Inline> {
    match table.cell_inlines(row, column) {
        Some(inlines) => inlines.to_vec(),
        None => {
            let text = table
                .rows
                .get(row)
                .and_then(|cells| cells.get(column))
                .cloned()
                .unwrap_or_default();
            let mut inlines = Vec::new();
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    inlines.push(Inline::SoftBreak);
                }
                if !line.is_empty() {
                    inlines.push(Inline::Text {
                        text: line.to_owned(),
                        marks: Marks::default(),
                    });
                }
            }
            inlines
        }
    }
}

fn run_marks(marks: &Marks, header: bool) -> RunMarks {
    RunMarks {
        bold: marks.bold || header,
        italic: marks.italic,
        underline: marks.underline,
        strikethrough: marks.strikethrough,
        highlight: marks.highlight,
        link: marks.link.is_some(),
        code: marks.inline_code,
        script: marks.script,
        color: marks.color,
    }
}

/// The same styling a note paragraph gives these marks (`styled_text_runs`).
fn text_run(len: usize, marks: RunMarks, base: &Font) -> TextRun {
    let mut font = base.clone();
    if marks.bold {
        font.weight = FontWeight::BOLD;
    }
    if marks.italic {
        font.style = FontStyle::Italic;
    }
    let color: Hsla = if let Some(color) = marks.color {
        super::layout::text_color(color)
    } else if marks.link {
        gpui::blue()
    } else {
        rgba(0x25342bff).into()
    };
    let thickness = px(1.0);
    TextRun {
        len,
        font,
        color,
        background_color: if marks.highlight {
            Some(rgba(0xffd84d66).into())
        } else if marks.code {
            Some(rgba(0xeef1f4ff).into())
        } else {
            None
        },
        underline: (marks.underline || marks.link).then_some(UnderlineStyle {
            color: Some(color),
            thickness,
            wavy: false,
        }),
        strikethrough: marks.strikethrough.then_some(StrikethroughStyle {
            color: Some(color),
            thickness,
        }),
        script: super::layout::script_for(
            marks.script == Some(app_lite_core::Script::Superscript),
            marks.script == Some(app_lite_core::Script::Subscript),
        ),
    }
}

/// Shape one run of text; None when there is nothing to shape.
fn shape(
    text: &str,
    runs: &[TextRun],
    width: f32,
    text_system: &gpui::WindowTextSystem,
) -> Vec<WrappedLine> {
    if text.is_empty() {
        return Vec::new();
    }
    text_system
        .shape_text(
            SharedString::from(text.to_owned()),
            px(TABLE_FONT_SIZE),
            runs,
            Some(px(width.max(1.0))),
            None,
        )
        .map(|lines| lines.into_vec())
        .unwrap_or_default()
}

fn lines_height(lines: &[WrappedLine]) -> f32 {
    lines
        .iter()
        .map(|line| f32::from(line.size(px(TABLE_LINE_HEIGHT)).height))
        .sum()
}

pub(crate) fn text_width(column_width: f32) -> f32 {
    (column_width - 2.0 * TABLE_CELL_PADDING_X).max(8.0)
}

/// Lays `table` out `width` wide with the real text system. `image_size`
/// knows the natural size of images already loaded.
pub(crate) fn measure_table(
    table: &TableContent,
    key: TableLayoutKey,
    text_system: &gpui::WindowTextSystem,
    image_size: &dyn Fn(&str) -> Option<(u32, u32)>,
) -> TableLayout {
    let columns = table.column_count().max(1);
    let column_width = (key.width / columns as f32).max(MIN_TABLE_COLUMN_WIDTH);
    let available = text_width(column_width);
    let mut cells = Vec::with_capacity(table.rows.len());
    let mut row_heights = Vec::with_capacity(table.rows.len());
    for row in 0..table.rows.len() {
        let header = table.header && row == 0;
        let mut row_cells = Vec::with_capacity(columns);
        for column in 0..columns {
            row_cells.push(measure_cell(
                &cell_inlines(table, row, column),
                header,
                available,
                &key.font,
                text_system,
                image_size,
            ));
        }
        let tallest = row_cells
            .iter()
            .map(|cell: &CellLayout| cell.height)
            .fold(TABLE_LINE_HEIGHT, f32::max);
        row_heights.push(tallest + TABLE_ROW_PADDING);
        cells.push(row_cells);
    }
    TableLayout {
        key,
        column_width,
        row_heights,
        cells,
    }
}

fn measure_cell(
    inlines: &[Inline],
    header: bool,
    width: f32,
    font: &Font,
    text_system: &gpui::WindowTextSystem,
    image_size: &dyn Fn(&str) -> Option<(u32, u32)>,
) -> CellLayout {
    let mut pieces = Vec::new();
    let mut y = 0.0;
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut described: Vec<(String, RunMarks)> = Vec::new();
    let mut links = Vec::new();
    let mut sources = Vec::new();
    let mut source = 0;
    let plain = run_marks(&Marks::default(), header);
    let flush = |text: &mut String,
                 runs: &mut Vec<TextRun>,
                 described: &mut Vec<(String, RunMarks)>,
                 links: &mut Vec<(std::ops::Range<usize>, String)>,
                 pieces: &mut Vec<CellPiece>,
                 sources: &mut Vec<usize>,
                 source: usize,
                 y: &mut f32| {
        // A break with nothing after it still ends the line.
        let lines = shape(text, runs, width, text_system);
        if !lines.is_empty() {
            sources.push(source - text.len());
            pieces.push(CellPiece::Text {
                top: *y,
                lines: lines.clone(),
                runs: std::mem::take(described),
                links: std::mem::take(links),
            });
            *y += lines_height(&lines);
        }
        text.clear();
        runs.clear();
        described.clear();
    };
    for inline in inlines {
        match inline {
            Inline::Text { text: run, marks } => {
                if let Some(url) = marks.link.as_ref() {
                    links.push((text.len()..text.len() + run.len(), url.clone()));
                }
                let marks = run_marks(marks, header);
                text.push_str(run);
                runs.push(text_run(run.len(), marks, font));
                described.push((run.clone(), marks));
                source += run.len();
            }
            Inline::SoftBreak => {
                text.push('\n');
                runs.push(text_run(1, plain, font));
                source += 1;
            }
            Inline::Image {
                resource_id,
                display_width,
                ..
            } => {
                flush(
                    &mut text,
                    &mut runs,
                    &mut described,
                    &mut links,
                    &mut pieces,
                    &mut sources,
                    source,
                    &mut y,
                );
                let natural = image_size(resource_id.as_str()).unwrap_or(IMAGE_PLACEHOLDER);
                let (image_width, image_height) = image_layout_size(width, natural, *display_width);
                sources.push(source);
                source += MEDIA_SOURCE_LEN;
                pieces.push(CellPiece::Image {
                    top: y,
                    width: image_width,
                    height: image_height,
                    resource_id: resource_id.as_str().to_owned(),
                });
                y += image_height + PIECE_GAP;
            }
            Inline::Attachment {
                resource_id,
                filename,
                ..
            } => {
                flush(
                    &mut text,
                    &mut runs,
                    &mut described,
                    &mut links,
                    &mut pieces,
                    &mut sources,
                    source,
                    &mut y,
                );
                let label = format!("📎 {filename}");
                let marks = RunMarks {
                    link: true,
                    ..plain
                };
                let lines = shape(
                    &label,
                    &[text_run(label.len(), marks, font)],
                    (width - 2.0 * ATTACHMENT_LABEL_PADDING).max(1.0),
                    text_system,
                );
                let label_width = lines
                    .iter()
                    .map(|line| f32::from(line.width()))
                    .fold(0.0, f32::max);
                let height = lines_height(&lines);
                sources.push(source);
                source += MEDIA_SOURCE_LEN;
                pieces.push(CellPiece::Attachment {
                    top: y,
                    width: (label_width + 2.0 * ATTACHMENT_LABEL_PADDING).min(width),
                    height,
                    resource_id: resource_id.as_str().to_owned(),
                    lines,
                });
                y += height + PIECE_GAP;
            }
        }
    }
    flush(
        &mut text,
        &mut runs,
        &mut described,
        &mut links,
        &mut pieces,
        &mut sources,
        source,
        &mut y,
    );
    // Trailing gap after a final image or label is not content.
    if matches!(
        pieces.last(),
        Some(CellPiece::Image { .. } | CellPiece::Attachment { .. })
    ) {
        y -= PIECE_GAP;
    }
    CellLayout {
        pieces,
        height: y.max(0.0),
        sources,
        source_len: source,
    }
}

/// Images a table refers to, by resource id.
pub(crate) fn table_image_ids(table: &TableContent) -> impl Iterator<Item = &str> {
    let rows: &[app_lite_core::document::TableRow] = match &table.canonical {
        app_lite_core::document::Block::Table { rows, .. } => rows,
        _ => &[],
    };
    rows.iter()
        .flat_map(|row| &row.cells)
        .flat_map(|cell| &cell.inlines)
        .filter_map(|inline| match inline {
            Inline::Image { resource_id, .. } => Some(resource_id.as_str()),
            _ => None,
        })
}
