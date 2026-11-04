//! The splash `hugin-launch` shows while Windows Terminal starts.
//!
//! A cold Windows Terminal takes about three and a half seconds to show its window, and until
//! then nothing is on screen: the shortcut key seems to have done nothing. So the launcher puts
//! up a small dark card (the shield, the name, the folder, a sweeping bar) the moment it starts,
//! and fades it out when the new terminal window appears.
//!
//! The card is a layered, topmost, no-activate tool window, so it never takes focus from the
//! terminal and has no taskbar button. The pure parts, the layout, the sweep, the fade and the
//! "is this window new" decision, are tested in `splash_tests.rs`; the rest is Win32.

use std::cell::RefCell;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use bifrost_core::colors::palette;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, HDC,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SRCCOPY, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DispatchMessageW,
    DrawIconEx, EnumWindows, GetClassNameW, GetCursorPos, GetMessageW, GetWindowThreadProcessId, HICON, IMAGE_ICON,
    IsWindowVisible, KillTimer, LR_LOADFROMFILE, LWA_ALPHA, LoadImageW, MA_NOACTIVATE, MSG, PostQuitMessage,
    RegisterClassW, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, WM_DESTROY,
    WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::launch::Started;

// ---- the pure parts ----

/// The card's width and height in logical pixels, at 96 dpi.
const CARD: (i32, i32) = (340, 104);
/// The padding inside the card, the shield's size and the gap after it, in logical pixels.
const PAD: i32 = 18;
const ICON: i32 = 48;
const GAP: i32 = 14;
/// The progress track's height, in logical pixels.
const TRACK_HEIGHT: i32 = 3;

/// How long the fade in and the fade out take, in milliseconds.
const FADE_IN_MS: u64 = 120;
const FADE_OUT_MS: u64 = 160;
/// One sweep of the bar, in milliseconds.
const SWEEP_MS: u64 = 1100;
/// A terminal window paints after it shows; this is how long the splash waits for that.
const PAINT_GRACE_MS: u64 = 150;
/// The longest the splash stays when the terminal never shows: with Windows Terminal, and with
/// the plain console.
const LIMIT_TERMINAL: Duration = Duration::from_secs(8);
const LIMIT_CONSOLE: Duration = Duration::from_secs(3);

const TERMINAL_CLASS: &str = "CASCADIA_HOSTING_WINDOW_CLASS";
const CONSOLE_CLASS: &str = "ConsoleWindowClass";

/// A rectangle as left, top, right, bottom, in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rc {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rc {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
}

/// Where everything on the card goes, for one display scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub card: Rc,
    pub icon: Rc,
    pub title: Rc,
    pub folder: Rc,
    pub track: Rc,
    /// The border's width, at least one device pixel.
    pub border: i32,
}

/// `logical` pixels at `dpi`, rounded to the nearest device pixel.
pub fn scaled(logical: i32, dpi: u32) -> i32 {
    (logical * dpi as i32 + 48) / 96
}

/// The card's layout at `dpi` (96 is 100 %).
pub fn layout(dpi: u32) -> Layout {
    let s = |v| scaled(v, dpi);
    let (width, height) = (s(CARD.0), s(CARD.1));
    let text_left = s(PAD + ICON + GAP);
    let track_top = height - s(PAD) - s(TRACK_HEIGHT);
    Layout {
        card: Rc {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        icon: Rc {
            left: s(PAD),
            top: s(PAD),
            right: s(PAD + ICON),
            bottom: s(PAD + ICON),
        },
        title: Rc {
            left: text_left,
            top: s(PAD),
            right: width - s(PAD),
            bottom: s(PAD + 26),
        },
        folder: Rc {
            left: text_left,
            top: s(PAD + 28),
            right: width - s(PAD),
            bottom: s(PAD + 48),
        },
        track: Rc {
            left: s(PAD),
            top: track_top,
            right: width - s(PAD),
            bottom: track_top + s(TRACK_HEIGHT),
        },
        border: s(1).max(1),
    }
}

/// The classic smoothstep: 0 at 0, 1 at 1, slow at both ends.
pub fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The bar's segment, as `(start, end)` offsets within a track `track_width` wide, `elapsed_ms`
/// after the splash began. The segment is a third of the track and crosses it from just off the
/// left edge to just off the right, eased, once per [`SWEEP_MS`], then starts again; the parts
/// outside the track are clipped.
pub fn sweep(elapsed_ms: u64, track_width: i32) -> (i32, i32) {
    let segment = (track_width / 3).max(1);
    let phase = (elapsed_ms % SWEEP_MS) as f64 / SWEEP_MS as f64;
    let start = -segment + (smoothstep(phase) * (track_width + segment) as f64).round() as i32;
    (start.clamp(0, track_width), (start + segment).clamp(0, track_width))
}

/// The window's opacity, 0 to 255: rising over [`FADE_IN_MS`] from `shown_ms` after it appeared,
/// and falling over [`FADE_OUT_MS`] from `out_ms` after the fade out began (if it has), from
/// whatever it had reached.
pub fn alpha(shown_ms: u64, out: Option<(u64, u8)>) -> u8 {
    match out {
        None => (shown_ms.min(FADE_IN_MS) * 255 / FADE_IN_MS) as u8,
        Some((out_ms, from)) => {
            let left = FADE_OUT_MS.saturating_sub(out_ms);
            (from as u64 * left / FADE_OUT_MS) as u8
        }
    }
}

/// The text under the name: the folder's last component, or the whole path when it is short or
/// has no last component (a drive root).
pub fn folder_label(folder: &str) -> String {
    let trimmed = folder.trim_end_matches(['\\', '/']);
    if folder.chars().count() <= 24 {
        return folder.to_string();
    }
    match Path::new(trimmed).file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => folder.to_string(),
    }
}

/// A top-level window as the splash sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub hwnd: isize,
    pub visible: bool,
    /// Whether its class is one a terminal window has.
    pub terminal_class: bool,
    pub pid: u32,
}

/// Whether the terminal has appeared: a visible window that was not there before and is of a
/// terminal class, or (in the console fallback, `pid` not 0) belongs to the process started.
/// `noted` holds the terminal windows that existed before the start.
pub fn terminal_appeared(noted: &[isize], now: &[Seen], pid: u32) -> bool {
    now.iter()
        .any(|w| w.visible && !noted.contains(&w.hwnd) && (w.terminal_class || (pid != 0 && w.pid == pid)))
}

// ---- Win32 ----

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

/// `#rrggbb` as a GDI `COLORREF` (0x00bbggrr). A malformed value is black, which the palette's
/// own tests rule out.
fn colorref(hex: &str) -> u32 {
    let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    ((n & 0xff) << 16) | (n & 0xff00) | (n >> 16)
}

/// Every top-level window, with its class judged against `classes`.
fn windows(classes: &[&str]) -> Vec<Seen> {
    struct Walk<'a> {
        classes: &'a [&'a str],
        found: Vec<Seen>,
    }
    unsafe extern "system" fn each(hwnd: HWND, param: LPARAM) -> i32 {
        // SAFETY: `param` is the `Walk` that `windows` keeps alive for the whole enumeration.
        let walk = unsafe { &mut *(param as *mut Walk) };
        let mut name = [0u16; 64];
        // SAFETY: the buffer is 64 wide characters, and its length is what is passed.
        let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) }.max(0) as usize;
        let class = String::from_utf16_lossy(&name[..len]);
        let mut pid = 0u32;
        // SAFETY: `pid` is a live u32.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        walk.found.push(Seen {
            hwnd: hwnd as isize,
            // SAFETY: a plain query on a window handle that may by now be gone, which it reports.
            visible: unsafe { IsWindowVisible(hwnd) } != 0,
            terminal_class: walk.classes.contains(&class.as_str()),
            pid,
        });
        1
    }
    let mut walk = Walk {
        classes,
        found: Vec::new(),
    };
    // SAFETY: `each` only uses `param` as the `Walk` it is, and `walk` outlives the call.
    unsafe { EnumWindows(Some(each), &mut walk as *mut Walk as LPARAM) };
    walk.found
}

/// Everything the window procedure needs, kept per thread because a window procedure has no
/// other way to be handed it.
struct State {
    layout: Layout,
    dpi: u32,
    title: Vec<u16>,
    folder: Vec<u16>,
    icon: Option<HICON>,
    colors: Colors,
    began: Instant,
    classes: &'static [&'static str],
    noted: Vec<isize>,
    /// The process to look for a window of, or 0.
    pid: u32,
    limit: Duration,
    /// When a new terminal window was first seen.
    seen_at: Option<Instant>,
    /// When the fade out began, and the opacity it began from.
    out: Option<(Instant, u8)>,
    last_alpha: Option<u8>,
}

struct Colors {
    back: u32,
    border: u32,
    track: u32,
    bar: u32,
    title: u32,
    folder: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Draw the whole card into `hdc`, which is the size of `state.layout.card`.
///
/// # Safety
/// `hdc` must be a valid device context.
unsafe fn draw(hdc: HDC, state: &State) {
    let l = &state.layout;
    let fill = |rc: &Rc, color: u32| {
        let rect = RECT {
            left: rc.left,
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        };
        // SAFETY: a brush made and deleted here, a rectangle on the stack.
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(hdc, &rect, brush);
            DeleteObject(brush);
        }
    };
    let inner = Rc {
        left: l.border,
        top: l.border,
        right: l.card.right - l.border,
        bottom: l.card.bottom - l.border,
    };
    fill(&l.card, state.colors.border);
    fill(&inner, state.colors.back);

    if let Some(icon) = state.icon {
        // SAFETY: `icon` is an icon handle owned by `State` until the window is destroyed.
        unsafe {
            DrawIconEx(
                hdc,
                l.icon.left,
                l.icon.top,
                icon,
                l.icon.width(),
                l.icon.width(),
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            )
        };
    }

    let text = |rc: &Rc, text: &[u16], face: &str, points: i32, weight: i32, color: u32| {
        let mut rect = RECT {
            left: rc.left,
            top: rc.top,
            right: rc.right,
            bottom: rc.bottom,
        };
        let face = wide(face);
        let height = -(points * state.dpi as i32 / 72);
        // SAFETY: the font is selected only while the text is drawn, then put back and deleted;
        // the text is a slice with the length passed, the face NUL-terminated.
        unsafe {
            let font = CreateFontW(height, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
            let old = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);
            SetTextColor(hdc, color);
            DrawTextW(
                hdc,
                text.as_ptr(),
                text.len() as i32,
                &mut rect,
                DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SelectObject(hdc, old);
            DeleteObject(font);
        }
    };
    text(&l.title, &state.title, "Segoe UI Semibold", 15, 600, state.colors.title);
    text(&l.folder, &state.folder, "Segoe UI", 10, 400, state.colors.folder);

    fill(&l.track, state.colors.track);
    let (from, to) = sweep(state.began.elapsed().as_millis() as u64, l.track.width());
    if to > from {
        fill(
            &Rc {
                left: l.track.left + from,
                top: l.track.top,
                right: l.track.left + to,
                bottom: l.track.bottom,
            },
            state.colors.bar,
        );
    }
}

fn paint(hwnd: HWND) {
    STATE.with(|cell| {
        let cell = cell.borrow();
        let Some(state) = cell.as_ref() else { return };
        let card = state.layout.card;
        // SAFETY: the usual double-buffered paint: a memory DC and bitmap made, drawn into,
        // copied to the window's DC in one go (so nothing flickers) and deleted again.
        unsafe {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let target = BeginPaint(hwnd, &mut ps);
            let memory = CreateCompatibleDC(target);
            let bitmap = CreateCompatibleBitmap(target, card.right, card.bottom);
            let old = SelectObject(memory, bitmap);
            draw(memory, state);
            BitBlt(target, 0, 0, card.right, card.bottom, memory, 0, 0, SRCCOPY);
            SelectObject(memory, old);
            DeleteObject(bitmap);
            DeleteDC(memory);
            EndPaint(hwnd, &ps);
        }
    });
}

/// One tick of the animation: set the opacity, look for the terminal, decide whether to go.
fn tick(hwnd: HWND) {
    let done = STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(state) = cell.as_mut() else { return false };
        let now = Instant::now();
        let since = now.duration_since(state.began);

        if state.out.is_none() {
            if state.seen_at.is_none() && terminal_appeared(&state.noted, &windows(state.classes), state.pid) {
                state.seen_at = Some(now);
            }
            let settled = state
                .seen_at
                .is_some_and(|t| now.duration_since(t) >= Duration::from_millis(PAINT_GRACE_MS));
            if settled || since >= state.limit {
                let reached = alpha(since.as_millis() as u64, None);
                state.out = Some((now, reached));
            }
        }

        let level = match state.out {
            None => alpha(since.as_millis() as u64, None),
            Some((t, from)) => alpha(0, Some((now.duration_since(t).as_millis() as u64, from))),
        };
        if state.last_alpha != Some(level) {
            state.last_alpha = Some(level);
            // SAFETY: a layered window of ours.
            unsafe { SetLayeredWindowAttributes(hwnd, 0, level, LWA_ALPHA) };
        }
        state
            .out
            .is_some_and(|(t, _)| now.duration_since(t).as_millis() as u64 >= FADE_OUT_MS)
    });
    if done {
        // SAFETY: our own window; WM_DESTROY ends the message loop.
        unsafe { DestroyWindow(hwnd) };
    } else {
        // SAFETY: repaint the whole client area, without erasing it first.
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        // The whole card is painted from a buffer; erasing first is what would flicker.
        WM_ERASEBKGND => 1,
        // A click on the card does nothing, and above all does not take focus from the terminal.
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_TIMER => {
            tick(hwnd);
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe {
                KillTimer(hwnd, 1);
                PostQuitMessage(0);
            }
            0
        }
        // SAFETY: the default handling of a message this window does not care about.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The monitor under the mouse: its work area and its scale in dpi.
fn monitor_under_mouse() -> (RECT, u32) {
    // SAFETY: plain queries; the structs are the sizes they declare.
    unsafe {
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        let monitor = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) != 0 || dpi_x == 0 {
            dpi_x = 96;
        }
        (info.rcWork, dpi_x)
    }
}

/// Create the card, centred on the monitor the mouse is on and not yet visible. `None` if
/// Windows would not make it; the launch goes on without a splash.
fn create(
    folder: &str,
    icon_path: &Path,
    classes: &'static [&'static str],
    noted: Vec<isize>,
    pid: u32,
) -> Option<HWND> {
    let (work, dpi) = monitor_under_mouse();
    let layout = layout(dpi);
    let (width, height) = (layout.card.right, layout.card.bottom);
    let x = work.left + (work.right - work.left - width) / 2;
    let y = work.top + (work.bottom - work.top - height) / 2;

    let p = palette();
    let icon = {
        let path = wide(&icon_path.to_string_lossy());
        // SAFETY: a NUL-terminated path; the icon is loaded at the size it is drawn.
        let handle = unsafe {
            LoadImageW(
                std::ptr::null_mut(),
                path.as_ptr(),
                IMAGE_ICON,
                layout.icon.width(),
                layout.icon.width(),
                LR_LOADFROMFILE,
            )
        };
        (!handle.is_null()).then_some(handle)
    };
    let state = State {
        layout,
        dpi,
        title: "Hugin".encode_utf16().collect(),
        folder: folder_label(folder).encode_utf16().collect(),
        icon,
        colors: Colors {
            back: colorref(&p.ink),
            border: colorref("#2a2a2a"),
            track: colorref("#262626"),
            bar: colorref(&p.hugin.dark),
            title: colorref(&p.ivory),
            folder: colorref("#8a8a8a"),
        },
        began: Instant::now(),
        classes,
        noted,
        pid,
        limit: if pid == 0 { LIMIT_TERMINAL } else { LIMIT_CONSOLE },
        seen_at: None,
        out: None,
        last_alpha: None,
    };
    STATE.with(|cell| *cell.borrow_mut() = Some(state));

    let class = wide("HuginLaunchSplash");
    // SAFETY: a window class and a window of our own; every string is NUL-terminated and lives
    // past the call that reads it.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class_def = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&class_def);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("Hugin").as_ptr(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        // Rounded corners where Windows 11 offers them; older Windows just keeps them square.
        let round: i32 = 2; // DWMWCP_ROUND
        DwmSetWindowAttribute(hwnd, 33, &round as *const i32 as *const _, size_of::<i32>() as u32); // DWMWA_WINDOW_CORNER_PREFERENCE
        SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, 1, 16, None);
        Some(hwnd)
    }
}

/// Show the splash, run `start` (which starts Windows Terminal or the console and must not wait
/// on it), and keep the splash up until the new window appears or the time limit passes. Returns
/// once the splash is gone, with what `start` returned.
///
/// If `start` fails the splash is closed first, so that it is not up behind the error message.
pub fn run(folder: &str, icon_path: &Path, start: impl FnOnce() -> Result<Started, String>) -> Result<Started, String> {
    // SAFETY: setting this process's DPI awareness, before any window exists; failing (already
    // set) is harmless.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    // The terminal windows already open are noted before starting, so that only a new one counts.
    let classes: &'static [&'static str] = &[TERMINAL_CLASS, CONSOLE_CLASS];
    let noted: Vec<isize> = windows(classes)
        .into_iter()
        .filter(|w| w.terminal_class)
        .map(|w| w.hwnd)
        .collect();

    // The card is made before anything is started and watches for Windows Terminal; if `start`
    // turns out to have used the plain console, it is told below to watch for that instead.
    let hwnd = create(folder, icon_path, &[TERMINAL_CLASS], noted.clone(), 0);
    // So that the new terminal window may come to the front, though this process was the one
    // that was last given the foreground.
    // SAFETY: a plain call.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };

    let started = start();
    match (&started, hwnd) {
        (Err(_), Some(hwnd)) => {
            // SAFETY: our own window; WM_DESTROY ends the loop below at once.
            unsafe { DestroyWindow(hwnd) };
        }
        (Ok(Started::Console(pid)), Some(_)) => STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                // The plain console: the console window and any window of the new process count,
                // and the card does not wait as long.
                state.classes = classes;
                state.pid = *pid;
                state.limit = LIMIT_CONSOLE;
            }
        }),
        _ => {}
    }

    if hwnd.is_some() {
        // SAFETY: the usual message loop of this thread, which owns the window.
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    started
}
