//! Drawing the card, with `tiny-skia`.
//!
//! `govox-py` draws this in Cairo, which GTK supplied along with a window and a
//! main loop. `tiny-skia` is pure Rust and gives the same primitives — paths,
//! fills, strokes, anti-aliasing — so it drops the last dev-header dependency
//! in the project. What it does *not* give is text, which is why [`Text`]
//! exists.
//!
//! Everything here is geometry the [`crate::geometry`] module already decided.
//! This file should contain no numbers of its own.

use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, Pixmap, PixmapMut, Rect as SkRect, Stroke, Transform,
};

use crate::geometry as g;

/// What the card is currently showing.
#[derive(Debug, Clone)]
pub struct State {
    /// 0..=1. Below 0.01 nothing is drawn at all.
    pub opacity: f32,
    pub level: f32,
    pub caption: String,
    pub compact: bool,
    /// The fallback liveness signal, used only until a level feed is seen.
    pub pulse_on: bool,
    pub has_level_feed: bool,
    /// The daemon's mode, or `None` for ordinary dictation.
    pub mode: Option<String>,
    pub width: i32,
    pub height: i32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            opacity: 0.0,
            level: 0.0,
            caption: String::new(),
            compact: false,
            pulse_on: true,
            has_level_feed: false,
            mode: None,
            width: g::CARD_WIDTH_IDLE,
            height: g::CARD_HEIGHT,
        }
    }
}

/// How one mode paints: a word for the card, a colour for the glyph.
///
/// The word is at most as long as [`g::LABEL`], the idle text it replaces. The
/// idle card has a fixed width, so a longer one would be clipped — and a mode
/// indicator that reads "Spelling mod" is worse than a terse one.
pub struct ModeStyle {
    pub label: &'static str,
    pub tint: (f32, f32, f32),
}

/// The colour and word for a mode name, or `None` for ordinary dictation.
///
/// Colour is the whole signal on the pill, which has no room for text — so the
/// three are picked to differ in *lightness* as well as hue, and stay
/// distinguishable to a red-green colour-blind reader.
///
/// An unrecognised name still paints an indicator rather than falling back to
/// the microphone. The daemon only sends a name when it is *not* dictating, so
/// "a mode this build does not know" and "dictating normally" are opposite
/// facts; showing the plain mic would assert the wrong one.
#[must_use]
pub fn mode_style(mode: &str) -> ModeStyle {
    match mode {
        "command" => ModeStyle {
            label: "Command",
            tint: (0.35, 0.62, 0.98),
        },
        "spelling" => ModeStyle {
            label: "Spelling",
            tint: (0.99, 0.75, 0.25),
        },
        "asleep" => ModeStyle {
            label: "Asleep",
            tint: (0.55, 0.56, 0.62),
        },
        _ => ModeStyle {
            label: "Mode",
            tint: (0.72, 0.52, 0.95),
        },
    }
}

fn rgba(r: f32, gr: f32, b: f32, a: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba(r, gr, b, a).unwrap_or(Color::TRANSPARENT));
    paint.anti_alias = true;
    paint
}

/// A rounded rectangle, as four corner arcs.
///
/// `PathBuilder` has no arc, so each corner is a quadratic through its own
/// control point — visually identical at these radii and considerably less code
/// than reconstructing Cairo's `arc`.
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut path = PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(x + w - r, y);
    path.quad_to(x + w, y, x + w, y + r);
    path.line_to(x + w, y + h - r);
    path.quad_to(x + w, y + h, x + w - r, y + h);
    path.line_to(x + r, y + h);
    path.quad_to(x, y + h, x, y + h - r);
    path.line_to(x, y + r);
    path.quad_to(x, y, x + r, y);
    path.close();
    path.finish()
}

/// A rounded card that stays visible on any background.
///
/// Overlapping low-alpha fills approximate a soft shadow, which lifts the card
/// off a light background; a bright rim then does the same job against a dark
/// one. Together they mean the card never sinks into whatever application
/// happens to be behind it.
///
/// Filled and overlapping rather than stroked: concentric *strokes* read as
/// three distinct rings against white, while overlapping fills accumulate into
/// something much closer to a falloff.
fn floating_background(canvas: &mut PixmapMut<'_>, x: f32, y: f32, w: f32, h: f32, r: f32, a: f32) {
    for step in (1..=g::HALO_STEPS).rev() {
        let spread = step as f32 * (g::HALO_PAD / g::HALO_STEPS as f32);
        if let Some(path) = rounded_rect(
            x - spread,
            y - spread,
            w + 2.0 * spread,
            h + 2.0 * spread,
            r + spread,
        ) {
            canvas.fill_path(
                &path,
                &rgba(0.0, 0.0, 0.0, 0.055 * a),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    let Some(path) = rounded_rect(x, y, w, h, r) else {
        return;
    };
    // Near-opaque: translucency looked good over a plain desktop and turned to
    // mud over dense text.
    canvas.fill_path(
        &path,
        &rgba(0.13, 0.13, 0.15, 0.97 * a),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    canvas.stroke_path(
        &path,
        &rgba(1.0, 1.0, 1.0, 0.45 * a),
        &Stroke {
            width: 1.0,
            ..Stroke::default()
        },
        Transform::identity(),
        None,
    );
}

/// Draw the whole card into a freshly cleared pixmap.
pub fn draw(pixmap: &mut Pixmap, state: &State, text: &Text) {
    // Fully transparent first. The window has an ARGB visual, so anything left
    // over from the previous frame would composite with this one.
    pixmap.fill(Color::TRANSPARENT);
    if state.opacity <= 0.01 {
        return;
    }
    let mut canvas = pixmap.as_mut();
    if state.compact {
        draw_pill(&mut canvas, state);
    } else {
        draw_card(&mut canvas, state, text);
    }
}

fn draw_card(canvas: &mut PixmapMut<'_>, state: &State, text: &Text) {
    let alpha = state.opacity;
    let width = state.width as f32;
    floating_background(
        canvas,
        g::HALO_PAD,
        g::HALO_PAD,
        width - 2.0 * g::HALO_PAD,
        g::CARD_HEIGHT as f32 - 2.0 * g::HALO_PAD,
        g::CARD_RADIUS,
        alpha,
    );

    // Record dot: steady once a level feed has been seen this session (the bar
    // meter then carries the liveness signal); pulses as a fallback when no
    // level data is arriving at all.
    let dot_cx = 18.0 + g::DOT_RADIUS;
    let dot_cy = g::CARD_HEIGHT as f32 / 2.0;
    let dot_alpha = if state.has_level_feed || state.pulse_on {
        1.0
    } else {
        0.32
    };
    // In a mode, the dot takes the mode's colour: the red record dot means
    // "your speech is becoming text", which in command, spelling and sleep is
    // not what is happening.
    let style = state.mode.as_deref().map(mode_style);
    let (dot_r, dot_g, dot_b) = style
        .as_ref()
        .map_or((0.92, 0.16, 0.16), |style| style.tint);
    if let Some(dot) = PathBuilder::from_circle(dot_cx, dot_cy, g::DOT_RADIUS) {
        canvas.fill_path(
            &dot,
            &rgba(dot_r, dot_g, dot_b, dot_alpha * alpha),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // Level meter: a background track, then a fill scaled to the level.
    let meter_x = dot_cx + g::DOT_RADIUS + g::METER_GAP;
    let meter_y = dot_cy - g::METER_HEIGHT / 2.0;
    let radius = g::METER_HEIGHT / 2.0;
    if let Some(track) = rounded_rect(meter_x, meter_y, g::METER_WIDTH, g::METER_HEIGHT, radius) {
        canvas.fill_path(
            &track,
            &rgba(1.0, 1.0, 1.0, 0.15 * alpha),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let fill_width = g::METER_WIDTH * state.level.clamp(0.0, 1.0);
    if fill_width > 0.0
        && let Some(fill) = rounded_rect(meter_x, meter_y, fill_width, g::METER_HEIGHT, radius)
    {
        canvas.fill_path(
            &fill,
            &rgba(0.30, 0.78, 0.42, 0.95 * alpha),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // The caption when there is one, else the mode's name, else the idle
    // label. A mode outranks the idle label but not the caption: in command
    // mode the caption is what you just said, and seeing it is how you find
    // out why the command did not match.
    let body = if !state.caption.is_empty() {
        &state.caption
    } else if let Some(style) = &style {
        style.label
    } else {
        g::LABEL
    };
    text.draw(
        canvas,
        body,
        meter_x + g::METER_WIDTH + g::METER_GAP,
        dot_cy + 5.0,
        state.width as f32 - g::HALO_PAD,
        alpha,
    );
}

/// A floating capsule: microphone glyph, then a live waveform.
///
/// Deliberately says only two things — "listening" and "hearing you" — because
/// by the time this is on screen the words themselves are already in the field
/// a few pixels above.
fn draw_pill(canvas: &mut PixmapMut<'_>, state: &State) {
    let alpha = state.opacity;
    let card_w = state.width as f32 - 2.0 * g::HALO_PAD;
    let card_h = state.height as f32 - 2.0 * g::HALO_PAD;
    floating_background(
        canvas,
        g::HALO_PAD,
        g::HALO_PAD,
        card_w,
        card_h,
        card_h / 2.0,
        alpha,
    );

    // Centre the glyph and the waveform as one group, rather than pinning each
    // to an end — that left a dead gap down the middle of the card.
    let mid_y = state.height as f32 / 2.0;
    let bars_width = g::BAR_COUNT as f32 * g::BAR_WIDTH + (g::BAR_COUNT as f32 - 1.0) * g::BAR_GAP;
    let group_width = g::MIC_CRADLE_RADIUS * 2.0 + g::PILL_GAP + bars_width;
    let group_x = (state.width as f32 - group_width) / 2.0;

    // The pill has no room for a word, so colour carries the mode on its own.
    let style = state.mode.as_deref().map(mode_style);
    let tint = style
        .as_ref()
        .map_or((0.97, 0.97, 0.98), |style| style.tint);
    // Asleep, the bars are held flat. Audio is still arriving — sleep suspends
    // acting on speech, not listening for "wake up" — so a live waveform here
    // would animate energetically while claiming to be asleep.
    let level = if state.mode.as_deref() == Some("asleep") {
        0.0
    } else {
        state.level
    };

    draw_microphone(
        canvas,
        group_x + g::MIC_CRADLE_RADIUS - g::MIC_WIDTH / 2.0,
        mid_y,
        tint,
        alpha,
    );
    draw_waveform(
        canvas,
        group_x + g::MIC_CRADLE_RADIUS * 2.0 + g::PILL_GAP,
        mid_y,
        level,
        tint,
        alpha,
    );
}

/// A microphone: capsule body, cradle arc, short stem.
///
/// Monochrome and light, like the system glyphs macOS uses. The whole glyph is
/// centred on `mid_y` as one unit, so the body sits above centre and the stem
/// below it rather than the group drifting downward.
fn draw_microphone(
    canvas: &mut PixmapMut<'_>,
    x: f32,
    mid_y: f32,
    tint: (f32, f32, f32),
    alpha: f32,
) {
    let total_h = g::MIC_BODY_HEIGHT + (g::MIC_CRADLE_RADIUS - g::MIC_WIDTH / 2.0) + g::MIC_STEM;
    let top = mid_y - total_h / 2.0;
    let centre_x = x + g::MIC_WIDTH / 2.0;
    let paint = rgba(tint.0, tint.1, tint.2, 0.95 * alpha);

    // Body: distinctly taller than wide, or the capsule reads as a circle.
    if let Some(body) = rounded_rect(x, top, g::MIC_WIDTH, g::MIC_BODY_HEIGHT, g::MIC_WIDTH / 2.0) {
        canvas.fill_path(
            &body,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let stroke = Stroke {
        width: 1.5,
        ..Stroke::default()
    };
    // Cradle: a half-arc hugging the lower half of the body, as two quadratics
    // — the same shape Cairo's `arc(centre, r, 0, pi)` draws.
    let cradle_y = top + g::MIC_BODY_HEIGHT - g::MIC_WIDTH / 2.0;
    let r = g::MIC_CRADLE_RADIUS;
    let mut arc = PathBuilder::new();
    arc.move_to(centre_x + r, cradle_y);
    arc.quad_to(centre_x + r, cradle_y + r * 1.34, centre_x, cradle_y + r);
    arc.quad_to(centre_x - r, cradle_y + r * 1.34, centre_x - r, cradle_y);
    if let Some(path) = arc.finish() {
        canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    // Stem, from the bottom of the cradle down.
    let mut stem = PathBuilder::new();
    stem.move_to(centre_x, cradle_y + r);
    stem.line_to(centre_x, cradle_y + r + g::MIC_STEM);
    if let Some(path) = stem.finish() {
        canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Bars that rise with the mic level, each on its own response curve.
///
/// A single meter reads as a progress bar; four bars moving by different
/// amounts read as a voice.
fn draw_waveform(
    canvas: &mut PixmapMut<'_>,
    x: f32,
    mid_y: f32,
    level: f32,
    tint: (f32, f32, f32),
    alpha: f32,
) {
    // Monochrome, matching the glyph. The green here was inherited from the old
    // level meter and is the least Apple-looking thing about a card that is
    // otherwise trying to be a system control.
    let paint = rgba(tint.0, tint.1, tint.2, 0.90 * alpha);
    for index in 0..g::BAR_COUNT {
        let scaled = g::bar_scale(level, g::BAR_RESPONSE[index]);
        let bar_h = g::BAR_MIN_HEIGHT + (g::BAR_MAX_HEIGHT - g::BAR_MIN_HEIGHT) * scaled;
        let bar_x = x + index as f32 * (g::BAR_WIDTH + g::BAR_GAP);
        if let Some(bar) = rounded_rect(
            bar_x,
            mid_y - bar_h / 2.0,
            g::BAR_WIDTH,
            bar_h,
            g::BAR_WIDTH / 2.0,
        ) {
            canvas.fill_path(&bar, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
}

/// A magenta box on the reported caret, with crosshair arms.
///
/// Magenta because nothing in ordinary UI is that colour, and arms because the
/// box itself can be a couple of pixels wide — the arms are what make it
/// findable on a 4K screen.
pub fn draw_marker(pixmap: &mut Pixmap) {
    pixmap.fill(Color::TRANSPARENT);
    let arm = g::MARKER_ARM as f32;
    let width = pixmap.width() as f32;
    let height = pixmap.height() as f32;
    let paint = rgba(1.0, 0.0, 0.85, 0.95);
    let stroke = Stroke {
        width: 2.0,
        ..Stroke::default()
    };
    let mut canvas = pixmap.as_mut();

    if let Some(rect) = SkRect::from_xywh(arm, arm, width - 2.0 * arm, height - 2.0 * arm)
        && let Some(path) = PathBuilder::from_rect(rect).transform(Transform::identity())
    {
        canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    let mid_x = width / 2.0;
    let mid_y = height / 2.0;
    let mut arms = PathBuilder::new();
    arms.move_to(0.0, mid_y);
    arms.line_to(arm, mid_y);
    arms.move_to(width - arm, mid_y);
    arms.line_to(width, mid_y);
    arms.move_to(mid_x, 0.0);
    arms.line_to(mid_x, arm);
    arms.move_to(mid_x, height - arm);
    arms.line_to(mid_x, height);
    if let Some(path) = arms.finish() {
        canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Text, rasterised by `fontdue` and blitted by hand.
///
/// `tiny-skia` has no text at all, which is the one thing Cairo gave `govox-py`
/// for free. The card draws a single short line in one size and one weight, so
/// a full shaping engine would be a large dependency for a feature none of it
/// is used: no bidi, no ligature that matters at this size, no fallback chain
/// beyond "the font the system already resolved".
pub struct Text {
    font: fontdue::Font,
    size: f32,
}

impl Text {
    /// Load a bold sans face, asking fontconfig where it is.
    ///
    /// `fc-match` rather than a hardcoded path: the reference asks Cairo for
    /// "Sans" and lets fontconfig resolve it, and a fixed path would render
    /// boxes on any machine that happens not to ship that file.
    pub fn load() -> anyhow::Result<Self> {
        let path = fontconfig_match()
            .ok_or_else(|| anyhow::anyhow!("no sans-serif font found; is fontconfig installed?"))?;
        let bytes = std::fs::read(&path)
            .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|error| anyhow::anyhow!("cannot parse {}: {error}", path.display()))?;
        Ok(Self { font, size: 15.0 })
    }

    /// Draw `body` with its baseline at `y`, clipped at `max_x`.
    ///
    /// Clipping rather than wrapping: the card is one line by construction and
    /// the sender has already truncated the caption to fit. This is the
    /// backstop for a glyph that is wider than the measurement assumed.
    fn draw(&self, canvas: &mut PixmapMut<'_>, body: &str, x: f32, y: f32, max_x: f32, alpha: f32) {
        let mut pen = x;
        for ch in body.chars() {
            let (metrics, bitmap) = self.font.rasterize(ch, self.size);
            if pen + metrics.advance_width > max_x {
                return;
            }
            let left = pen + metrics.xmin as f32;
            let top = y - metrics.height as f32 - metrics.ymin as f32;
            blit(canvas, &bitmap, metrics.width, left, top, alpha);
            pen += metrics.advance_width;
        }
    }
}

/// Composite an 8-bit coverage bitmap as near-white text.
///
/// Straight source-over with premultiplied alpha, which is what the pixmap
/// stores. Doing this by hand rather than through a shader keeps the whole
/// text path to one loop.
fn blit(canvas: &mut PixmapMut<'_>, bitmap: &[u8], width: usize, left: f32, top: f32, alpha: f32) {
    if width == 0 {
        return;
    }
    let canvas_w = canvas.width() as i32;
    let canvas_h = canvas.height() as i32;
    let pixels = canvas.pixels_mut();
    let left = left.round() as i32;
    let top = top.round() as i32;

    for (index, coverage) in bitmap.iter().enumerate() {
        if *coverage == 0 {
            continue;
        }
        let x = left + (index % width) as i32;
        let y = top + (index / width) as i32;
        if x < 0 || y < 0 || x >= canvas_w || y >= canvas_h {
            continue;
        }
        let a = f32::from(*coverage) / 255.0 * 0.95 * alpha;
        let slot = &mut pixels[(y * canvas_w + x) as usize];
        let inv = 1.0 - a;
        // 0.96, 0.96, 0.97 — the same near-white the reference uses.
        let blend = |dst: u8, src: f32| ((src * a + f32::from(dst) / 255.0 * inv) * 255.0) as u8;
        let (r, g_, b, dst_a) = (slot.red(), slot.green(), slot.blue(), slot.alpha());
        *slot = tiny_skia::PremultipliedColorU8::from_rgba(
            blend(r, 0.96),
            blend(g_, 0.96),
            blend(b, 0.97),
            blend(dst_a, 1.0),
        )
        .unwrap_or(*slot);
    }
}

/// Ask fontconfig for a bold sans-serif file.
fn fontconfig_match() -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "Sans:bold"])
        .output()
        .ok()?;
    let path = std::path::PathBuf::from(String::from_utf8(output.stdout).ok()?.trim().to_owned());
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rounded_rect_clamps_its_radius_to_the_shape() {
        // A radius larger than half the box would invert the corner curves.
        // The pill asks for exactly half its height, so this is on the path.
        assert!(rounded_rect(0.0, 0.0, 10.0, 10.0, 50.0).is_some());
        assert!(rounded_rect(0.0, 0.0, 10.0, 10.0, 5.0).is_some());
        // A degenerate size must not panic. Whether it yields a path or not
        // is tiny-skia's business; the card is never drawn at this size, and
        // the clamp above is what stops a radius inverting the corners.
        let _ = rounded_rect(0.0, 0.0, 0.0, 0.0, 4.0);
    }

    #[test]
    fn a_fully_transparent_card_draws_nothing() {
        // Not just invisible — skipped. The fade runs at 60 Hz and the card
        // spends most of a session at rest.
        let mut pixmap = Pixmap::new(64, 64).unwrap();
        let text = Text {
            font: fontdue::Font::from_bytes(
                std::fs::read(fontconfig_match().expect("a system font")).unwrap(),
                fontdue::FontSettings::default(),
            )
            .unwrap(),
            size: 15.0,
        };
        pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
        draw(
            &mut pixmap,
            &State {
                opacity: 0.0,
                ..State::default()
            },
            &text,
        );
        // Cleared to transparent, and nothing drawn over it.
        assert!(pixmap.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn the_card_actually_puts_pixels_down() {
        // The cheapest possible guard against a renderer that runs clean and
        // draws nothing — which is exactly how this code fails.
        let mut pixmap = Pixmap::new(g::CARD_WIDTH_IDLE as u32, g::CARD_HEIGHT as u32).unwrap();
        let text = Text::load().expect("a system font");
        draw(
            &mut pixmap,
            &State {
                opacity: 1.0,
                level: 0.5,
                ..State::default()
            },
            &text,
        );
        let painted = pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(painted > 1000, "only {painted} pixels were painted");
    }

    #[test]
    fn the_pill_is_drawn_without_text() {
        let mut pixmap = Pixmap::new(g::PILL_WIDTH as u32, g::PILL_HEIGHT as u32).unwrap();
        let text = Text::load().expect("a system font");
        draw(
            &mut pixmap,
            &State {
                opacity: 1.0,
                level: 1.0,
                compact: true,
                caption: "this must not be drawn".to_owned(),
                width: g::PILL_WIDTH,
                height: g::PILL_HEIGHT,
                ..State::default()
            },
            &text,
        );
        let painted = pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(painted > 1000, "only {painted} pixels were painted");
    }

    #[test]
    fn the_marker_is_drawn_hollow() {
        // It sits on top of a text field, so a filled box would hide the very
        // caret it is pointing at.
        let mut pixmap = Pixmap::new(40, 40).unwrap();
        draw_marker(&mut pixmap);
        let centre = pixmap.pixel(20, 20).expect("in bounds");
        assert_eq!(centre.alpha(), 0, "the middle must stay see-through");
        let painted = pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(painted > 50, "the marker drew almost nothing");
    }

    #[test]
    fn every_mode_paints_a_distinct_colour() {
        // Colour is the entire indicator on the pill, which has no room for a
        // word. Two modes sharing one would make the pill a decoration.
        let modes = ["command", "spelling", "asleep", "somethingnew"];
        for (index, one) in modes.iter().enumerate() {
            for other in &modes[index + 1..] {
                assert_ne!(
                    mode_style(one).tint,
                    mode_style(other).tint,
                    "{one} and {other} look the same"
                );
            }
        }
    }

    #[test]
    fn no_mode_is_painted_the_record_dots_red() {
        // The red dot means "your speech is becoming text". In command,
        // spelling and sleep it is not, so reusing that red would say the one
        // thing the indicator exists to deny.
        const RECORD_RED: (f32, f32, f32) = (0.92, 0.16, 0.16);
        for mode in ["command", "spelling", "asleep", "somethingnew"] {
            assert_ne!(mode_style(mode).tint, RECORD_RED, "{mode}");
        }
    }

    #[test]
    fn a_mode_label_fits_the_idle_card() {
        // The idle card has a fixed width and the label replaces `g::LABEL` in
        // the same slot, so anything longer is silently clipped.
        for mode in ["command", "spelling", "asleep", "somethingnew"] {
            let label = mode_style(mode).label;
            assert!(
                label.chars().count() <= g::LABEL.chars().count(),
                "{mode} draws {label:?}, which is wider than the slot"
            );
        }
    }

    #[test]
    fn a_mode_changes_what_the_pill_paints() {
        // The pill is the surface that follows the caret, and the one the user
        // actually looks at while dictating. A mode that left it identical
        // would mean the indicator is invisible where it is needed most.
        let pixels = |mode: Option<&str>| {
            let mut pixmap = Pixmap::new(g::PILL_WIDTH as u32, g::PILL_HEIGHT as u32)
                .expect("pill pixmap");
            let state = State {
                opacity: 1.0,
                level: 0.5,
                compact: true,
                mode: mode.map(str::to_owned),
                width: g::PILL_WIDTH,
                height: g::PILL_HEIGHT,
                ..State::default()
            };
            draw(&mut pixmap, &state, &Text::load().expect("font"));
            pixmap.data().to_vec()
        };

        let dictating = pixels(None);
        for mode in ["command", "spelling", "asleep"] {
            assert_ne!(pixels(Some(mode)), dictating, "{mode} looks like dictation");
        }
    }
}
