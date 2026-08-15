//! The window itself: an override-redirect X11 popup with an ARGB visual.
//!
//! `govox-py` gets this from GTK3, which supplies a window, an RGBA visual and
//! a main loop and is otherwise uninvolved — the card is drawn entirely in
//! Cairo. The GTK3 Rust bindings are unmaintained and GTK4 removed both the
//! override-redirect control and the input-shape API this depends on, so
//! talking to X directly is both the smaller dependency and the more faithful
//! one.
//!
//! Three properties make the card a HUD rather than a window:
//!
//! 1. **Override-redirect**, so the window manager does not decorate, stack or
//!    focus it. Without it the card takes focus away from whatever the user is
//!    dictating into, which is the one thing it must never do.
//! 2. **A 32-bit visual**, so the rounded corners and the halo are actually
//!    transparent rather than composited against black.
//! 3. **An empty input region** via the SHAPE extension, so clicks land on the
//!    application underneath. With `--click-to-stop` the region becomes the
//!    card's own bounds and clicks on it stop dictation; clicks anywhere else
//!    still pass through.

use anyhow::Context as _;
use x11rb::connection::Connection as _;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::geometry::Rect;

/// A click-through, always-on-top card.
pub struct Overlay {
    connection: RustConnection,
    screen: usize,
    window: xproto::Window,
    gc: xproto::Gcontext,
    width: u16,
    height: u16,
    mapped: bool,
    clickable: bool,
}

impl Overlay {
    /// Create the window, hidden.
    pub fn new(width: i32, height: i32, clickable: bool) -> anyhow::Result<Self> {
        let (connection, screen) =
            RustConnection::connect(None).context("cannot reach an X server (is DISPLAY set?)")?;
        let root = connection.setup().roots[screen].root;
        let (depth, visual) = argb_visual(&connection, screen)
            .context("no 32-bit visual: the card cannot be transparent on this display")?;

        let colormap = connection.generate_id()?;
        connection.create_colormap(xproto::ColormapAlloc::NONE, colormap, root, visual)?;

        let window = connection.generate_id()?;
        let values = xproto::CreateWindowAux::new()
            // Both are required with a non-default depth, or CreateWindow fails
            // with BadMatch — the values are inherited from the parent
            // otherwise, and the root is 24-bit.
            .background_pixel(0)
            .border_pixel(0)
            .colormap(colormap)
            // The whole point: no decoration, no focus, no stacking policy.
            .override_redirect(1)
            .event_mask(xproto::EventMask::EXPOSURE | xproto::EventMask::BUTTON_PRESS);
        connection.create_window(
            depth,
            window,
            root,
            0,
            0,
            u16::try_from(width).unwrap_or(1),
            u16::try_from(height).unwrap_or(1),
            0,
            xproto::WindowClass::INPUT_OUTPUT,
            visual,
            &values,
        )?;

        let gc = connection.generate_id()?;
        connection.create_gc(gc, window, &xproto::CreateGCAux::new())?;

        let overlay = Self {
            connection,
            screen,
            window,
            gc,
            width: u16::try_from(width).unwrap_or(1),
            height: u16::try_from(height).unwrap_or(1),
            mapped: false,
            clickable,
        };
        overlay.declare_kind()?;
        overlay.apply_input_region()?;
        overlay.connection.flush()?;
        Ok(overlay)
    }

    /// Tell the compositor what this is, for the cases override-redirect misses.
    ///
    /// A compositing manager still decides whether to fade, shadow or blur a
    /// surface, and it reads these hints to do it. Declaring NOTIFICATION and
    /// ABOVE gets the card treated like a notification rather than like a
    /// stray toplevel someone forgot to decorate.
    fn declare_kind(&self) -> anyhow::Result<()> {
        let atom = |name: &str| -> anyhow::Result<xproto::Atom> {
            Ok(self
                .connection
                .intern_atom(false, name.as_bytes())?
                .reply()?
                .atom)
        };
        let window_type = atom("_NET_WM_WINDOW_TYPE")?;
        let notification = atom("_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
        self.connection.change_property32(
            xproto::PropMode::REPLACE,
            self.window,
            window_type,
            xproto::AtomEnum::ATOM,
            &[notification],
        )?;
        let state = atom("_NET_WM_STATE")?;
        let above = atom("_NET_WM_STATE_ABOVE")?;
        let sticky = atom("_NET_WM_STATE_STICKY")?;
        let skip_taskbar = atom("_NET_WM_STATE_SKIP_TASKBAR")?;
        let skip_pager = atom("_NET_WM_STATE_SKIP_PAGER")?;
        self.connection.change_property32(
            xproto::PropMode::REPLACE,
            self.window,
            state,
            xproto::AtomEnum::ATOM,
            &[above, sticky, skip_taskbar, skip_pager],
        )?;
        Ok(())
    }

    /// Empty region — every click falls through — unless click-to-stop is on.
    ///
    /// This is what stops the card being a hole in the desktop. It floats over
    /// an application the user is typing into, so a card that swallowed clicks
    /// would make part of their window unusable for as long as dictation ran.
    fn apply_input_region(&self) -> anyhow::Result<()> {
        let rectangles = if self.clickable {
            vec![xproto::Rectangle {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            }]
        } else {
            Vec::new()
        };
        // An empty rectangle list *is* the empty region; no XFixes region
        // object is needed for either case.
        self.connection.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            xproto::ClipOrdering::UNSORTED,
            self.window,
            0,
            0,
            &rectangles,
        )?;
        Ok(())
    }

    /// Resize, keeping the input region in step with the new bounds.
    pub fn resize(&mut self, width: i32, height: i32) -> anyhow::Result<()> {
        let (width, height) = (
            u16::try_from(width).unwrap_or(1).max(1),
            u16::try_from(height).unwrap_or(1).max(1),
        );
        if width == self.width && height == self.height {
            return Ok(());
        }
        self.width = width;
        self.height = height;
        self.connection.configure_window(
            self.window,
            &xproto::ConfigureWindowAux::new()
                .width(u32::from(width))
                .height(u32::from(height)),
        )?;
        self.apply_input_region()?;
        Ok(())
    }

    pub fn move_to(&self, x: i32, y: i32) -> anyhow::Result<()> {
        self.connection
            .configure_window(self.window, &xproto::ConfigureWindowAux::new().x(x).y(y))?;
        Ok(())
    }

    pub fn show(&mut self) -> anyhow::Result<()> {
        if !self.mapped {
            self.connection.map_window(self.window)?;
            self.mapped = true;
        }
        // Re-assert stacking on every show: override-redirect keeps the window
        // manager out, which also means nothing else is keeping the card on
        // top as other windows are raised.
        self.connection.configure_window(
            self.window,
            &xproto::ConfigureWindowAux::new().stack_mode(xproto::StackMode::ABOVE),
        )?;
        Ok(())
    }

    pub fn hide(&mut self) -> anyhow::Result<()> {
        if self.mapped {
            self.connection.unmap_window(self.window)?;
            self.mapped = false;
        }
        Ok(())
    }

    /// Push a rendered frame to the server.
    ///
    /// `tiny-skia` stores premultiplied RGBA; X11 wants BGRA for a depth-32
    /// ZPixmap on a little-endian server, so the two colour channels are
    /// swapped on the way out. Getting this wrong is not subtle — the card
    /// comes out blue.
    pub fn present(&self, pixmap: &tiny_skia::Pixmap) -> anyhow::Result<()> {
        let mut data = pixmap.data().to_vec();
        for pixel in data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        self.connection.put_image(
            xproto::ImageFormat::Z_PIXMAP,
            self.window,
            self.gc,
            self.width,
            self.height,
            0,
            0,
            0,
            32,
            &data,
        )?;
        self.connection.flush()?;
        Ok(())
    }

    /// Every monitor the server knows about.
    ///
    /// RandR rather than the X screen, which reports one merged rectangle
    /// spanning every display — placing the card in "the top-right corner" of
    /// that is the far corner of the rightmost monitor, which is not what
    /// anyone means.
    pub fn monitors(&self) -> Vec<Rect> {
        let root = self.connection.setup().roots[self.screen].root;
        let monitors = self
            .connection
            .randr_get_monitors(root, true)
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        match monitors {
            Some(reply) => reply
                .monitors
                .iter()
                .map(|m| Rect::new(m.x.into(), m.y.into(), m.width.into(), m.height.into()))
                .collect(),
            None => {
                // A server without RandR still has one screen, and one screen
                // is a perfectly good answer.
                let screen = &self.connection.setup().roots[self.screen];
                vec![Rect::new(
                    0,
                    0,
                    screen.width_in_pixels.into(),
                    screen.height_in_pixels.into(),
                )]
            }
        }
    }

    /// The desktop work area — the screen minus panels and docks.
    ///
    /// `_NET_WORKAREA` is a list of `x, y, width, height` per virtual desktop;
    /// only the first is read, because the card is placed for the desktop the
    /// user is looking at and every desktop reserves the same struts in
    /// practice.
    ///
    /// `None` on any failure, which the caller treats as "use the whole
    /// monitor". A window manager that sets no work area is a normal thing to
    /// meet, not an error: the property is a hint, and losing it costs a card
    /// that sits where it always used to.
    ///
    /// Queried per placement, like [`Self::monitors`], rather than cached. A
    /// panel can appear, hide or change size while govox runs, and a cached
    /// answer would put the card under it until the next restart.
    pub fn work_area(&self) -> Option<Rect> {
        let root = self.connection.setup().roots[self.screen].root;
        let atom = self
            .connection
            .intern_atom(false, b"_NET_WORKAREA")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let reply = self
            .connection
            .get_property(false, root, atom, xproto::AtomEnum::CARDINAL, 0, 4)
            .ok()?
            .reply()
            .ok()?;

        let mut values = reply.value32()?;
        let x = values.next()?;
        let y = values.next()?;
        let width = values.next()?;
        let height = values.next()?;
        // A zero-sized work area is a malformed hint, not an instruction to
        // place the card in a rectangle with no room in it.
        if width == 0 || height == 0 {
            return None;
        }
        Some(Rect::new(
            i32::try_from(x).ok()?,
            i32::try_from(y).ok()?,
            i32::try_from(width).ok()?,
            i32::try_from(height).ok()?,
        ))
    }

    /// Where the pointer is, for choosing the corner monitor.
    ///
    /// The pointer's monitor, not the primary one: "primary" is a fixed screen
    /// that has nothing to do with where the user is working, so on a
    /// multi-monitor desk the card appeared on a different display from the
    /// application being dictated into.
    pub fn pointer(&self) -> Option<(i32, i32)> {
        let root = self.connection.setup().roots[self.screen].root;
        let reply = self.connection.query_pointer(root).ok()?.reply().ok()?;
        Some((reply.root_x.into(), reply.root_y.into()))
    }

    /// Drain pending events, reporting whether the card was clicked.
    pub fn poll_clicked(&self) -> bool {
        let mut clicked = false;
        while let Ok(Some(event)) = self.connection.poll_for_event() {
            if matches!(event, x11rb::protocol::Event::ButtonPress(_)) {
                clicked = true;
            }
        }
        clicked
    }
}

/// A visual with an alpha channel, if the display has one.
fn argb_visual(connection: &RustConnection, screen: usize) -> Option<(u8, xproto::Visualid)> {
    connection.setup().roots[screen]
        .allowed_depths
        .iter()
        .find(|depth| depth.depth == 32)
        .and_then(|depth| depth.visuals.first().map(|v| (depth.depth, v.visual_id)))
}
