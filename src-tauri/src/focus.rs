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
#[cfg(target_os = "windows")]
use tauri::Emitter;
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
        Some(state) => match state.0.lock() {
            Ok(mut guard) => {
                *guard = Some(hwnd.0 as isize);
                log::debug!("focus: captured target HWND {:?}", hwnd.0);
            }
            Err(_) => {
                log::warn!("focus: TargetWindow mutex poisoned; cannot capture");
            }
        },
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

    match app.try_state::<TargetWindow>() {
        Some(state) => match state.0.lock() {
            Ok(mut guard) => {
                *guard = Some(window as isize);
                log::debug!("focus: captured target X11 window {:#x}", window);
            }
            Err(_) => {
                log::warn!("focus: TargetWindow mutex poisoned; cannot capture");
            }
        },
        None => {
            log::warn!("focus: TargetWindow state not managed; cannot capture");
        }
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

        // One attempt = attach our input queue to the target thread (lets
        // SetForegroundWindow bypass the foreground-lock policy), call it,
        // detach. Retry once after a short yield: the WM occasionally
        // rejects the first call right after our overlay hides, then
        // accepts it a few ms later.
        let mut ok = false;
        for attempt in 0..2 {
            // windows 0.61 wrapper takes `bool` (not `BOOL`) and converts.
            let attached = if should_attach {
                AttachThreadInput(our_tid, target_tid, true).as_bool()
            } else {
                false
            };

            ok = SetForegroundWindow(hwnd).as_bool();

            if attached {
                let _ = AttachThreadInput(our_tid, target_tid, false);
            }

            if ok {
                log::debug!(
                    "focus: restored foreground to HWND {:?} (attempt {})",
                    hwnd.0,
                    attempt + 1
                );
                break;
            }

            if attempt == 0 {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }

        if !ok {
            log::warn!(
                "focus: SetForegroundWindow denied for HWND {:?} after retry; \
                 paste may land in the wrong window",
                hwnd.0
            );
            // Surface it: a silent misdirect is the worst failure mode.
            // App.tsx toasts on this (same pattern as `paste-error`).
            let _ = app.emit("focus-restore-failed", ());
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
    use once_cell::sync::Lazy;
    use std::env;
    use std::sync::Mutex;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask, InputFocus};
    use x11rb::rust_connection::RustConnection;
    use x11rb::CURRENT_TIME;

    /// Session type is fixed for the process lifetime, so probe the env
    /// once instead of on every keystroke + paste.
    static IS_X11: Lazy<bool> = Lazy::new(|| {
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
    });

    /// True when the current session is X11 (or XWayland with no native
    /// Wayland display). On pure Wayland sessions we never attempt focus
    /// tracking — the X server may not even be running.
    pub fn is_x11_session() -> bool {
        *IS_X11
    }

    struct XConn {
        conn: RustConnection,
        screen: usize,
    }

    /// One process-lifetime X11 connection instead of a fresh
    /// connect()/teardown on every capture + restore. Cleared on any
    /// connection-level error so the next call transparently reconnects
    /// (handles X server restart / display change).
    static CONN: Lazy<Mutex<Option<XConn>>> = Lazy::new(|| Mutex::new(None));

    /// Run `f` against the cached connection, establishing it on first
    /// use. The closure returns `Err(())` for connection-level failures
    /// (which invalidate the cache for a lazy reconnect) vs `Ok` for
    /// normal results — a transient "no focused window" must NOT drop the
    /// connection.
    fn with_conn<T>(f: impl FnOnce(&RustConnection, usize) -> Result<T, ()>) -> Option<T> {
        let mut guard = CONN.lock().ok()?;
        if guard.is_none() {
            match RustConnection::connect(None) {
                Ok((conn, screen)) => *guard = Some(XConn { conn, screen }),
                Err(_) => return None,
            }
        }
        let xc = guard.as_ref()?;
        match f(&xc.conn, xc.screen) {
            Ok(v) => Some(v),
            Err(()) => {
                // Connection is likely dead — drop it so the next call
                // reconnects from scratch.
                *guard = None;
                None
            }
        }
    }

    /// Capture the currently-focused X11 window via `GetInputFocus`.
    /// Returns `None` if the connection fails, the focus is the root, or
    /// the server reports `None`/`PointerRoot`.
    pub fn capture() -> Option<u32> {
        with_conn(|conn, _screen| {
            let reply = conn
                .get_input_focus()
                .map_err(|_| ())?
                .reply()
                .map_err(|_| ())?;
            let win = reply.focus;
            // 0 == None, 1 == PointerRoot (per X protocol). Neither is a
            // usable target window — but this is a normal transient state,
            // not a connection failure, so report Ok(None) (keep the conn).
            Ok(if win <= 1 { None } else { Some(win) })
        })
        .flatten()
    }

    /// Restore focus to `window` using EWMH `_NET_ACTIVE_WINDOW` plus a
    /// best-effort `SetInputFocus`. Returns true if the ClientMessage was
    /// dispatched.
    pub fn restore(window: u32) -> bool {
        with_conn(|conn, screen| {
            let root = conn.setup().roots.get(screen).ok_or(())?.root;

            let net_active_atom = conn
                .intern_atom(false, b"_NET_ACTIVE_WINDOW")
                .map_err(|_| ())?
                .reply()
                .map_err(|_| ())?
                .atom;

            // EWMH spec: data.l[0] = source (1 = normal app), data.l[1] =
            // timestamp, data.l[2] = currently-active window (0 if
            // unknown), data.l[3..] = zero.
            let event =
                ClientMessageEvent::new(32, window, net_active_atom, [1u32, CURRENT_TIME, 0, 0, 0]);

            let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT;
            conn.send_event(false, root, mask, event).map_err(|_| ())?;

            // Belt-and-braces: also issue SetInputFocus. EWMH-aware WMs
            // honour the ClientMessage; un-aware ones (rare) at least move
            // keyboard focus.
            let _ = conn.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME);
            let _ = conn.flush();
            Ok(())
        })
        .is_some()
    }
}
