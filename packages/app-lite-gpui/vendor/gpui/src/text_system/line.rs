use crate::{
    App, Bounds, Half, Hsla, LineLayout, Pixels, Point, Result, RunScript, SharedString,
    StrikethroughStyle, TextAlign, UnderlineStyle, Window, WrapBoundary, WrappedLineLayout, black,
    fill, point, px, size,
};
use derive_more::{Deref, DerefMut};
use smallvec::SmallVec;
use std::sync::Arc;

/// Set the text decoration for a run of text.
#[derive(Debug, Clone)]
pub struct DecorationRun {
    /// The length of the run in utf-8 bytes.
    pub len: u32,

    /// The color for this run
    pub color: Hsla,

    /// The background color for this run
    pub background_color: Option<Hsla>,

    /// The underline style for this run
    pub underline: Option<UnderlineStyle>,

    /// The strikethrough style for this run
    pub strikethrough: Option<StrikethroughStyle>,

    /// Size and baseline of the run's text, which its underline and
    /// strikethrough follow (Joplin Lite local change).
    pub script: Option<RunScript>,
}

/// A line of text that has been shaped and decorated.
#[derive(Clone, Default, Debug, Deref, DerefMut)]
pub struct ShapedLine {
    #[deref]
    #[deref_mut]
    pub(crate) layout: Arc<LineLayout>,
    /// The text that was shaped for this line.
    pub text: SharedString,
    pub(crate) decoration_runs: SmallVec<[DecorationRun; 32]>,
}

impl ShapedLine {
    /// The length of the line in utf-8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.layout.len
    }

    /// Override the len, useful if you're rendering text a
    /// as text b (e.g. rendering invisibles).
    pub fn with_len(mut self, len: usize) -> Self {
        let layout = self.layout.as_ref();
        self.layout = Arc::new(LineLayout {
            font_size: layout.font_size,
            width: layout.width,
            ascent: layout.ascent,
            descent: layout.descent,
            runs: layout.runs.clone(),
            len,
        });
        self
    }

    /// Paint the line of text to the window.
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        paint_line(
            origin,
            &self.layout,
            line_height,
            TextAlign::default(),
            None,
            &self.decoration_runs,
            &[],
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint the background of the line to the window.
    pub fn paint_background(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        paint_line_background(
            origin,
            &self.layout,
            line_height,
            TextAlign::default(),
            None,
            &self.decoration_runs,
            &[],
            window,
            cx,
        )?;

        Ok(())
    }
}

/// A line of text that has been shaped, decorated, and wrapped by the text layout system.
#[derive(Clone, Default, Debug, Deref, DerefMut)]
pub struct WrappedLine {
    #[deref]
    #[deref_mut]
    pub(crate) layout: Arc<WrappedLineLayout>,
    /// The text that was shaped for this line.
    pub text: SharedString,
    pub(crate) decoration_runs: SmallVec<[DecorationRun; 32]>,
}

impl WrappedLine {
    /// The length of the underlying, unwrapped layout, in utf-8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.layout.len()
    }

    /// Paint this line of text to the window.
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        bounds: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let align_width = match bounds {
            Some(bounds) => Some(bounds.size.width),
            None => self.layout.wrap_width,
        };

        paint_line(
            origin,
            &self.layout.unwrapped_layout,
            line_height,
            align,
            align_width,
            &self.decoration_runs,
            &self.wrap_boundaries,
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint the background of line of text to the window.
    pub fn paint_background(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        bounds: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let align_width = match bounds {
            Some(bounds) => Some(bounds.size.width),
            None => self.layout.wrap_width,
        };

        paint_line_background(
            origin,
            &self.layout.unwrapped_layout,
            line_height,
            align,
            align_width,
            &self.decoration_runs,
            &self.wrap_boundaries,
            window,
            cx,
        )?;

        Ok(())
    }
}

/// How far a run's underline and strikethrough move from where the line's own
/// text puts them: script text keeps GPUI's placement relative to its own
/// smaller glyphs and raised or lowered baseline (Joplin Lite local change).
fn script_decoration_shift(layout: &LineLayout, script: Option<RunScript>) -> (Pixels, Pixels) {
    let Some(script) = script else {
        return (Pixels::ZERO, Pixels::ZERO);
    };
    let scale = script.size_permille as f32 / 1000.0;
    let rise = script.rise(layout.font_size);
    // GPUI puts the underline 0.618 descents below the baseline and the
    // strikethrough a quarter ascent above the line's usual position.
    let underline = layout.descent * 0.618 * (scale - 1.0) - rise;
    let strikethrough = layout.ascent * 0.25 * (1.0 - scale) - rise;
    (underline, strikethrough)
}

fn paint_line(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    decoration_runs: &[DecorationRun],
    wrap_boundaries: &[WrapBoundary],
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let line_bounds = Bounds::new(
        origin,
        size(
            layout.width,
            line_height * (wrap_boundaries.len() as f32 + 1.),
        ),
    );
    window.paint_layer(line_bounds, |window| {
        let padding_top = (line_height - layout.ascent - layout.descent) / 2.;
        let baseline_offset = point(px(0.), padding_top + layout.ascent);
        let mut decoration_runs = decoration_runs.iter();
        let mut wraps = wrap_boundaries.iter().peekable();
        let mut run_end = 0;
        let mut color = black();
        let mut current_underline: Option<(Point<Pixels>, UnderlineStyle)> = None;
        let mut current_strikethrough: Option<(Point<Pixels>, StrikethroughStyle)> = None;
        let mut decoration_script: Option<RunScript> = None;
        let text_system = cx.text_system().clone();
        let mut glyph_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                px(0.0),
                &align,
                layout,
                wraps.peek(),
            ),
            origin.y,
        );
        let mut prev_glyph_position = Point::default();
        let mut max_glyph_size = size(px(0.), px(0.));
        let mut first_glyph_x = origin.x;
        for (run_ix, run) in layout.runs.iter().enumerate() {
            // Joplin Lite: a script run paints at its own size and baseline.
            let run_font_size = run.font_size(layout.font_size);
            let run_rise = point(px(0.), -run.rise(layout.font_size));
            max_glyph_size = text_system.bounding_box(run.font_id, run_font_size).size;

            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                glyph_origin.x += glyph.position.x - prev_glyph_position.x;
                if glyph_ix == 0 && run_ix == 0 {
                    first_glyph_x = glyph_origin.x;
                }

                if wraps.peek() == Some(&&WrapBoundary { run_ix, glyph_ix }) {
                    wraps.next();
                    if let Some((underline_origin, underline_style)) = current_underline.as_mut() {
                        if glyph_origin.x == underline_origin.x {
                            underline_origin.x -= max_glyph_size.width.half();
                        };
                        window.paint_underline(
                            *underline_origin,
                            glyph_origin.x - underline_origin.x,
                            underline_style,
                        );
                        underline_origin.x = origin.x;
                        underline_origin.y += line_height;
                    }
                    if let Some((strikethrough_origin, strikethrough_style)) =
                        current_strikethrough.as_mut()
                    {
                        if glyph_origin.x == strikethrough_origin.x {
                            strikethrough_origin.x -= max_glyph_size.width.half();
                        };
                        window.paint_strikethrough(
                            *strikethrough_origin,
                            glyph_origin.x - strikethrough_origin.x,
                            strikethrough_style,
                        );
                        strikethrough_origin.x = origin.x;
                        strikethrough_origin.y += line_height;
                    }

                    glyph_origin.x = aligned_origin_x(
                        origin,
                        align_width.unwrap_or(layout.width),
                        glyph.position.x,
                        &align,
                        layout,
                        wraps.peek(),
                    );
                    glyph_origin.y += line_height;
                }
                prev_glyph_position = glyph.position;

                let mut finished_underline: Option<(Point<Pixels>, UnderlineStyle)> = None;
                let mut finished_strikethrough: Option<(Point<Pixels>, StrikethroughStyle)> = None;
                if glyph.index >= run_end {
                    let mut style_run = decoration_runs.next();

                    // ignore style runs that apply to a partial glyph
                    while let Some(run) = style_run {
                        if glyph.index < run_end + (run.len as usize) {
                            break;
                        }
                        run_end += run.len as usize;
                        style_run = decoration_runs.next();
                    }

                    if let Some(style_run) = style_run {
                        // Joplin Lite: script text carries its own lines, as
                        // Evernote nests underline and strikethrough inside
                        // <sup>/<sub>, so they break where the script does.
                        let script_changed = style_run.script != decoration_script;
                        decoration_script = style_run.script;
                        let (underline_dy, strikethrough_dy) =
                            script_decoration_shift(layout, style_run.script);
                        if let Some((_, underline_style)) = &mut current_underline
                            && (script_changed
                                || style_run.underline.as_ref() != Some(underline_style))
                        {
                            finished_underline = current_underline.take();
                        }
                        if let Some(run_underline) = style_run.underline.as_ref() {
                            current_underline.get_or_insert((
                                point(
                                    glyph_origin.x,
                                    glyph_origin.y
                                        + baseline_offset.y
                                        + (layout.descent * 0.618)
                                        + underline_dy,
                                ),
                                UnderlineStyle {
                                    color: Some(run_underline.color.unwrap_or(style_run.color)),
                                    thickness: run_underline.thickness,
                                    wavy: run_underline.wavy,
                                },
                            ));
                        }
                        if let Some((_, strikethrough_style)) = &mut current_strikethrough
                            && (script_changed
                                || style_run.strikethrough.as_ref() != Some(strikethrough_style))
                        {
                            finished_strikethrough = current_strikethrough.take();
                        }
                        if let Some(run_strikethrough) = style_run.strikethrough.as_ref() {
                            current_strikethrough.get_or_insert((
                                point(
                                    glyph_origin.x,
                                    glyph_origin.y
                                        + (((layout.ascent * 0.5) + baseline_offset.y) * 0.5)
                                        + strikethrough_dy,
                                ),
                                StrikethroughStyle {
                                    color: Some(run_strikethrough.color.unwrap_or(style_run.color)),
                                    thickness: run_strikethrough.thickness,
                                },
                            ));
                        }

                        run_end += style_run.len as usize;
                        color = style_run.color;
                    } else {
                        run_end = layout.len;
                        finished_underline = current_underline.take();
                        finished_strikethrough = current_strikethrough.take();
                    }
                }

                if let Some((mut underline_origin, underline_style)) = finished_underline {
                    if underline_origin.x == glyph_origin.x {
                        underline_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_underline(
                        underline_origin,
                        glyph_origin.x - underline_origin.x,
                        &underline_style,
                    );
                }

                if let Some((mut strikethrough_origin, strikethrough_style)) =
                    finished_strikethrough
                {
                    if strikethrough_origin.x == glyph_origin.x {
                        strikethrough_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_strikethrough(
                        strikethrough_origin,
                        glyph_origin.x - strikethrough_origin.x,
                        &strikethrough_style,
                    );
                }

                let max_glyph_bounds = Bounds {
                    origin: glyph_origin + run_rise,
                    size: max_glyph_size,
                };

                let content_mask = window.content_mask();
                if max_glyph_bounds.intersects(&content_mask.bounds) {
                    if glyph.is_emoji {
                        window.paint_emoji(
                            glyph_origin + baseline_offset + run_rise,
                            run.font_id,
                            glyph.id,
                            run_font_size,
                        )?;
                    } else {
                        window.paint_glyph(
                            glyph_origin + baseline_offset + run_rise,
                            run.font_id,
                            glyph.id,
                            run_font_size,
                            color,
                        )?;
                    }
                }
            }
        }

        let mut last_line_end_x = first_glyph_x + layout.width;
        if let Some(boundary) = wrap_boundaries.last() {
            let run = &layout.runs[boundary.run_ix];
            let glyph = &run.glyphs[boundary.glyph_ix];
            last_line_end_x -= glyph.position.x;
        }

        if let Some((mut underline_start, underline_style)) = current_underline.take() {
            if last_line_end_x == underline_start.x {
                underline_start.x -= max_glyph_size.width.half()
            };
            window.paint_underline(
                underline_start,
                last_line_end_x - underline_start.x,
                &underline_style,
            );
        }

        if let Some((mut strikethrough_start, strikethrough_style)) = current_strikethrough.take() {
            if last_line_end_x == strikethrough_start.x {
                strikethrough_start.x -= max_glyph_size.width.half()
            };
            window.paint_strikethrough(
                strikethrough_start,
                last_line_end_x - strikethrough_start.x,
                &strikethrough_style,
            );
        }

        Ok(())
    })
}

fn paint_line_background(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    decoration_runs: &[DecorationRun],
    wrap_boundaries: &[WrapBoundary],
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    // Joplin Lite: aligned rows sit anywhere within the alignment width, so
    // the layer spans it rather than only the unaligned text width.
    let line_bounds = Bounds::new(
        origin,
        size(
            align_width.unwrap_or(layout.width).max(layout.width),
            line_height * (wrap_boundaries.len() as f32 + 1.),
        ),
    );
    window.paint_layer(line_bounds, |window| {
        let mut decoration_runs = decoration_runs.iter();
        let mut wraps = wrap_boundaries.iter().peekable();
        let mut run_end = 0;
        let mut current_background: Option<(Point<Pixels>, Hsla)> = None;
        let text_system = cx.text_system().clone();
        let mut glyph_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                px(0.0),
                &align,
                layout,
                wraps.peek(),
            ),
            origin.y,
        );
        let mut prev_glyph_position = Point::default();
        let mut max_glyph_size = size(px(0.), px(0.));
        for (run_ix, run) in layout.runs.iter().enumerate() {
            max_glyph_size = text_system.bounding_box(run.font_id, layout.font_size).size;

            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                glyph_origin.x += glyph.position.x - prev_glyph_position.x;

                if wraps.peek() == Some(&&WrapBoundary { run_ix, glyph_ix }) {
                    wraps.next();
                    if let Some((background_origin, background_color)) = current_background.as_mut()
                    {
                        if glyph_origin.x == background_origin.x {
                            background_origin.x -= max_glyph_size.width.half()
                        }
                        window.paint_quad(fill(
                            Bounds {
                                origin: *background_origin,
                                size: size(glyph_origin.x - background_origin.x, line_height),
                            },
                            *background_color,
                        ));
                    }

                    glyph_origin.x = aligned_origin_x(
                        origin,
                        align_width.unwrap_or(layout.width),
                        glyph.position.x,
                        &align,
                        layout,
                        wraps.peek(),
                    );
                    glyph_origin.y += line_height;
                    // Joplin Lite: a highlight continuing onto the next row
                    // starts where that row's aligned glyphs do.
                    if let Some((background_origin, _)) = current_background.as_mut() {
                        background_origin.x = glyph_origin.x;
                        background_origin.y += line_height;
                    }
                }
                prev_glyph_position = glyph.position;

                let mut finished_background: Option<(Point<Pixels>, Hsla)> = None;
                if glyph.index >= run_end {
                    let mut style_run = decoration_runs.next();

                    // ignore style runs that apply to a partial glyph
                    while let Some(run) = style_run {
                        if glyph.index < run_end + (run.len as usize) {
                            break;
                        }
                        run_end += run.len as usize;
                        style_run = decoration_runs.next();
                    }

                    if let Some(style_run) = style_run {
                        if let Some((_, background_color)) = &mut current_background
                            && style_run.background_color.as_ref() != Some(background_color)
                        {
                            finished_background = current_background.take();
                        }
                        if let Some(run_background) = style_run.background_color {
                            current_background.get_or_insert((
                                point(glyph_origin.x, glyph_origin.y),
                                run_background,
                            ));
                        }
                        run_end += style_run.len as usize;
                    } else {
                        run_end = layout.len;
                        finished_background = current_background.take();
                    }
                }

                if let Some((mut background_origin, background_color)) = finished_background {
                    let mut width = glyph_origin.x - background_origin.x;
                    if background_origin.x == glyph_origin.x {
                        background_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_quad(fill(
                        Bounds {
                            origin: background_origin,
                            size: size(width, line_height),
                        },
                        background_color,
                    ));
                }
            }
        }

        // Joplin Lite: the last row ends at its own aligned start plus its
        // width, not at the unaligned origin.
        let last_row_source_x = wrap_boundaries.last().map_or(px(0.), |boundary| {
            layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].position.x
        });
        let last_row_origin_x = aligned_origin_x(
            origin,
            align_width.unwrap_or(layout.width),
            last_row_source_x,
            &align,
            layout,
            None,
        );
        let last_line_end_x = last_row_origin_x + layout.width - last_row_source_x;

        if let Some((mut background_origin, background_color)) = current_background.take() {
            if last_line_end_x == background_origin.x {
                background_origin.x -= max_glyph_size.width.half()
            };
            window.paint_quad(fill(
                Bounds {
                    origin: background_origin,
                    size: size(last_line_end_x - background_origin.x, line_height),
                },
                background_color,
            ));
        }

        Ok(())
    })
}

fn aligned_origin_x(
    origin: Point<Pixels>,
    align_width: Pixels,
    last_glyph_x: Pixels,
    align: &TextAlign,
    layout: &LineLayout,
    wrap_boundary: Option<&&WrapBoundary>,
) -> Pixels {
    let end_of_line = if let Some(WrapBoundary { run_ix, glyph_ix }) = wrap_boundary {
        layout.runs[*run_ix].glyphs[*glyph_ix].position.x
    } else {
        layout.width
    };

    let line_width = end_of_line - last_glyph_x;

    match align {
        TextAlign::Left => origin.x,
        TextAlign::Center => (origin.x * 2.0 + align_width - line_width) / 2.0,
        TextAlign::Right => origin.x + align_width - line_width,
    }
}

// Joplin Lite: script text carries its own underline and strikethrough.
#[cfg(test)]
mod script_decoration_tests {
    use super::script_decoration_shift;
    use crate::{
        LineLayout, RunScript, TestAppContext, TextRun, UnderlineStyle, WindowTextSystem, font, px,
    };

    const SUPERSCRIPT: RunScript = RunScript {
        size_permille: 833,
        rise_permille: 333,
    };
    const SUBSCRIPT: RunScript = RunScript {
        size_permille: 833,
        rise_permille: -200,
    };

    #[test]
    fn lines_follow_the_script_texts_own_baseline() {
        let layout = LineLayout {
            font_size: px(20.),
            ascent: px(18.),
            descent: px(5.),
            ..Default::default()
        };
        assert_eq!(script_decoration_shift(&layout, None), (px(0.), px(0.)));
        let (under_up, strike_up) = script_decoration_shift(&layout, Some(SUPERSCRIPT));
        let (under_down, strike_down) = script_decoration_shift(&layout, Some(SUBSCRIPT));
        // Superscript lines rise with its text (y grows downward); subscript
        // lines drop with it.
        assert!(
            under_up < px(-6.) && strike_up < px(-5.),
            "{under_up:?} {strike_up:?}"
        );
        assert!(
            under_down > px(3.) && strike_down > px(3.),
            "{under_down:?} {strike_down:?}"
        );
    }

    #[crate::test]
    fn decoration_runs_break_where_script_text_starts_and_ends(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let text_system = WindowTextSystem::new(cx.text_system().clone());
            let underline = Some(UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            });
            let run = |len, script| TextRun {
                len,
                font: font("Helvetica"),
                color: Default::default(),
                background_color: None,
                underline,
                strikethrough: None,
                script,
            };
            let line = text_system.shape_line(
                "x22y".into(),
                px(20.),
                &[run(1, None), run(2, Some(SUPERSCRIPT)), run(1, None)],
                None,
            );
            assert_eq!(
                line.decoration_runs
                    .iter()
                    .map(|run| (run.len, run.script))
                    .collect::<Vec<_>>(),
                vec![(1, None), (2, Some(SUPERSCRIPT)), (1, None)]
            );
        });
    }
}

// Joplin Lite acceptance 137: native highlight must cover the aligned text,
// not merely report a successful background-paint call.
#[cfg(test)]
mod aligned_background_acceptance_tests {
    use crate as gpui;
    use crate::{
        App, Bounds, Context, Render, Styled, TestAppContext, TextAlign, TextRun,
        Window, canvas, font, point, px, rgba, size,
    };
    use std::{cell::Cell, rc::Rc};

    struct HighlightView {
        align: TextAlign,
        text_width: Rc<Cell<f32>>,
    }

    impl Render for HighlightView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl crate::IntoElement {
            let align = self.align;
            let text_width = self.text_width.clone();
            canvas(
                move |_bounds, window, _cx| {
                    let text: crate::SharedString = "第一段：工具栏真实多段验收".into();
                    let lines = window.text_system().shape_text(
                        text.clone(),
                        px(20.),
                        &[TextRun {
                            len: text.len(),
                            font: font("Helvetica"),
                            color: crate::black(),
                            background_color: Some(rgba(0xffd84d66).into()),
                            underline: None,
                            strikethrough: None,
                            script: None,
                        }],
                        Some(px(600.)),
                        None,
                    ).unwrap();
                    assert_eq!(lines.len(), 1);
                    assert!(lines[0].wrap_boundaries.is_empty());
                    text_width.set(f32::from(lines[0].width()));
                    lines
                },
                move |_bounds, lines, window, cx: &mut App| {
                    let bounds = Bounds::new(point(px(20.), px(20.)), size(px(600.), px(40.)));
                    lines[0].paint_background(bounds.origin, px(40.), align, Some(bounds), window, cx).unwrap();
                },
            ).w(px(640.)).h(px(100.))
        }
    }

    fn assert_highlight_covers_aligned_text(cx: &mut TestAppContext, align: TextAlign) {
        let text_width = Rc::new(Cell::new(0.));
        let width_for_view = text_width.clone();
        let (_view, cx) = cx.add_window_view(move |_window, _cx| HighlightView {
            align,
            text_width: width_for_view,
        });
        let (quads, scale) = cx.update(|window, app| {
            window.draw(app).clear();
            (
                window.rendered_frame.scene.quads.iter().map(|quad| quad.bounds).collect::<Vec<_>>(),
                window.scale_factor(),
            )
        });
        let width = text_width.get();
        assert!(width > 0. && width < 600., "fixture must have alignment room: {width}");
        // Literal viewport/origin; expected placement is independent of
        // aligned_origin_x and paint_line_background's endpoint logic.
        let left = match align {
            TextAlign::Left => 20.,
            TextAlign::Center => 20. + (600. - width) / 2.,
            TextAlign::Right => 620. - width,
        };
        assert_eq!(quads.len(), 1, "{align:?}: one complete highlighted row must reach the scene; {quads:?}");
        let actual = quads[0];
        assert!((f64::from(actual.left()) / f64::from(scale) - f64::from(left)).abs() < 0.5,
            "{align:?}: highlight starts with the glyphs: {actual:?}, expected left {left}");
        assert!((f64::from(actual.right()) / f64::from(scale) - f64::from(left + width)).abs() < 0.5,
            "{align:?}: highlight ends with the last glyph: {actual:?}, expected right {}", left + width);
    }

    #[crate::test]
    fn left_highlight_covers_the_complete_cjk_row(cx: &mut TestAppContext) {
        assert_highlight_covers_aligned_text(cx, TextAlign::Left);
    }

    #[crate::test]
    fn centered_highlight_covers_the_complete_cjk_row(cx: &mut TestAppContext) {
        assert_highlight_covers_aligned_text(cx, TextAlign::Center);
    }

    #[crate::test]
    fn right_highlight_covers_the_complete_cjk_row(cx: &mut TestAppContext) {
        assert_highlight_covers_aligned_text(cx, TextAlign::Right);
    }
}

// Acceptance 149: inspect actual scene output for partial ranges and wrapped
// rows. This is deliberately independent of aligned_origin_x/background paint.
#[cfg(test)]
mod highlight_range_acceptance_tests {
    use crate as gpui;
    use crate::{
        App, Bounds, Context, Hsla, Render, Styled, TestAppContext, TextAlign, TextRun,
        Window, WrappedLine, canvas, font, point, px, rgba, size,
    };
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Debug)]
    struct ExpectedBackground {
        rect: [f32; 4],
        color: Hsla,
    }

    struct RangeView {
        text: &'static str,
        spans: Vec<(usize, Option<u32>)>,
        width: f32,
        align: TextAlign,
        must_wrap: bool,
        native_shaping: bool,
        expected: Rc<RefCell<Vec<ExpectedBackground>>>,
    }

    // The oracle uses only shaping metrics and literal style ranges. It does
    // not call the background painter or its alignment/endpoint helpers.
    fn expected_backgrounds(
        line: &WrappedLine,
        spans: &[(usize, Option<u32>)],
        width: f32,
        align: TextAlign,
    ) -> Vec<ExpectedBackground> {
        let layout = &line.layout.unwrapped_layout;
        let x_at = |index: usize| -> f32 {
            if index == layout.len {
                return f32::from(layout.width);
            }
            f32::from(layout.runs.iter().flat_map(|run| &run.glyphs)
                .find(|glyph| glyph.index == index)
                .expect("fixture style boundary must coincide with a complete glyph").position.x)
        };
        let mut cuts = vec![0];
        cuts.extend(line.wrap_boundaries.iter().map(|boundary| {
            layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
        }));
        cuts.push(layout.len);
        let mut expected = Vec::new();
        for (row, pair) in cuts.windows(2).enumerate() {
            let row_width = x_at(pair[1]) - x_at(pair[0]);
            let row_left = 20. + match align {
                TextAlign::Left => 0.,
                TextAlign::Center => (width - row_width) / 2.,
                TextAlign::Right => width - row_width,
            };
            let mut start = 0;
            for &(len, color) in spans {
                let end = start + len;
                let lo = start.max(pair[0]);
                let hi = end.min(pair[1]);
                if lo < hi && let Some(color) = color {
                    expected.push(ExpectedBackground {
                        rect: [row_left + x_at(lo) - x_at(pair[0]),
                            20. + row as f32 * 40., x_at(hi) - x_at(lo), 40.],
                        color: rgba(color).into(),
                    });
                }
                start = end;
            }
        }
        expected
    }

    impl Render for RangeView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl crate::IntoElement {
            let text = self.text;
            let spans = self.spans.clone();
            let width = self.width;
            let align = self.align;
            let must_wrap = self.must_wrap;
            let native_shaping = self.native_shaping;
            let expected = self.expected.clone();
            canvas(
                move |_bounds, window, _cx| {
                    assert_eq!(spans.iter().map(|span| span.0).sum::<usize>(), text.len());
                    let runs = spans.iter().map(|&(len, color)| TextRun {
                        len,
                        font: font("Helvetica"),
                        color: crate::black(),
                        background_color: color.map(|color| rgba(color).into()),
                        underline: None,
                        strikethrough: None,
                        script: None,
                    }).collect::<Vec<_>>();
                    // TestAppContext normally supplies NoopTextSystem. On macOS
                    // also shape with the actual CoreText backend, without
                    // changing the app/test-platform implementation. These
                    // are background-scene checks, not physical glyph pixels.
                    #[cfg(target_os = "macos")]
                    let native_text_system = native_shaping.then(|| crate::WindowTextSystem::new(
                        std::sync::Arc::new(crate::TextSystem::new(
                            std::sync::Arc::new(crate::MacTextSystem::new()),
                        )),
                    ));
                    #[cfg(target_os = "macos")]
                    let text_system = native_text_system.as_ref().unwrap_or(window.text_system());
                    #[cfg(not(target_os = "macos"))]
                    let text_system = window.text_system();
                    let lines = text_system.shape_text(
                        text.into(), px(20.), &runs, Some(px(width)), None,
                    ).unwrap();
                    assert_eq!(lines.len(), 1);
                    assert_eq!(!lines[0].wrap_boundaries.is_empty(), must_wrap,
                        "fixture must exercise its advertised wrapping condition: {text}");
                    *expected.borrow_mut() = expected_backgrounds(&lines[0], &spans, width, align);
                    eprintln!("range acceptance: native={native_shaping}, text={text:?}, align={align:?}, width={:?}, rows={}",
                        lines[0].layout.unwrapped_layout.width, lines[0].wrap_boundaries.len() + 1);
                    lines
                },
                move |_bounds, lines, window, cx: &mut App| {
                    let bounds = Bounds::new(point(px(20.), px(20.)), size(px(width), px(400.)));
                    lines[0].paint_background(bounds.origin, px(40.), align, Some(bounds), window, cx).unwrap();
                },
            ).w(px(640.)).h(px(440.))
        }
    }

    fn check_ranges(
        cx: &mut TestAppContext,
        text: &'static str,
        spans: Vec<(usize, Option<u32>)>,
        width: f32,
        must_wrap: bool,
    ) {
        #[cfg(target_os = "macos")]
        let shapers = [false, true];
        #[cfg(not(target_os = "macos"))]
        let shapers = [false];
        for native_shaping in shapers {
        for align in [TextAlign::Left, TextAlign::Center, TextAlign::Right] {
            let expected = Rc::new(RefCell::new(Vec::<ExpectedBackground>::new()));
            let for_view = expected.clone();
            let spans_for_view = spans.clone();
            let (_view, window_cx) = cx.add_window_view(move |_window, _cx| RangeView {
                text, spans: spans_for_view, width, align, must_wrap, native_shaping, expected: for_view,
            });
            let mut actual = window_cx.update(|window, app| {
                window.draw(app).clear();
                let scale = f64::from(window.scale_factor());
                window.rendered_frame.scene.quads.iter().map(|quad| ExpectedBackground {
                    rect: [
                        (f64::from(quad.bounds.left()) / scale) as f32,
                        (f64::from(quad.bounds.top()) / scale) as f32,
                        (f64::from(quad.bounds.size.width) / scale) as f32,
                        (f64::from(quad.bounds.size.height) / scale) as f32,
                    ],
                    color: quad.background.solid,
                }).collect::<Vec<_>>()
            });
            let mut expected = expected.borrow().clone();
            let order = |a: &ExpectedBackground, b: &ExpectedBackground| {
                a.rect[1].total_cmp(&b.rect[1]).then(a.rect[0].total_cmp(&b.rect[0]))
            };
            actual.sort_by(order);
            expected.sort_by(order);
            assert_eq!(actual.len(), expected.len(),
                "native={native_shaping}/{text:?}/{align:?}: no omitted marked row or invented background; actual {actual:?}, expected {expected:?}");
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.color, expected.color, "{text:?}/{align:?}: adjacent marks must retain their colors");
                assert!(actual.rect[2] > 0., "background must have positive width: {actual:?}");
                for axis in 0..4 {
                    assert!((actual.rect[axis] - expected.rect[axis]).abs() < 0.5,
                        "native={native_shaping}/{text:?}/{align:?}: exact marked range on axis {axis}; actual {actual:?}, expected {expected:?}");
                }
            }
        }
        }
    }

    #[crate::test]
    fn partial_cjk_mark_does_not_cover_unmarked_prefix_or_suffix(cx: &mut TestAppContext) {
        check_ranges(cx, "甲乙丙丁", vec![(3, None), (6, Some(0xffd84d66)), (3, None)], 600., false);
    }

    #[crate::test]
    fn adjacent_cjk_marks_retain_distinct_colors(cx: &mut TestAppContext) {
        check_ranges(cx, "甲乙丙丁", vec![(6, Some(0xffd84d66)), (6, Some(0x509cff66))], 600., false);
    }

    #[crate::test]
    fn wrapped_full_mark_aligns_each_row_including_short_last_row(cx: &mut TestAppContext) {
        let text = "第一段中文换行与最后短行甲乙丙丁";
        check_ranges(cx, text, vec![(text.len(), Some(0xffd84d66))], 120., true);
    }

    #[crate::test]
    fn wrapped_partial_mark_preserves_unmarked_ends(cx: &mut TestAppContext) {
        let text = "第一段中文换行与最后短行甲乙丙丁";
        check_ranges(cx, text, vec![(3, None), (text.len() - 6, Some(0xffd84d66)), (3, None)], 120., true);
    }

    #[crate::test]
    fn mixed_cjk_latin_emoji_full_mark_matches_shaped_extent(cx: &mut TestAppContext) {
        let text = "中文 Latin 😀";
        check_ranges(cx, text, vec![(text.len(), Some(0xffd84d66))], 600., false);
    }

    #[crate::test]
    fn no_mark_produces_no_background_quad(cx: &mut TestAppContext) {
        let text = "无高亮";
        check_ranges(cx, text, vec![(text.len(), None)], 600., false);
    }
}
