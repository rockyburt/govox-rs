//! Everything about the HUD that can be checked without a screen.
//!
//! The overlay is ~800 lines of rendering in `govox-py` of which almost none is
//! tested, and the reason is that it needs a display. So the parts that are
//! really *decisions* — how tall a bar is, which monitor the card belongs to,
//! where it sits under a caret, how far a glide moves in one frame — live here
//! as plain functions over plain numbers, and the drawing code is left with
//! nothing to decide.
//!
//! The ~40 tuning constants are ported verbatim. They are the accumulated
//! result of looking at the thing on a real screen, and none of them is
//! improvable from first principles.
// The renderer that consumes these is the remaining half of M12; until it
// lands most of them are referenced only by the tests below. `allow` rather
// than `expect` because which items count as dead shifts as the drawing code
// arrives, and an unfulfilled expectation would then be a build error that
// says nothing useful.
#![allow(dead_code)]

/// Toast-card geometry, in logical pixels.
///
/// The card is narrow at idle and widens only while a caption is showing, so it
/// stays unobtrusive the rest of the time.
pub const CARD_WIDTH_IDLE: i32 = 184;
pub const CARD_WIDTH_CAPTION: i32 = 340;
pub const CARD_HEIGHT: i32 = 52;
pub const CARD_RADIUS: f32 = 14.0;
pub const DOT_RADIUS: f32 = 7.0;
pub const MARGIN: i32 = 24;

/// Compact: a floating capsule with a microphone glyph and a live waveform, and
/// no text at all.
///
/// Used while the focused field is itself showing the dictation as preedit —
/// the words are already on screen, so repeating them in the HUD is noise. This
/// is the shape macOS Dictation uses: a small pill near the insertion point
/// that says "listening, and hearing you", nothing more.
pub const PILL_WIDTH: i32 = 171;
pub const PILL_HEIGHT: i32 = 96;

/// The card floats over applications govox knows nothing about, so it cannot
/// rely on contrast with any particular background. It carries both edges: a
/// soft dark halo, which separates it from light backgrounds, and a bright rim,
/// which separates it from dark ones. Either alone disappears against
/// something — a white border is invisible on white, a shadow invisible on
/// black.
///
/// The halo is drawn *inside* the window, so these pixels are reserved for it
/// and the visible card is inset by this much on every side.
pub const HALO_PAD: f32 = 4.0;
pub const HALO_STEPS: u32 = 5;

/// Microphone glyph, drawn rather than shipped as an asset.
///
/// The body must be clearly taller than it is wide: at anything near square, a
/// capsule with a half-width corner radius renders as a plain circle, and the
/// glyph reads as a face rather than a microphone.
pub const MIC_WIDTH: f32 = 16.5;
pub const MIC_BODY_HEIGHT: f32 = 26.0;
pub const MIC_CRADLE_RADIUS: f32 = 12.9;
pub const MIC_STEM: f32 = 6.9;

/// Waveform bars: how many, how wide, how far apart, and the height they span
/// between silence and a full-scale signal.
pub const BAR_COUNT: usize = 4;
pub const BAR_WIDTH: f32 = 6.0;
pub const BAR_GAP: f32 = 7.5;
pub const BAR_MIN_HEIGHT: f32 = 6.0;
pub const BAR_MAX_HEIGHT: f32 = 60.0;

/// Gap between the microphone and the waveform. Kept tight so the two read as
/// one control rather than as two things at opposite ends of a slab.
pub const PILL_GAP: f32 = 19.5;

/// Each bar reacts to a different fraction of the level, so the group moves
/// like a waveform instead of a single block rising and falling.
pub const BAR_RESPONSE: [f32; BAR_COUNT] = [0.55, 1.0, 0.80, 0.40];

/// Speech spends most of its time in the bottom third of the meter, so a linear
/// mapping leaves the bars almost still: rendered side by side, levels 0.0 and
/// 0.1 were indistinguishable and 0.2 barely moved. Raising the level to a
/// power below 1 expands that crowded low end — 0.1 becomes 0.29, 0.2 becomes
/// 0.42 — which is what makes ordinary speech visibly drive the waveform.
pub const LEVEL_GAMMA: f32 = 0.55;

/// Headroom so the loudest bars reach full height without two of them pegging
/// there together, which would flatten the waveform at exactly the moment it
/// should be most animated.
pub const LEVEL_HEADROOM: f32 = 1.15;

/// Animation: one shared tick drives the fade and the glide, so the pill floats
/// to a new caret position instead of teleporting to it.
pub const ANIM_INTERVAL_MS: u64 = 16;
pub const FADE_STEP: f32 = 0.14;
pub const GLIDE_FACTOR: f32 = 0.28;
pub const GLIDE_SNAP_PX: f32 = 1.0;

/// How long to stay invisible waiting for a caret rectangle before giving up
/// and showing in the corner.
///
/// The input method's client reports the caret a few milliseconds after the
/// engine activates, so this is generously long; it is a backstop for clients
/// that never report at all, and short enough that a user who is already
/// speaking does not notice the card is late.
pub const ANCHOR_WAIT_MS: u64 = 250;

/// Caret-marker geometry. The reported rectangle can be zero pixels wide, so
/// the drawn box has a floor, and the crosshair arms extend outside it.
pub const MARKER_MIN_W: i32 = 3;
pub const MARKER_MIN_H: i32 = 8;
pub const MARKER_ARM: i32 = 10;

/// Gap between the caret's baseline and the card when anchored beneath it.
pub const CARET_GAP: i32 = 6;

pub const LABEL: &str = "Recording";
pub const PULSE_INTERVAL_MS: u64 = 650;

/// Level-meter bar geometry, sitting just right of the record dot.
pub const METER_WIDTH: f32 = 50.0;
pub const METER_HEIGHT: f32 = 6.0;
pub const METER_GAP: f32 = 10.0;

/// A rectangle in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Does this rectangle contain the point?
    ///
    /// Half-open on the far edges, matching the monitor-geometry test the
    /// reference does inline: a point on the right or bottom edge belongs to
    /// the next monitor along, not this one.
    #[must_use]
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Which corner the card sits in when there is no caret to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corner {
    pub right: bool,
    pub bottom: bool,
}

impl Corner {
    /// Parse `top-left` / `top-right` / `bottom-left` / `bottom-right`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "top-left" => Some(Self {
                right: false,
                bottom: false,
            }),
            "top-right" => Some(Self {
                right: true,
                bottom: false,
            }),
            "bottom-left" => Some(Self {
                right: false,
                bottom: true,
            }),
            "bottom-right" => Some(Self {
                right: true,
                bottom: true,
            }),
            _ => None,
        }
    }
}

/// How tall one waveform bar should be, as a fraction of its travel.
///
/// Pulled out of the drawing code because it is the whole feel of the meter and
/// the only part of it that can be checked without a screen.
#[must_use]
pub fn bar_scale(level: f32, response: f32) -> f32 {
    let expanded = level.max(0.0).powf(LEVEL_GAMMA);
    (expanded * response * LEVEL_HEADROOM).clamp(0.0, 1.0)
}

/// The card's size for the current mode.
///
/// Compact wins over everything: it means "there is nothing to show".
#[must_use]
pub fn card_size(compact: bool, has_caption: bool) -> (i32, i32) {
    if compact {
        return (PILL_WIDTH, PILL_HEIGHT);
    }
    let width = if has_caption {
        CARD_WIDTH_CAPTION
    } else {
        CARD_WIDTH_IDLE
    };
    (width, CARD_HEIGHT)
}

/// Where the card goes when it sits in its configured corner.
#[must_use]
pub fn corner_position(monitor: Rect, card: (i32, i32), corner: Corner) -> (i32, i32) {
    let (width, height) = card;
    let x = monitor.x
        + if corner.right {
            monitor.width - width - MARGIN
        } else {
            MARGIN
        };
    let y = monitor.y
        + if corner.bottom {
            monitor.height - height - MARGIN
        } else {
            MARGIN
        };
    (x, y)
}

/// Where the card goes beneath a caret, or `None` if the rectangle is unusable.
///
/// Clients are inconsistent about this: some never report, some report zeroes,
/// and some report coordinates in a space that does not match the X11 geometry
/// the overlay is positioned in. Rather than trust it, the rectangle is checked
/// against the monitor it claims to be on and rejected if it does not land
/// there — **a HUD in the wrong place is worse than one in a predictable
/// corner**.
#[must_use]
pub fn caret_position(caret: Rect, monitor: Rect, card: (i32, i32)) -> Option<(i32, i32)> {
    if !monitor.contains(caret.x, caret.y) {
        return None;
    }
    let (card_w, card_h) = card;

    // Below the caret, unless that would run off the bottom, in which case flip
    // above it — the same rule a candidate window follows.
    let mut y = caret.y + caret.height.max(0) + CARET_GAP;
    if y + card_h > monitor.y + monitor.height {
        y = caret.y - card_h - CARET_GAP;
    }
    let x = caret.x.clamp(monitor.x, monitor.x + monitor.width - card_w);
    let y = y.clamp(monitor.y, monitor.y + monitor.height - card_h);
    Some((x, y))
}

/// The monitor a point falls on, if any.
#[must_use]
pub fn monitor_at(monitors: &[Rect], x: i32, y: i32) -> Option<Rect> {
    monitors.iter().copied().find(|m| m.contains(x, y))
}

/// One frame of the fade.
///
/// Returns the new opacity and whether it is still moving.
#[must_use]
pub fn fade_step(opacity: f32, target: f32) -> (f32, bool) {
    if (opacity - target).abs() > 0.01 {
        let step = if target > opacity {
            FADE_STEP
        } else {
            -FADE_STEP
        };
        ((opacity + step).clamp(0.0, 1.0), true)
    } else {
        (target, false)
    }
}

/// One frame of the glide.
///
/// Returns the new position and whether it is still moving. Within
/// [`GLIDE_SNAP_PX`] it lands exactly, so the card does not creep for ever by
/// ever-smaller fractions of a pixel.
#[must_use]
pub fn glide_step(current: (f32, f32), target: (i32, i32)) -> ((f32, f32), bool) {
    let dx = target.0 as f32 - current.0;
    let dy = target.1 as f32 - current.1;
    if dx.abs() > GLIDE_SNAP_PX || dy.abs() > GLIDE_SNAP_PX {
        (
            (current.0 + dx * GLIDE_FACTOR, current.1 + dy * GLIDE_FACTOR),
            true,
        )
    } else {
        ((target.0 as f32, target.1 as f32), false)
    }
}

/// The marker window's outer size and origin for a reported caret rectangle.
///
/// A reported caret is often zero or one pixel wide — Chrome reports 0 where
/// GTK reports 11 — so the drawn box is widened to stay visible while keeping
/// its top-left exactly on the reported point.
#[must_use]
pub fn marker_geometry(caret: Rect) -> Rect {
    let width = caret.width.max(MARKER_MIN_W);
    let height = caret.height.max(MARKER_MIN_H);
    Rect::new(
        caret.x - MARKER_ARM,
        caret.y - MARKER_ARM,
        width + 2 * MARKER_ARM,
        height + 2 * MARKER_ARM,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the meter's feel ---------------------------------------------------

    #[test]
    fn the_gamma_curve_expands_the_quiet_end_where_speech_lives() {
        // The numbers named in the constant's comment, which is the whole
        // justification for the curve existing. Ordinary speech sits in the
        // bottom third, and a linear mapping left the bars visibly still.
        let expand = |level: f32| bar_scale(level, 1.0) / LEVEL_HEADROOM;
        assert!((expand(0.1) - 0.29).abs() < 0.01, "{}", expand(0.1));
        assert!((expand(0.2) - 0.42).abs() < 0.01, "{}", expand(0.2));
    }

    #[test]
    fn silence_is_flat_and_a_full_signal_is_capped() {
        assert_eq!(bar_scale(0.0, 1.0), 0.0);
        assert_eq!(bar_scale(1.0, 1.0), 1.0, "headroom must not overflow");
        // Negative levels cannot arrive from the daemon, but a clamp here is
        // cheaper than a NaN from powf on a negative base.
        assert_eq!(bar_scale(-1.0, 1.0), 0.0);
    }

    #[test]
    fn the_bars_do_not_move_together() {
        // Four bars moving by different amounts read as a voice; one meter
        // reads as a progress bar. If every response were equal the waveform
        // would be a single block rising and falling.
        let heights: Vec<f32> = BAR_RESPONSE.iter().map(|r| bar_scale(0.5, *r)).collect();
        let first = heights[0];
        assert!(
            heights.iter().any(|h| (h - first).abs() > 0.05),
            "{heights:?}"
        );
    }

    #[test]
    fn headroom_lets_the_loudest_bar_peg_before_the_quietest() {
        // The point of the headroom: at a loud-but-not-maximal level the
        // strongest bar is already at full height while the others are not,
        // so the waveform stays animated instead of flattening.
        assert_eq!(bar_scale(0.9, 1.0), 1.0);
        assert!(bar_scale(0.9, 0.40) < 1.0);
    }

    // --- sizing -------------------------------------------------------------

    #[test]
    fn compact_beats_a_caption() {
        // Compact means "the field is already showing the words", so a caption
        // that arrived first must not widen the pill back into a card.
        assert_eq!(card_size(true, true), (PILL_WIDTH, PILL_HEIGHT));
        assert_eq!(card_size(true, false), (PILL_WIDTH, PILL_HEIGHT));
        assert_eq!(card_size(false, true), (CARD_WIDTH_CAPTION, CARD_HEIGHT));
        assert_eq!(card_size(false, false), (CARD_WIDTH_IDLE, CARD_HEIGHT));
    }

    // --- placement ----------------------------------------------------------

    fn monitor() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn each_corner_insets_from_its_own_edges() {
        let card = (CARD_WIDTH_IDLE, CARD_HEIGHT);
        let at = |name: &str| corner_position(monitor(), card, Corner::parse(name).unwrap());
        assert_eq!(at("top-left"), (MARGIN, MARGIN));
        assert_eq!(at("top-right"), (1920 - CARD_WIDTH_IDLE - MARGIN, MARGIN));
        assert_eq!(at("bottom-left"), (MARGIN, 1080 - CARD_HEIGHT - MARGIN));
        assert_eq!(
            at("bottom-right"),
            (1920 - CARD_WIDTH_IDLE - MARGIN, 1080 - CARD_HEIGHT - MARGIN)
        );
    }

    #[test]
    fn a_corner_on_a_second_monitor_is_relative_to_that_monitor() {
        // The bug this guards: treating monitor-local geometry as global put
        // the card at a height that existed on neither screen.
        let right_hand = Rect::new(1920, 0, 1080, 1920);
        let card = (CARD_WIDTH_IDLE, CARD_HEIGHT);
        let corner = Corner::parse("top-left").unwrap();
        assert_eq!(
            corner_position(right_hand, card, corner),
            (1920 + MARGIN, MARGIN)
        );
    }

    #[test]
    fn the_card_sits_just_below_the_caret() {
        let caret = Rect::new(400, 300, 11, 26);
        let card = (PILL_WIDTH, PILL_HEIGHT);
        assert_eq!(
            caret_position(caret, monitor(), card),
            Some((400, 300 + 26 + CARET_GAP))
        );
    }

    #[test]
    fn a_caret_near_the_bottom_flips_the_card_above_it() {
        // The same rule a candidate window follows. Without it the card is
        // clamped to the screen edge and covers the text being dictated.
        let caret = Rect::new(400, 1040, 11, 26);
        let card = (PILL_WIDTH, PILL_HEIGHT);
        let (_, y) = caret_position(caret, monitor(), card).unwrap();
        assert!(y < 1040, "should be above the caret, got {y}");
        assert_eq!(y, 1040 - PILL_HEIGHT - CARET_GAP);
    }

    #[test]
    fn a_caret_near_the_right_edge_keeps_the_whole_card_on_screen() {
        let caret = Rect::new(1900, 300, 11, 26);
        let card = (PILL_WIDTH, PILL_HEIGHT);
        let (x, _) = caret_position(caret, monitor(), card).unwrap();
        assert_eq!(x, 1920 - PILL_WIDTH);
    }

    #[test]
    fn a_caret_that_is_not_on_the_monitor_is_refused() {
        // The load-bearing check. Some clients report coordinates in a space
        // that has nothing to do with X11's, and a HUD in the wrong place is
        // worse than one in a predictable corner — so this returns None and
        // the caller falls back rather than clamping nonsense onto a screen.
        let caret = Rect::new(9000, 9000, 11, 26);
        assert_eq!(
            caret_position(caret, monitor(), (PILL_WIDTH, PILL_HEIGHT)),
            None
        );
    }

    #[test]
    fn monitors_do_not_both_claim_a_shared_edge() {
        // Half-open bounds: the pixel column at x=1920 belongs to the right
        // monitor alone, or a caret there would resolve to whichever monitor
        // the X server happened to list first.
        let monitors = [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1080, 1920)];
        assert_eq!(monitor_at(&monitors, 1919, 10), Some(monitors[0]));
        assert_eq!(monitor_at(&monitors, 1920, 10), Some(monitors[1]));
        assert_eq!(monitor_at(&monitors, -1, 10), None);
    }

    // --- animation ----------------------------------------------------------

    #[test]
    fn a_fade_reaches_its_target_and_then_reports_settled() {
        let mut opacity = 0.0;
        let mut frames = 0;
        loop {
            let (next, busy) = fade_step(opacity, 1.0);
            opacity = next;
            frames += 1;
            if !busy {
                break;
            }
            assert!(frames < 100, "the fade never settled");
        }
        assert_eq!(opacity, 1.0);
        // ~8 frames at 16 ms is about an eighth of a second: quick enough not
        // to feel laggy, slow enough to read as floating in rather than
        // snapping into existence.
        assert!((6..=10).contains(&frames), "{frames} frames");
    }

    #[test]
    fn a_glide_lands_exactly_rather_than_creeping_for_ever() {
        // Geometric approach never *arrives*; without the snap the card would
        // keep moving by ever-smaller fractions and the animation timer would
        // never stop, which is a permanent 60 Hz wakeup for nothing.
        let mut position = (0.0_f32, 0.0_f32);
        let mut frames = 0;
        loop {
            let (next, busy) = glide_step(position, (500, 300));
            position = next;
            frames += 1;
            if !busy {
                break;
            }
            assert!(frames < 200, "the glide never settled");
        }
        assert_eq!(position, (500.0, 300.0));
    }

    #[test]
    fn a_glide_already_at_its_target_does_no_work() {
        let (position, busy) = glide_step((100.0, 100.0), (100, 100));
        assert!(!busy);
        assert_eq!(position, (100.0, 100.0));
    }

    // --- the caret marker ---------------------------------------------------

    #[test]
    fn a_zero_width_caret_still_draws_a_findable_marker() {
        // Chrome reports width 0 where GTK reports 11. The box is widened to
        // stay visible while its top-left stays exactly on the reported point.
        let marker = marker_geometry(Rect::new(400, 300, 0, 0));
        assert_eq!(marker.x, 400 - MARKER_ARM);
        assert_eq!(marker.y, 300 - MARKER_ARM);
        assert_eq!(marker.width, MARKER_MIN_W + 2 * MARKER_ARM);
        assert_eq!(marker.height, MARKER_MIN_H + 2 * MARKER_ARM);
    }
}
