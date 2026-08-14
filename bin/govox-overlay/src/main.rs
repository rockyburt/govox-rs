//! On-screen dictation HUD, driven over a line protocol on stdin.
//!
//! Deliberately a separate process rather than a thread. In `govox-py` that was
//! forced — GDK is single-backend per process and the tray already held the
//! Wayland connection — but the reason that replaced it is better: this is the
//! least-tested, most crash-prone code in the project, and out-of-process means
//! an overlay crash cannot take dictation down.
//!
//! The window is an override-redirect X11 popup on XWayland with an ARGB
//! visual, drawn entirely by hand with `tiny-skia`, using an empty XShape input
//! region for click-through. See [`x11`] for why each of those matters.
//!
//! Protocol, newline-delimited and byte-identical to the Python helper's:
//! `show` `pulse` `hide` `level <0-1>` `caption <text>` `anchor <x> <y> <w> <h>`
//! `expect-anchor` `caret-marker 0|1` `compact 0|1` `quit` on stdin; `stop` on
//! stdout when the card is clicked.

mod geometry;
mod protocol;
mod render;
mod x11;

use std::io::{BufRead as _, Write as _};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use geometry as g;
use protocol::Command;

fn main() -> anyhow::Result<()> {
    let mut position = "top-right".to_owned();
    let mut click_to_stop = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--position" => position = args.next().unwrap_or_else(|| "top-right".to_owned()),
            "--click-to-stop" => click_to_stop = true,
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }
    let corner = g::Corner::parse(&position)
        .ok_or_else(|| anyhow::anyhow!("unknown --position {position:?}"))?;

    Hud::new(corner, click_to_stop)?.run()
}

/// The card, its window, and everything it is currently being told.
struct Hud {
    window: x11::Overlay,
    marker: Option<x11::Overlay>,
    text: render::Text,
    pixmap: tiny_skia::Pixmap,
    state: render::State,
    corner: g::Corner,
    click_to_stop: bool,

    visible: bool,
    opacity_target: f32,
    /// Where the card is now, in floating point so the glide has somewhere to
    /// accumulate. `None` until it has been placed once.
    position: Option<(f32, f32)>,
    position_target: Option<(i32, i32)>,
    anchor: Option<g::Rect>,
    /// Set when the daemon says a caret rectangle is coming. While it is true
    /// the card stays transparent, so it appears *under* the caret rather than
    /// fading in at the corner and then gliding across the screen.
    awaiting_anchor: Option<Instant>,
    caret_marker: bool,
}

impl Hud {
    fn new(corner: g::Corner, click_to_stop: bool) -> anyhow::Result<Self> {
        let state = render::State::default();
        let window = x11::Overlay::new(state.width, state.height, click_to_stop)?;
        let pixmap = tiny_skia::Pixmap::new(state.width as u32, state.height as u32)
            .ok_or_else(|| anyhow::anyhow!("cannot allocate the card's pixmap"))?;
        Ok(Self {
            window,
            marker: None,
            text: render::Text::load()?,
            pixmap,
            state,
            corner,
            click_to_stop,
            visible: false,
            opacity_target: 0.0,
            position: None,
            position_target: None,
            anchor: None,
            awaiting_anchor: None,
            caret_marker: false,
        })
    }

    /// Read commands on one thread, animate on this one.
    ///
    /// stdin is blocking and the animation is a 60 Hz timer, so they cannot
    /// share a thread without one starving the other. The reference solves this
    /// with GLib's `io_add_watch` on its main loop; a channel is the same idea
    /// with less machinery.
    fn run(mut self) -> anyhow::Result<()> {
        let (commands, incoming) = mpsc::channel();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if commands.send(line).is_err() {
                    break;
                }
            }
            // EOF means the daemon is gone. Quitting is the only sane response:
            // a HUD with nothing driving it would sit on screen for ever.
            let _ = commands.send("quit".to_owned());
        });

        loop {
            match incoming.try_recv() {
                Ok(line) => {
                    if let Some(command) = Command::parse(&line)
                        && !self.handle(command)?
                    {
                        return Ok(());
                    }
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => {}
            }

            if self.click_to_stop && self.window.poll_clicked() {
                let mut out = std::io::stdout();
                let _ = writeln!(out, "stop");
                let _ = out.flush();
            }
            self.expire_anchor_wait();
            self.tick()?;
            std::thread::sleep(Duration::from_millis(g::ANIM_INTERVAL_MS));
        }
    }

    /// Apply one command. `false` means quit.
    fn handle(&mut self, command: Command) -> anyhow::Result<bool> {
        match command {
            Command::ExpectAnchor => {
                self.awaiting_anchor = Some(Instant::now());
            }
            Command::Show => {
                self.visible = true;
                self.state.pulse_on = true;
                self.window.show()?;
                // Fade in rather than snapping into existence — unless a caret
                // is still on its way, because fading in now would show the
                // card at its corner and the anchor a moment later would drag
                // it across the screen.
                if self.awaiting_anchor.is_none() {
                    self.opacity_target = 1.0;
                }
            }
            Command::Hide => {
                self.visible = false;
                self.state.has_level_feed = false;
                self.state.level = 0.0;
                self.state.caption.clear();
                self.state.compact = false;
                self.anchor = None;
                // A session that ends while still waiting must not leave the
                // flag set, or the next `show` would be held back by a wait
                // that already belongs to a finished session.
                self.awaiting_anchor = None;
                self.hide_marker()?;
                self.resize()?;
                // Fade out; `tick` unmaps once it reaches zero, so the card
                // never blinks out mid-fade.
                self.opacity_target = 0.0;
            }
            Command::Anchor(rect) => {
                self.anchor = rect;
                // Placing without a glide while still invisible is what makes
                // the card appear *at* the caret. Once on screen, a caret that
                // moves is followed smoothly.
                let immediate = self.state.opacity <= 0.0;
                self.reposition(immediate)?;
                self.resolve_anchor_wait();
                self.update_marker()?;
            }
            Command::CaretMarker(on) => {
                self.caret_marker = on;
                self.update_marker()?;
            }
            Command::Compact(on) => {
                self.state.compact = on;
                self.resize()?;
            }
            Command::Level(level) => {
                self.state.level = level;
                self.state.has_level_feed = true;
            }
            Command::Caption(text) => {
                self.state.caption = text;
                self.resize()?;
            }
            Command::Quit => return Ok(false),
        }
        Ok(true)
    }

    /// Give up waiting for a caret and show in the corner.
    fn expire_anchor_wait(&mut self) {
        if let Some(since) = self.awaiting_anchor
            && since.elapsed() >= Duration::from_millis(g::ANCHOR_WAIT_MS)
        {
            self.awaiting_anchor = None;
            if self.visible {
                self.opacity_target = 1.0;
            }
        }
    }

    fn resolve_anchor_wait(&mut self) {
        if self.awaiting_anchor.take().is_some() && self.visible {
            self.opacity_target = 1.0;
        }
    }

    /// Match the window to the card's current mode, and re-place it.
    fn resize(&mut self) -> anyhow::Result<()> {
        let (width, height) = g::card_size(self.state.compact, !self.state.caption.is_empty());
        if width == self.state.width && height == self.state.height {
            return Ok(());
        }
        self.state.width = width;
        self.state.height = height;
        self.pixmap = tiny_skia::Pixmap::new(width as u32, height as u32)
            .ok_or_else(|| anyhow::anyhow!("cannot allocate a {width}x{height} pixmap"))?;
        self.window.resize(width, height)?;
        // A size change moves the anchor point, so re-place at once rather than
        // gliding — the pill would otherwise drift from a stale position.
        self.reposition(true)
    }

    /// Sit beneath the caret when we know where it is, else in the corner.
    fn reposition(&mut self, immediate: bool) -> anyhow::Result<()> {
        let monitors = self.window.monitors();
        let card = (self.state.width, self.state.height);

        if let Some(caret) = self.anchor
            && let Some(monitor) = g::monitor_at(&monitors, caret.x, caret.y)
            && let Some((x, y)) = g::caret_position(caret, monitor, card)
        {
            return self.place(x, y, immediate);
        }

        let monitor = self
            .window
            .pointer()
            .and_then(|(x, y)| g::monitor_at(&monitors, x, y))
            .or_else(|| monitors.first().copied())
            .unwrap_or(g::Rect::new(0, 0, 1920, 1080));
        let (x, y) = g::corner_position(monitor, card, self.corner);
        // The corner is a fixed home, not somewhere to drift to: landing there
        // at once avoids a long diagonal glide when the caret is simply lost.
        self.place(x, y, immediate || self.anchor.is_none())
    }

    fn place(&mut self, x: i32, y: i32, immediate: bool) -> anyhow::Result<()> {
        self.position_target = Some((x, y));
        if immediate || self.position.is_none() {
            self.position = Some((x as f32, y as f32));
            self.window.move_to(x, y)?;
        }
        Ok(())
    }

    /// Advance the fade and the glide by one frame, then redraw.
    fn tick(&mut self) -> anyhow::Result<()> {
        let (opacity, fading) = g::fade_step(self.state.opacity, self.opacity_target);
        self.state.opacity = opacity;
        if !fading && opacity == 0.0 && !self.visible {
            self.window.hide()?;
            return Ok(());
        }

        let mut gliding = false;
        if let (Some(current), Some(target)) = (self.position, self.position_target) {
            let (next, moving) = g::glide_step(current, target);
            self.position = Some(next);
            gliding = moving;
            if moving {
                self.window
                    .move_to(next.0.round() as i32, next.1.round() as i32)?;
            }
        }

        // Redraw whenever anything could have changed. The level meter moves
        // on its own, so "nothing is animating" is not the same as "nothing
        // needs drawing" — but a hidden card is genuinely free.
        if self.visible || fading || gliding || opacity > 0.0 {
            render::draw(&mut self.pixmap, &self.state, &self.text);
            self.window.present(&self.pixmap)?;
        }
        Ok(())
    }

    /// The diagnostic box on the caret the client reported.
    fn update_marker(&mut self) -> anyhow::Result<()> {
        let Some(caret) = self.anchor.filter(|_| self.caret_marker) else {
            return self.hide_marker();
        };
        let geometry = g::marker_geometry(caret);
        let window = match self.marker.as_mut() {
            Some(window) => window,
            None => {
                // Never clickable: it sits directly on top of a text field.
                self.marker = Some(x11::Overlay::new(geometry.width, geometry.height, false)?);
                self.marker.as_mut().expect("just created")
            }
        };
        window.resize(geometry.width, geometry.height)?;
        window.move_to(geometry.x, geometry.y)?;
        window.show()?;
        let mut pixmap = tiny_skia::Pixmap::new(geometry.width as u32, geometry.height as u32)
            .ok_or_else(|| anyhow::anyhow!("cannot allocate the marker's pixmap"))?;
        render::draw_marker(&mut pixmap);
        window.present(&pixmap)
    }

    fn hide_marker(&mut self) -> anyhow::Result<()> {
        if let Some(window) = self.marker.as_mut() {
            window.hide()?;
        }
        Ok(())
    }
}
