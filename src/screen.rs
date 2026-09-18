//! Per-display detection.
//!
//! "Active display" is the screen the user is currently working on. Used to
//! scope hide_previous (only minimize prev frontmost windows on the display
//! the target window is being summoned to) and to disambiguate visible-window
//! picks when the user's cursor history doesn't determine one.
//!
//! Coordinates: Quartz (top-left origin, y grows down). Matches the space
//! returned by AX position attributes and `CGDisplay::bounds`, so window
//! frames and display bounds compare directly without flipping.

use core_graphics::display::{CGDisplay, CGDirectDisplayID};
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect};

use crate::{app, window};

pub type DisplayId = CGDirectDisplayID;

/// The display the user is currently "on" — defined as the display
/// containing the frontmost app's focused window. Falls back to the display
/// under the mouse cursor when no app has a focused window (e.g. only the
/// Finder desktop is focused). Last-resort fallback is the main display.
pub fn active_display() -> DisplayId {
    focused_window_display()
        .or_else(mouse_display)
        .unwrap_or_else(|| CGDisplay::main().id)
}

/// Display containing the centre of `win`'s frame. None when AX refuses to
/// hand back the window's position/size, or the centre lies outside every
/// active display.
pub fn window_display(win: &window::WindowEl) -> Option<DisplayId> {
    let frame = window::frame(win)?;
    display_for_point(centre(frame))
}

fn centre(r: CGRect) -> CGPoint {
    CGPoint::new(
        r.origin.x + r.size.width / 2.0,
        r.origin.y + r.size.height / 2.0,
    )
}

fn display_for_point(p: CGPoint) -> Option<DisplayId> {
    let ids = CGDisplay::active_displays().ok()?;
    ids.into_iter().find(|&id| contains(CGDisplay::new(id).bounds(), p))
}

fn contains(b: CGRect, p: CGPoint) -> bool {
    p.x >= b.origin.x
        && p.x < b.origin.x + b.size.width
        && p.y >= b.origin.y
        && p.y < b.origin.y + b.size.height
}

fn focused_window_display() -> Option<DisplayId> {
    let pid = app::frontmost_pid()?;
    let app_el = window::AppEl::for_pid(pid)?;
    let win = window::focused_window(&app_el)?;
    window_display(&win)
}

fn mouse_display() -> Option<DisplayId> {
    let src = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    let evt = CGEvent::new(src).ok()?;
    display_for_point(evt.location())
}
