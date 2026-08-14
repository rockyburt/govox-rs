//! Finding and reading the focused text field over AT-SPI.
//!
//! **Reading only.** Measured on the reference machine, GTK4 accepts
//! `insert_text` and `delete_text` in under 3 ms — but Chromium reports
//! `is_editable_text` false on fields it will happily let you *read*. A backend
//! that wrote through AT-SPI would work in GNOME Text Editor and silently do
//! nothing in Chrome, which is the exact failure class this project spends most
//! of its comments on. Writing stays with the injector, which behaves the same
//! everywhere.

use std::time::{Duration, Instant};

use atspi::proxy::accessible::AccessibleProxy;
use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{Interface, ObjectRefOwned, State};
use govox_core::domain::FieldSnapshot;

use crate::A11yError;

/// How long the search for the focused field may take.
///
/// A *time* budget rather than a node count, because the thing worth bounding
/// is how long a command waits, and nodes are a poor proxy for that.
///
/// This replaced a 400-node cap in `govox-py` that was a real bug: inherited
/// from a whole-desktop coverage probe, it was far too small for one window.
/// Logseq's tree runs past 800 nodes, so the walk gave up before reaching the
/// focused entry and reported "nothing readable" for an application that was
/// exposing exactly what was wanted.
///
/// The safe direction is **not** "bias low". Missing the field costs a silent
/// fallback indistinguishable from an application genuinely exposing nothing,
/// which is the hardest kind of failure to notice.
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(150);

/// Backstop against a pathological tree, not the primary limit.
///
/// Chromium pages reach into the tens of thousands of nodes; the deadline is
/// what stops those.
pub const NODE_CEILING: usize = 50_000;

/// How far up to climb looking for a node's toplevel.
///
/// A deeply nested widget in a web page is perhaps 30 levels down; the bound
/// stops a cyclic or broken parent chain from spinning.
const MAX_ANCESTRY: usize = 100;

/// gnome-shell's "Main stage" holds FOCUSED permanently and sorts first on the
/// bus, so a naive first-match search returns it every time and never reaches
/// the application being typed into. Skipped by name.
///
/// Nothing else is filtered by role: ACTIVE is the signal, and excluding
/// toplevels for calling themselves "window" rather than "frame" would silently
/// lose whichever toolkits do that.
const SHELL_APP: &str = "gnome-shell";

/// Reads the focused accessible.
pub struct FieldReader {
    connection: atspi::AccessibilityConnection,
    budget: Duration,
    ceiling: usize,
}

impl FieldReader {
    /// Connect to the accessibility bus.
    pub async fn connect() -> Result<Self, A11yError> {
        let connection = atspi::AccessibilityConnection::new()
            .await
            .map_err(|error| A11yError::NoBus(error.to_string()))?;
        Ok(Self {
            connection,
            budget: DEFAULT_BUDGET,
            ceiling: NODE_CEILING,
        })
    }

    pub(crate) fn connection(&self) -> &atspi::AccessibilityConnection {
        &self.connection
    }

    /// The focused field, or `None` when it cannot be read.
    ///
    /// `None` is the ordinary answer, not an error. A bus that has gone away,
    /// an application that dies mid-read, a toolkit that exposes a broken node
    /// — all mean the same thing to the caller, and turning any of them into a
    /// failure would make field access a dependency.
    pub async fn read(&self, tracked: Option<&ObjectRefOwned>) -> Option<FieldSnapshot> {
        let node = self.focused_text_node(tracked).await?;
        let text = node.proxies().await.ok()?.text().await.ok()?;

        let caret = text.caret_offset().await.ok()?;
        if caret < 0 {
            // An unfocused or stub implementation reports -1. Without a caret
            // there is nothing to compare "the text before it" against.
            return None;
        }
        let count = text.character_count().await.ok()?;
        let body = text.get_text(0, count).await.ok()?;
        Some(FieldSnapshot {
            text: body,
            // AT-SPI reports character offsets, which is what `CharIdx` means.
            caret: usize::try_from(caret).ok()?,
        })
    }

    /// A human label for the window a read would come from.
    ///
    /// Exists so a diagnostic can say *which* application answered. A probe
    /// that reports 6888 characters without naming the window is impossible to
    /// act on: the natural reading is "the app I clicked into", and the true
    /// answer may be the terminal the probe was launched from.
    pub async fn active_window(&self) -> Option<String> {
        let frame = self.active_frame().await?;
        let title = frame.name().await.unwrap_or_default();
        let title = if title.is_empty() {
            "(untitled)".to_owned()
        } else {
            title
        };
        let app = match frame.parent().await {
            Ok(parent) => match self.proxy(&parent).await {
                Ok(proxy) => proxy.name().await.unwrap_or_else(|_| "?".to_owned()),
                Err(_) => "?".to_owned(),
            },
            Err(_) => "?".to_owned(),
        };
        Some(format!("{app} / {title}"))
    }

    /// The focused, text-bearing node inside the active window.
    ///
    /// Both halves matter, and neither is sufficient alone:
    ///
    /// * FOCUSED alone is not enough. A GTK4 text view in an *inactive* window
    ///   reports FOCUSABLE, SHOWING and EDITABLE and reads perfectly — but the
    ///   keystrokes are not going there. Confirming "delete that" against a
    ///   window that will not receive the backspaces is worse than not reading
    ///   at all: no snapshot beats the wrong snapshot.
    /// * Scoping to the ACTIVE frame makes that a guarantee rather than a
    ///   coincidence of bus ordering, and it is far cheaper besides — one
    ///   window's subtree instead of every application on the desktop.
    async fn focused_text_node(
        &self,
        tracked: Option<&ObjectRefOwned>,
    ) -> Option<AccessibleProxy<'static>> {
        if let Some(node) = self.verified_tracked(tracked).await {
            return Some(node);
        }

        let frame = self.active_frame().await?;
        let deadline = Instant::now() + self.budget;
        let mut queue = vec![frame];
        let mut seen = 0_usize;

        while let Some(node) = pop_front(&mut queue) {
            if seen >= self.ceiling {
                break;
            }
            if Instant::now() >= deadline {
                // Ran out of time rather than out of tree. Worth
                // distinguishing: this is the one "no snapshot" that means
                // "look again", not "this application exposes nothing".
                tracing::debug!(
                    budget_ms = self.budget.as_millis(),
                    "AT-SPI focus search hit its budget; treating as unreadable"
                );
                return None;
            }
            seen += 1;

            if self.is_focused_text(&node).await {
                return Some(node);
            }

            // A dead or slow peer is that application's problem, not ours: keep
            // looking rather than failing the whole read.
            let Ok(children) = node.get_children().await else {
                continue;
            };
            for child in children {
                if let Ok(proxy) = self.proxy(&child).await {
                    queue.push(proxy);
                }
            }
        }
        None
    }

    /// The tracker's node, if it still passes the same checks as a walk.
    ///
    /// Re-verified rather than trusted. The tracker holds whatever last gained
    /// focus, which can be stale by the time a command arrives — the window may
    /// have changed without a new focus event, and the object may have gone
    /// away entirely. Both checks are a handful of round-trips against one
    /// known object, which is what makes this worth doing next to a tree walk.
    async fn verified_tracked(
        &self,
        tracked: Option<&ObjectRefOwned>,
    ) -> Option<AccessibleProxy<'static>> {
        let node = self.proxy(tracked?).await.ok()?;
        if !self.is_focused_text(&node).await {
            return None;
        }
        if !self.in_active_window(&node).await {
            return None;
        }
        Some(node)
    }

    /// Does this node carry FOCUSED *and* implement the text interface?
    async fn is_focused_text(&self, node: &AccessibleProxy<'_>) -> bool {
        let Ok(interfaces) = node.get_interfaces().await else {
            return false;
        };
        if !interfaces.contains(Interface::Text) {
            return false;
        }
        node.get_state()
            .await
            .is_ok_and(|states| states.contains(State::Focused))
    }

    /// Whether `node` sits under a toplevel carrying ACTIVE.
    ///
    /// Keeps the safety property the walk establishes: a window that is not
    /// receiving keystrokes must never answer for one that is.
    async fn in_active_window(&self, node: &AccessibleProxy<'_>) -> bool {
        let Ok(mut current) = ObjectRefOwned::try_from(node) else {
            return false;
        };
        for _ in 0..MAX_ANCESTRY {
            let Ok(proxy) = self.proxy(&current).await else {
                return false;
            };
            let Ok(parent) = proxy.parent().await else {
                return false;
            };
            let Ok(parent_proxy) = self.proxy(&parent).await else {
                return false;
            };
            if parent_proxy.get_role_name().await.as_deref() == Ok("application") {
                // The node one level *below* the application is the toplevel,
                // and ACTIVE is a property of that toplevel, not of the field.
                return proxy
                    .get_state()
                    .await
                    .is_ok_and(|states| states.contains(State::Active));
            }
            current = parent;
        }
        false
    }

    /// The toplevel the user is actually typing into.
    async fn active_frame(&self) -> Option<AccessibleProxy<'static>> {
        let desktop = self.connection.root_accessible_on_registry().await.ok()?;
        for app in desktop.get_children().await.ok()? {
            let Ok(app) = self.proxy(&app).await else {
                continue;
            };
            if app.name().await.as_deref() == Ok(SHELL_APP) {
                continue;
            }
            let Ok(frames) = app.get_children().await else {
                continue;
            };
            for frame in frames {
                let Ok(frame) = self.proxy(&frame).await else {
                    continue;
                };
                if frame
                    .get_state()
                    .await
                    .is_ok_and(|states| states.contains(State::Active))
                {
                    return Some(frame);
                }
            }
        }
        None
    }

    /// An `AccessibleProxy` for one object reference.
    async fn proxy(
        &self,
        object: &ObjectRefOwned,
    ) -> Result<AccessibleProxy<'static>, zbus::Error> {
        AccessibleProxy::builder(self.connection.connection())
            .cache_properties(zbus::proxy::CacheProperties::No)
            .destination(
                object
                    .name()
                    .ok_or(zbus::Error::MissingParameter("name"))?
                    .to_owned(),
            )?
            .path(object.path().to_owned())?
            .build()
            .await
    }
}

/// Breadth-first, so a shallow field is found before a deep one.
///
/// `Vec::remove(0)` on a queue that stays small is cheaper than the allocation
/// a `VecDeque` would save; the walk is bounded by time, not by queue length.
fn pop_front(queue: &mut Vec<AccessibleProxy<'static>>) -> Option<AccessibleProxy<'static>> {
    if queue.is_empty() {
        None
    } else {
        Some(queue.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_is_generous_enough_for_a_large_tree() {
        // 400 nodes was a shipped bug: Logseq's tree runs past 800 and the
        // walk gave up before reaching the focused entry. At 1000 nodes the
        // whole read still took under 100 ms, so 150 ms has real headroom.
        assert!(DEFAULT_BUDGET >= Duration::from_millis(100));
        // And still short enough that a command does not visibly stall.
        assert!(DEFAULT_BUDGET <= Duration::from_millis(300));
    }

    #[test]
    fn the_node_ceiling_is_a_backstop_not_the_limit() {
        // Chromium pages reach tens of thousands of nodes. A ceiling low
        // enough to bite before the deadline would reintroduce the 400-node
        // bug in a new disguise.
        const { assert!(NODE_CEILING >= 10_000) };
    }
}
