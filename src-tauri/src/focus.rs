//! Foreground-window capture and restore.
//!
//! The user reports that transcribed text frequently lands in the wrong
//! window — typically because the foreground window changes between the
//! moment the user presses the transcribe hotkey and the moment Handy
//! synthesises keystrokes through enigo.
//!
//! Strategy: capture the foreground window the instant the hotkey goes
//! down (before any of Handy's UI activates), store it in Tauri-managed
//! state, then re-activate it immediately before keystroke synthesis.
//!
//! ## Windows
//!
//! `GetForegroundWindow` to capture, `SetForegroundWindow` to restore.
//! The bare call can be silently denied by the foreground-lock policy;
//! the standard workaround is to attach our thread's input queue to the
//! target window's thread for the duration of the call.
//!
//! ## Linux (X11)
//!
//! `GetInputFocus` to capture, then an EWMH `_NET_ACTIVE_WINDOW`
//! `ClientMessage` to the root window plus `SetInputFocus` to restore.
//! The ClientMessage is the canonical activation path on Xfce/X11
//! Mutter/KWin — bare `SetInputFocus` won't raise the window. Bug fix
//! for issue #315 (Fedora/Xfce: second-and-subsequent paste lands in
//! Handy's overlay rather than the user's intended target window).
//!
//! ## Linux (Wayland) / macOS
//!
//! No-op. Wayland has no protocol that exposes the previously-focused
//! client to a non-compositor app. macOS uses an NSPanel overlay with
//! `no_activate(true)` so focus is never stolen from the user's app.

use std::sync::Mutex;
use tauri::AppHandle;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use tauri::Manager;

/// HWND of the window that had foreground when the transcribe hotkey was
/// pressed. Stored as `isize` to keep the type `Send + Sync` without
/// platform-conditional struct definitions. `None` means nothing has been
/// captured yet this session.
#[derive(Default)]
pub struct TargetWindow(pub Mutex<Option<isize>>);

/// Capture the current foreground window. Call this on hotkey-down,
/// before any Handy UI activates and steals focus.
#[cfg(target_os = "windows")]
pub fn capture_foreground(app: &AppHandle) {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        log::debug!("focus: GetForegroundWindow returned null; nothing captured");
        return;
    }

    match app.try_state::<TargetWindow>() {
        Some(state) => {
            if let Ok(mut guard) = state.0.lock() {
                *guard = Some(hwnd.0 as isize);
                log::debug!("focus: captured target HWND {:?}", hwnd.0);
            }
        }
        None => {
            log::warn!("focus: TargetWindow state not managed; cannot capture");
        }
    }
}

#[cfg(target_os = "linux")]
pub fn capture_foreground(app: &AppHandle) {
    if !linux_x11::is_x11_session() {
        log::debug!("focus: not an X11 session; capture is a no-op");
        return;
    }

    let Some(window) = linux_x11::capture() else {
        log::debug!("focus: X11 capture returned no usable window");
        return;
    };

    let Some(state) = app.try_state::<TargetWindow>() else {
        log::warn!("focus: TargetWindow state not managed; cannot capture");
        return;
    };
    if let Ok(mut guard) = state.0.lock() {
        *guard = Some(window as isize);
        log::debug!("focus: captured target X11 window {:#x}", window);
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn capture_foreground(_app: &AppHandle) {
    // No-op on macOS: NSPanel overlay uses no_activate(true), focus stays
    // with the user's app. On Wayland we cannot observe foreground.
}

/// Restore the previously captured foreground window. Call this just
/// before keystroke synthesis so injection lands in the right place.
///
/// Safe to call when nothing has been captured — it just no-ops.
#[cfg(target_os = "windows")]
pub fn restore_foreground(app: &AppHandle) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
    };

    let raw = match app.try_state::<TargetWindow>() {
        Some(state) => match state.0.lock() {
            Ok(guard) => *guard,
            Err(_) => {
                log::warn!("focus: TargetWindow mutex poisoned; skipping restore");
                return;
            }
        },
        None => {
            log::warn!("focus: TargetWindow state not managed; skipping restore");
            return;
        }
    };

    let Some(raw) = raw else {
        log::debug!("focus: no target HWND captured; skipping restore");
        return;
    };

    let hwnd = HWND(raw as *mut core::ffi::c_void);

    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            log::debug!("focus: captured HWND is no longer a valid window");
            return;
        }

        let target_tid = GetWindowThreadProcessId(hwnd, None);
        let our_tid = GetCurrentThreadId();
        let should_attach = target_tid != 0 && target_tid != our_tid;

        // windows 0.61 wrapper takes `bool` (not `BOOL`) and converts internally.
        let attached = if should_attach {
            AttachThreadInput(our_tid, target_tid, true).as_bool()
        } else {
            false
        };

        let ok = SetForegroundWindow(hwnd).as_bool();

        if attached {
            let _ = AttachThreadInput(our_tid, target_tid, false);
        }

        if ok {
            log::debug!("focus: restored foreground to HWND {:?}", hwnd.0);
        } else {
            log::warn!(
                "focus: SetForegroundWindow failed for HWND {:?} (attached={attached})",
                hwnd.0
            );
        }
    }
}

#[cfg(target_os = "linux")]
pub fn restore_foreground(app: &AppHandle) {
    if !linux_x11::is_x11_session() {
        return;
    }

    let raw = match app.try_state::<TargetWindow>() {
        Some(state) => match state.0.lock() {
            Ok(guard) => *guard,
            Err(_) => {
                log::warn!("focus: TargetWindow mutex poisoned; skipping restore");
                return;
            }
        },
        None => {
            log::warn!("focus: TargetWindow state not managed; skipping restore");
            return;
        }
    };

    let Some(raw) = raw else {
        log::debug!("focus: no target X11 window captured; skipping restore");
        return;
    };

    let window = raw as u32;
    if linux_x11::restore(window) {
        log::debug!("focus: restored X11 focus to window {:#x}", window);
    } else {
        log::warn!(
            "focus: X11 focus restore failed for window {:#x}; paste may target wrong window",
            window
        );
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn restore_foreground(_app: &AppHandle) {
    // No-op on macOS; see capture_foreground note.
}

#[cfg(target_os = "linux")]
mod linux_x11 {
    use std::env;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        ClientMessageEvent, ConnectionExt as _, EventMask, InputFocus,
    };
    use x11rb::rust_connection::RustConnection;
    use x11rb::CURRENT_TIME;

    /// True when the current session is X11 (or XWayland with no native
    /// Wayland display). On pure Wayland sessions we never attempt focus
    /// tracking — the X server may not even be running.
    pub fn is_x11_session() -> bool {
        if env::var("WAYLAND_DISPLAY")
            .ok()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            return false;
        }
        env::var("DISPLAY")
            .ok()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Capture the currently-focused X11 window via `GetInputFocus`.
    /// Returns `None` if the connection fails, the focus is the root, or
    /// the server reports `None`/`PointerRoot`.
    pub fn capture() -> Option<u32> {
        let (conn, _screen) = RustConnection::connect(None).ok()?;
        let reply = conn.get_input_focus().ok()?.reply().ok()?;
        let win = reply.focus;
        // 0 == None, 1 == PointerRoot (per X protocol). Neither is a
        // usable target window.
        if win <= 1 {
            return None;
        }
        Some(win)
    }

    /// Restore focus to `window` using EWMH `_NET_ACTIVE_WINDOW` plus a
    /// best-effort `SetInputFocus`. Returns true if the ClientMessage was
    /// dispatched.
    pub fn restore(window: u32) -> bool {
        let Ok((conn, screen_num)) = RustConnection::connect(None) else {
            return false;
        };
        let root = match conn.setup().roots.get(screen_num) {
            Some(s) => s.root,
            None => return false,
        };

        let net_active_atom = match conn.intern_atom(false, b"_NET_ACTIVE_WINDOW") {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply.atom,
                Err(_) => return false,
            },
            Err(_) => return false,
        };

        // EWMH spec: data.l[0] = source (1 = normal app), data.l[1] =
        // timestamp, data.l[2] = currently-active window (0 if unknown),
        // data.l[3..] = zero.
        let event = ClientMessageEvent::new(
            32,
            window,
            net_active_atom,
            [1u32, CURRENT_TIME, 0, 0, 0],
        );

        let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT;
        if conn.send_event(false, root, mask, event).is_err() {
            return false;
        }

        // Belt-and-braces: also issue SetInputFocus. EWMH-aware WMs will
        // honour the ClientMessage; un-aware ones (rare) at least move
        // keyboard focus.
        let _ = conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME);
        let _ = conn.flush();
        true
    }
}
