// Copyright 2026 Mark West
// SPDX-License-Identifier: Apache-2.0

// Prevents an additional console window from appearing on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicI32, AtomicIsize};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, State};

/// Startup command line, read by the frontend via `get_launch_args`.
#[derive(Clone, Default, Serialize)]
struct LaunchArgs {
    path: Option<String>,
    preview: bool,
}

// ----------------------------------------------------------------------
// Command line

const USAGE: &str = "\
Usage: hawk [OPTIONS] [FILE]

Arguments:
  FILE            Text file to open. Without it, Hawk starts with an empty,
                  untitled document.

Options:
  --preview       Open FILE in preview mode, if its type supports preview
                  (Markdown: .md; HyperText: .html, .htm). Otherwise the
                  status bar says preview isn't available.
  --log           Append diagnostic messages to hawk.log, next to hawk.exe.
  --version       Print the version and exit.
  -h, --help, /?  Print this help and exit.
  --              Treat everything after this as FILE, even if it starts
                  with \"-\".";

enum CliAction {
    Run { args: LaunchArgs, log: bool },
    Help,
    Version,
    Error(String),
}

fn parse_args(raw: impl IntoIterator<Item = String>) -> CliAction {
    let mut args = LaunchArgs::default();
    let mut log = false;
    let mut options_done = false;
    for arg in raw {
        if !options_done {
            match arg.as_str() {
                "-h" | "--help" | "/?" | "-?" => return CliAction::Help,
                "--version" => return CliAction::Version,
                "--log" => {
                    log = true;
                    continue;
                }
                "--preview" => {
                    args.preview = true;
                    continue;
                }
                "--" => {
                    options_done = true;
                    continue;
                }
                s if s.starts_with('-') => {
                    return CliAction::Error(format!("unknown option '{s}'"));
                }
                _ => {}
            }
        }
        if args.path.is_some() {
            return CliAction::Error(format!("unexpected extra argument '{arg}' (only one FILE can be opened)"));
        }
        args.path = Some(arg);
    }
    CliAction::Run { args, log }
}

fn help_text() -> String {
    format!(
        "Hawk {} - a simple notepad-style text editor with Markdown and HTML preview.\n\n{USAGE}\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// Prints help/version/usage errors. Release builds are GUI-subsystem apps
/// with no stdout, so: write to a redirected handle if there is one, else to
/// the parent console (the prompt has usually returned already, hence the
/// leading newline), else show a message box.
#[cfg(target_os = "windows")]
fn print_cli_output(text: &str, is_error: bool) {
    use std::io::Write;
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };

    let std_handle = if is_error { STD_ERROR_HANDLE } else { STD_OUTPUT_HANDLE };
    if unsafe { GetStdHandle(std_handle) }.is_ok() {
        let _ = if is_error {
            std::io::stderr().write_all(text.as_bytes())
        } else {
            std::io::stdout().write_all(text.as_bytes())
        };
        return;
    }
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
        if let Ok(mut con) = fs::OpenOptions::new().write(true).open("CONOUT$") {
            let _ = con.write_all(format!("\r\n{}", text.replace('\n', "\r\n")).as_bytes());
            return;
        }
    }
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let icon = if is_error { MB_ICONERROR } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(None, PCWSTR(wide.as_ptr()), w!("Hawk"), MB_OK | icon);
    }
}

#[cfg(not(target_os = "windows"))]
fn print_cli_output(text: &str, is_error: bool) {
    if is_error {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
}

/// User settings, saved as JSON in the per-user app config directory.
#[derive(Clone, Serialize, Deserialize)]
struct AppSettings {
    word_wrap: bool,
    spellcheck: bool,
    /// "None" | "Tab" | "Spaces"
    tab_mode: String,
    /// "Default" | "Light" | "Dark"
    theme: String,
    /// 1.0 = 100%
    #[serde(default = "default_zoom")]
    zoom: f64,
    /// Editor font family. No UI: set by editing settings.json, so the copy
    /// on disk always wins (see persist_settings).
    #[serde(default = "default_font")]
    font: String,
}

fn default_font() -> String {
    "Consolas".to_string()
}

fn default_zoom() -> f64 {
    1.0
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            word_wrap: true,
            spellcheck: true,
            tab_mode: "None".to_string(),
            theme: "Default".to_string(),
            zoom: default_zoom(),
            font: default_font(),
        }
    }
}

/// Zoom steps, the same ladder Edge/Chrome use.
const ZOOM_LEVELS: [f64; 17] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0,
];

fn nearest_zoom_index(zoom: f64) -> usize {
    let mut best = 0;
    let mut best_diff = f64::MAX;
    for (i, level) in ZOOM_LEVELS.iter().enumerate() {
        let diff = (level - zoom).abs();
        if diff < best_diff {
            best_diff = diff;
            best = i;
        }
    }
    best
}

fn apply_zoom(app: &AppHandle, zoom: f64) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_zoom(zoom);
        update_resize_overlay_for_zoom(&window, zoom);
    }
}

// ----------------------------------------------------------------------
// Application log: hawk.log next to hawk.exe, only with --log. Lines are
// "[UTC timestamp] [PID] message"; the PID tells instances apart.
static LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn app_log(line: &str) {
    if !LOGGING_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "[{}] [{}] {line}", format_utc_now(), std::process::id());
    }
}

/// "YYYY-MM-DD HH:MM:SS.mmm UTC", via Howard Hinnant's civil-from-days
/// algorithm (avoids a date/time dependency).
fn format_utc_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let days = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{millis:03} UTC")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ----------------------------------------------------------------------
// Cursor-vanish workaround (Windows only)
//
// WebView2 hides the cursor while typing and only restores it on mouse
// movement inside its own client area, so after typing, the cursor stays
// invisible over the title bar and other non-client areas. Introduced in
// WebView2 runtime version 152. Fix: subclass the top-level window and call
// ShowCursor(true) on WM_NCMOUSEMOVE. Remove once WebView2 is fixed.
// Similar upstream issue:
// https://github.com/MicrosoftEdge/WebView2Feedback/issues/5687
#[cfg(target_os = "windows")]
const WM_NCMOUSEMOVE: u32 = 0x00A0;

#[cfg(target_os = "windows")]
const CURSOR_VANISH_SUBCLASS_ID: usize = 0x4841_574B; // arbitrary

#[cfg(target_os = "windows")]
unsafe extern "system" fn cursor_vanish_subclass_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    _ref_data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::ShowCursor;

    if msg == WM_NCMOUSEMOVE {
        let _ = ShowCursor(true);
    }

    DefSubclassProc(hwnd, msg, wparam, lparam)
}

#[cfg(target_os = "windows")]
fn install_cursor_vanish_workaround(window: &tauri::WebviewWindow) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::SetWindowSubclass;

    let Ok(handle) = window.window_handle() else {
        app_log("cursor-vanish workaround: window.window_handle() failed; workaround not installed");
        return;
    };
    let RawWindowHandle::Win32(win32_handle) = handle.as_raw() else {
        app_log("cursor-vanish workaround: raw handle was not RawWindowHandle::Win32; workaround not installed");
        return;
    };
    let hwnd = HWND(win32_handle.hwnd.get() as *mut core::ffi::c_void);
    unsafe {
        let ok = SetWindowSubclass(
            hwnd,
            Some(cursor_vanish_subclass_proc),
            CURSOR_VANISH_SUBCLASS_ID,
            0,
        );
        app_log(&format!(
            "cursor-vanish workaround: SetWindowSubclass on hwnd={:?} returned {:?}",
            hwnd.0, ok
        ));
    }
}

#[cfg(not(target_os = "windows"))]
fn install_cursor_vanish_workaround(_window: &tauri::WebviewWindow) {}

// ----------------------------------------------------------------------
// Resize overlay (Windows only)
//
// WebView2 covers the whole client area, so clicks in the bottom-right
// corner never reach Windows' resize handling. A small WS_CHILD STATIC
// window on top of WebView2 in that corner answers WM_NCHITTEST with
// HTBOTTOMRIGHT (STATIC's default, HTTRANSPARENT, lets clicks through; 0
// hangs the message loop), and Windows does the resize.
//
// Sized to the status bar height: 24 CSS px x zoom x DPI scale. Keep
// STATUS_BAR_CSS_HEIGHT_PX in sync with #statusbar in ui/styles.css.
#[cfg(target_os = "windows")]
const WM_NCHITTEST: u32 = 0x0084;
#[cfg(target_os = "windows")]
const HTBOTTOMRIGHT: isize = 17;
#[cfg(target_os = "windows")]
const RESIZE_OVERLAY_SUBCLASS_ID: usize = 0x4841_574D; // arbitrary
#[cfg(target_os = "windows")]
const STATUS_BAR_CSS_HEIGHT_PX: f64 = 24.0;

#[cfg(target_os = "windows")]
static OVERLAY_HWND: AtomicIsize = AtomicIsize::new(0);
#[cfg(target_os = "windows")]
static OVERLAY_SIZE_PX: AtomicI32 = AtomicI32::new(24);

#[cfg(target_os = "windows")]
unsafe extern "system" fn resize_overlay_subclass_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    _ref_data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::Shell::DefSubclassProc;

    if msg == WM_NCHITTEST {
        return LRESULT(HTBOTTOMRIGHT);
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

#[cfg(target_os = "windows")]
fn compute_overlay_size_px(zoom: f64, scale_factor: f64) -> i32 {
    (STATUS_BAR_CSS_HEIGHT_PX * zoom * scale_factor)
        .round()
        .max(1.0) as i32
}

#[cfg(target_os = "windows")]
fn parent_hwnd_of(window: &tauri::WebviewWindow) -> Option<windows::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(win32_handle) = handle.as_raw() else {
        return None;
    };
    Some(HWND(win32_handle.hwnd.get() as *mut core::ffi::c_void))
}

#[cfg(target_os = "windows")]
fn reposition_and_resize_overlay(parent: windows::Win32::Foundation::HWND, size: i32) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, SetWindowPos, HWND_TOP, SWP_NOACTIVATE,
    };
    let raw = OVERLAY_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(parent, &mut rect);
    }
    let x = (rect.right - size).max(0);
    let y = (rect.bottom - size).max(0);
    let overlay = HWND(raw as *mut core::ffi::c_void);
    unsafe {
        let _ = SetWindowPos(overlay, HWND_TOP, x, y, size, size, SWP_NOACTIVATE);
    }
}

/// Records the overlay size for a zoom factor, and applies it if the
/// overlay already exists.
#[cfg(target_os = "windows")]
fn update_resize_overlay_for_zoom(window: &tauri::WebviewWindow, zoom: f64) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let size = compute_overlay_size_px(zoom, scale);
    OVERLAY_SIZE_PX.store(size, Ordering::Relaxed);
    if let Some(parent) = parent_hwnd_of(window) {
        reposition_and_resize_overlay(parent, size);
    }
}

/// Repositions the overlay (same size) after the main window is resized.
#[cfg(target_os = "windows")]
fn reposition_resize_overlay(window: &tauri::WebviewWindow) {
    let size = OVERLAY_SIZE_PX.load(Ordering::Relaxed);
    if let Some(parent) = parent_hwnd_of(window) {
        reposition_and_resize_overlay(parent, size);
    }
}

/// Called from RunEvent::Ready: before the event loop runs, CreateWindowExW
/// can fail with ERROR_CANNOT_FIND_WND_CLASS.
#[cfg(target_os = "windows")]
fn install_resize_overlay(window: &tauri::WebviewWindow) {
    use windows::core::w;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::SetWindowSubclass;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, GetClientRect, SetWindowPos, HWND_TOP, SWP_NOACTIVATE, WINDOW_EX_STYLE,
        WS_CHILD, WS_VISIBLE,
    };

    let Some(parent) = parent_hwnd_of(window) else {
        app_log("resize overlay: could not get native window handle; not installed");
        return;
    };

    let size = OVERLAY_SIZE_PX.load(Ordering::Relaxed);
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(parent, &mut rect);
    }
    let x = (rect.right - size).max(0);
    let y = (rect.bottom - size).max(0);

    let hinstance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            app_log(&format!("resize overlay: GetModuleHandleW failed: {e}"));
            return;
        }
    };

    let overlay_hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WS_CHILD | WS_VISIBLE,
            x,
            y,
            size,
            size,
            parent,
            None,
            hinstance,
            None,
        )
    };
    let overlay_hwnd = match overlay_hwnd {
        Ok(h) if !h.0.is_null() => h,
        Ok(_) => {
            app_log("resize overlay: CreateWindowExW returned a null HWND");
            return;
        }
        Err(e) => {
            app_log(&format!("resize overlay: CreateWindowExW failed: {e}"));
            return;
        }
    };

    unsafe {
        let _ = SetWindowPos(overlay_hwnd, HWND_TOP, x, y, size, size, SWP_NOACTIVATE);
        let _ = SetWindowSubclass(
            overlay_hwnd,
            Some(resize_overlay_subclass_proc),
            RESIZE_OVERLAY_SUBCLASS_ID,
            0,
        );
    }
    OVERLAY_HWND.store(overlay_hwnd.0 as isize, Ordering::Relaxed);
    app_log(&format!("resize overlay: created at ({x},{y}) size {size}x{size}"));
}

#[cfg(not(target_os = "windows"))]
fn install_resize_overlay(_window: &tauri::WebviewWindow) {}
#[cfg(not(target_os = "windows"))]
fn reposition_resize_overlay(_window: &tauri::WebviewWindow) {}
#[cfg(not(target_os = "windows"))]
fn update_resize_overlay_for_zoom(_window: &tauri::WebviewWindow, _zoom: f64) {}

// ----------------------------------------------------------------------
// New-window placement
//
// The window starts hidden and centered (tauri.conf.json). If another Hawk
// window's top-left corner is already there, it moves right and down by
// CASCADE_STEP until it finds a free spot, wrapping to the top-left of the
// monitor's work area when it would go off screen. Hawk windows are found
// by their window class (windowClassname in tauri.conf.json). When several
// instances are started at once (dropped files), the launching instance
// staggers them (SPAWN_STAGGER) so each sees the windows placed before it.

/// Delay between instances started together, so they cascade in turn.
const SPAWN_STAGGER: std::time::Duration = std::time::Duration::from_millis(200);

/// Logical pixels; scaled by the monitor's DPI.
const CASCADE_STEP: f64 = 25.0;

fn place_and_show_window(window: &tauri::WebviewWindow) {
    cascade_window(window);
    // Always shown, even if placement failed.
    let _ = window.show();
    let _ = window.set_focus();
}

fn cascade_window(window: &tauri::WebviewWindow) {
    let others = other_window_positions(window);
    if others.is_empty() {
        return;
    }
    let (Ok(start), Ok(size), Ok(Some(monitor))) =
        (window.outer_position(), window.outer_size(), window.current_monitor())
    else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let step = (CASCADE_STEP * scale).round().max(1.0) as i32;
    let work = monitor.work_area();
    let area = (
        work.position.x,
        work.position.y,
        work.position.x + work.size.width as i32,
        work.position.y + work.size.height as i32,
    );
    let size = (size.width as i32, size.height as i32);
    let (x, y) = cascade_position((start.x, start.y), size, area, step, &others);
    if (x, y) != (start.x, start.y) {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        app_log(&format!("Window placed at ({x},{y}), {} other window(s) open", others.len()));
    }
}

/// First spot from `start`, stepping by `step`, whose top-left isn't within
/// half a step of another window's. `area` is (left, top, right, bottom).
fn cascade_position(
    start: (i32, i32),
    size: (i32, i32),
    area: (i32, i32, i32, i32),
    step: i32,
    others: &[(i32, i32)],
) -> (i32, i32) {
    let occupied = |x: i32, y: i32| {
        others
            .iter()
            .any(|&(ox, oy)| (ox - x).abs() < step / 2 && (oy - y).abs() < step / 2)
    };
    let (left, top, right, bottom) = area;
    let (mut x, mut y) = start;
    for _ in 0..100 {
        if !occupied(x, y) {
            break;
        }
        x += step;
        y += step;
        if x + size.0 > right || y + size.1 > bottom {
            x = left;
            y = top;
        }
    }
    (x, y)
}

/// Top-left corners of the other visible, non-minimized Hawk windows.
#[cfg(target_os = "windows")]
fn other_window_positions(window: &tauri::WebviewWindow) -> Vec<(i32, i32)> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowRect, IsIconic, IsWindowVisible,
    };
    let own = parent_hwnd_of(window);
    let mut positions = Vec::new();
    let mut prev = HWND::default();
    // Bounded in case windows come and go during the walk.
    for _ in 0..256 {
        let Ok(hwnd) = (unsafe { FindWindowExW(None, prev, w!("HawkWindow"), PCWSTR::null()) })
        else {
            break;
        };
        if hwnd.0.is_null() {
            break;
        }
        prev = hwnd;
        if Some(hwnd) == own || unsafe { !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() } {
            continue;
        }
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
            positions.push((rect.left, rect.top));
        }
    }
    positions
}

#[cfg(not(target_os = "windows"))]
fn other_window_positions(_window: &tauri::WebviewWindow) -> Vec<(i32, i32)> {
    Vec::new()
}

struct SettingsState(Mutex<AppSettings>);

fn settings_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

fn log_settings(prefix: &str, settings: &AppSettings) {
    app_log(&format!(
        "{prefix}: word_wrap={} spellcheck={} tab_mode={} theme={} zoom={:.2} font={}",
        settings.word_wrap, settings.spellcheck, settings.tab_mode, settings.theme, settings.zoom, settings.font
    ));
}

fn load_settings_from_disk(app: &AppHandle) -> AppSettings {
    let Ok(path) = settings_file_path(app) else {
        return AppSettings::default();
    };
    let (settings, rewrite) = parse_settings(fs::read_to_string(&path).ok().as_deref());
    log_settings("Settings loaded", &settings);
    if rewrite {
        persist_settings(app, &settings);
    }
    settings
}

/// Settings from the file's text (None = no file), and whether to write the
/// file back out: when it's missing or lacks a setting, so every setting
/// (including "font", which has no UI) is visible in it. A file that can't
/// be read as settings is left alone so hand edits aren't lost.
fn parse_settings(text: Option<&str>) -> (AppSettings, bool) {
    let Some(text) = text else {
        return (AppSettings::default(), true);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return (AppSettings::default(), false);
    };
    let Ok(settings) = serde_json::from_value::<AppSettings>(value.clone()) else {
        return (AppSettings::default(), false);
    };
    let complete = match (serde_json::to_value(&settings), value.as_object()) {
        (Ok(serde_json::Value::Object(all)), Some(on_disk)) => {
            all.keys().all(|key| on_disk.contains_key(key))
        }
        _ => true,
    };
    (settings, !complete)
}

fn font_on_disk(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    value.get("font")?.as_str().map(str::to_string)
}

fn persist_settings(app: &AppHandle, settings: &AppSettings) {
    if let Ok(path) = settings_file_path(app) {
        // Keep a font edited in the file since this instance started.
        let mut settings = settings.clone();
        if let Some(font) = font_on_disk(&path) {
            settings.font = font;
        }
        if let Ok(json) = serde_json::to_string_pretty(&settings) {
            if let Err(e) = fs::write(&path, json) {
                app_log(&format!("Settings write FAILED: {e}"));
            }
        }
    }
}

#[tauri::command]
fn get_launch_args(state: State<LaunchArgs>) -> LaunchArgs {
    state.inner().clone()
}

#[tauri::command]
fn get_settings(state: State<SettingsState>) -> AppSettings {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn read_text_file(path: String) -> Result<String, String> {
    match fs::read_to_string(&path) {
        Ok(contents) => {
            app_log(&format!("Open: {path}"));
            Ok(contents)
        }
        Err(e) => {
            app_log(&format!("Open FAILED: {path}: {e}"));
            Err(format!("{e}"))
        }
    }
}

/// `kind` ("save" or "save_as") only changes the log label.
#[tauri::command]
fn write_text_file(path: String, contents: String, kind: String) -> Result<(), String> {
    let label = if kind == "save_as" { "Save As" } else { "Save" };
    match fs::write(&path, contents.as_bytes()) {
        Ok(()) => {
            app_log(&format!("{label}: {path}"));
            Ok(())
        }
        Err(e) => {
            app_log(&format!("{label} FAILED: {path}: {e}"));
            Err(format!("{e}"))
        }
    }
}

/// Lets the frontend write to the application log.
#[tauri::command]
fn log_event(message: String) {
    app_log(&message);
}

/// File > New: starts another, empty instance.
#[tauri::command]
fn spawn_new_instance() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| format!("{e}"))?;
    let mut cmd = Command::new(&exe);
    if LOGGING_ENABLED.load(Ordering::Relaxed) {
        cmd.arg("--log");
    }
    match cmd.spawn() {
        Ok(_) => {
            app_log("New: spawned a new instance");
            Ok(())
        }
        Err(e) => {
            app_log(&format!("New FAILED: {e}"));
            Err(format!("{e}"))
        }
    }
}

/// Starts a new instance that opens `path` (drag-and-drop, preview links).
/// `label` prefixes the log line.
fn spawn_new_instance_with_path(path: &Path, preview: bool, label: &str) -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| format!("{e}"))?;
    let mut cmd = Command::new(&exe);
    if LOGGING_ENABLED.load(Ordering::Relaxed) {
        cmd.arg("--log");
    }
    if preview {
        cmd.arg("--preview");
    }
    // "--" so a file name starting with "-" isn't parsed as an option.
    cmd.arg("--").arg(path);
    match cmd.spawn() {
        Ok(_) => {
            app_log(&format!("{label}: opened {} in a new instance", path.display()));
            Ok(())
        }
        Err(e) => {
            app_log(&format!("{label} FAILED: {}: {e}", path.display()));
            Err(format!("{e}"))
        }
    }
}

// ----------------------------------------------------------------------
// Links clicked in the preview
//
// The preview is an iframe whose relative URLs resolve against Hawk's own
// page (http://tauri.localhost/), and Tauri answers unknown paths with
// index.html, so letting the iframe follow a link shows a broken copy of
// Hawk's UI. The frontend cancels every navigation and sends links here.

/// Turns a link's href into a file path: drops "?query" and "#fragment",
/// percent-decodes, accepts file: URLs, and resolves relative paths
/// against the document's folder.
fn resolve_link_path(doc_path: Option<&str>, href: &str) -> Result<PathBuf, String> {
    let end = href.find(|c| c == '?' || c == '#').unwrap_or(href.len());
    let mut target = percent_decode(&href[..end]);
    if target.len() >= 5 && target[..5].eq_ignore_ascii_case("file:") {
        let mut rest = target[5..].to_string();
        if let Some(r) = rest.strip_prefix("//") {
            rest = if r.len() >= 9 && r[..9].eq_ignore_ascii_case("localhost") {
                r[9..].to_string()
            } else if r.starts_with('/') {
                r.to_string()
            } else {
                format!("//{r}") // file://server/share/x.md -> UNC path
            };
        }
        // file:///C:/dir/x.md -> C:/dir/x.md
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            rest.remove(0);
        }
        target = rest;
    }
    if target.is_empty() {
        return Err("Link has no file path.".to_string());
    }
    let target = Path::new(&target);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        let Some(dir) = doc_path.and_then(|p| Path::new(p).parent()) else {
            return Err("Save the document to follow relative links.".to_string());
        };
        dir.join(target) // a rooted "/x" keeps the document's drive on Windows
    };
    #[cfg(target_os = "windows")]
    let resolved = PathBuf::from(resolved.to_string_lossy().replace('/', "\\"));
    Ok(resolved)
}

fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Opens a linked local file in a new instance (in preview if asked).
#[tauri::command]
fn open_linked_file(doc_path: Option<String>, href: String, preview: bool) -> Result<(), String> {
    let path = resolve_link_path(doc_path.as_deref(), &href)?;
    if !path.is_file() {
        app_log(&format!("Link FAILED: {href}: not found ({})", path.display()));
        return Err(format!("Link target not found: {}", path.display()));
    }
    spawn_new_instance_with_path(&path, preview, "Link")
        .map_err(|e| format!("Could not open {}: {e}", path.display()))
}

/// Opens an http(s) or mailto link with the system's default handler.
#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return Err(format!("Unsupported link: {url}"));
    }
    shell_open(&url)?;
    app_log(&format!("Link: opened {url}"));
    Ok(())
}

#[cfg(target_os = "windows")]
fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a value > 32 on success.
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        app_log(&format!("Link FAILED: {target}: ShellExecuteW returned {code}"));
        Err(format!("Windows could not open the link (error {code})."))
    }
}

#[cfg(not(target_os = "windows"))]
fn shell_open(_target: &str) -> Result<(), String> {
    Err("Opening links is only supported on Windows".to_string())
}

#[tauri::command]
fn exit_app(app: AppHandle) {
    app_log("Exit: requested");
    app.exit(0);
}

/// Copies `path` to the clipboard with clip.exe. clip.exe reads stdin in the
/// console code page, so characters outside it may be mangled.
#[cfg(target_os = "windows")]
#[tauri::command]
fn copy_path_to_clipboard(path: String) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("clip")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{e}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(path.as_bytes()).map_err(|e| format!("{e}"))?;
    }
    child.wait().map_err(|e| format!("{e}"))?;
    app_log(&format!("File Path to Clipboard: {path}"));
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn copy_path_to_clipboard(_path: String) -> Result<(), String> {
    Err("Copying the file path to the clipboard is only supported on Windows".to_string())
}

/// Opens Explorer with the file selected (`explorer /select, <path>`).
/// Explorer's exit code is meaningless, so only the launch is checked.
#[cfg(target_os = "windows")]
#[tauri::command]
fn open_file_directory(path: String) -> Result<(), String> {
    match Command::new("explorer").args(["/select,", &path]).spawn() {
        Ok(_) => {
            app_log(&format!("Open File Directory: {path}"));
            Ok(())
        }
        Err(e) => {
            app_log(&format!("Open File Directory FAILED: {path}: {e}"));
            Err(format!("{e}"))
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn open_file_directory(_path: String) -> Result<(), String> {
    Err("Opening the file's directory is only supported on Windows".to_string())
}

/// The Operations submenus, disabled while the preview is showing.
struct TextOpMenus {
    format_submenu: tauri::menu::Submenu<tauri::Wry>,
    line_endings_submenu: tauri::menu::Submenu<tauri::Wry>,
    line_operations_submenu: tauri::menu::Submenu<tauri::Wry>,
    blank_ops_submenu: tauri::menu::Submenu<tauri::Wry>,
}

#[tauri::command]
fn set_text_ops_enabled(app: AppHandle, enabled: bool) {
    if let Some(menus) = app.try_state::<TextOpMenus>() {
        let _ = menus.format_submenu.set_enabled(enabled);
        let _ = menus.line_endings_submenu.set_enabled(enabled);
        let _ = menus.line_operations_submenu.set_enabled(enabled);
        let _ = menus.blank_ops_submenu.set_enabled(enabled);
    }
}

/// Installed WebView2 Runtime version, for the About box.
#[tauri::command]
fn get_webview2_version() -> Option<String> {
    tauri::webview_version().ok()
}

/// Hawk's version from Cargo.toml, for the About box.
#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn zoom_in(app: AppHandle, state: State<SettingsState>) -> f64 {
    let mut s = state.0.lock().unwrap();
    let idx = nearest_zoom_index(s.zoom);
    let new_idx = (idx + 1).min(ZOOM_LEVELS.len() - 1);
    s.zoom = ZOOM_LEVELS[new_idx];
    apply_zoom(&app, s.zoom);
    persist_settings(&app, &s);
    app_log(&format!("Zoom changed: {:.2}", s.zoom));
    s.zoom
}

#[tauri::command]
fn zoom_out(app: AppHandle, state: State<SettingsState>) -> f64 {
    let mut s = state.0.lock().unwrap();
    let idx = nearest_zoom_index(s.zoom);
    let new_idx = idx.saturating_sub(1);
    s.zoom = ZOOM_LEVELS[new_idx];
    apply_zoom(&app, s.zoom);
    persist_settings(&app, &s);
    app_log(&format!("Zoom changed: {:.2}", s.zoom));
    s.zoom
}

#[tauri::command]
fn zoom_reset(app: AppHandle, state: State<SettingsState>) -> f64 {
    let mut s = state.0.lock().unwrap();
    s.zoom = default_zoom();
    apply_zoom(&app, s.zoom);
    persist_settings(&app, &s);
    app_log(&format!("Zoom changed: {:.2}", s.zoom));
    s.zoom
}

fn main() {
    let (launch_args, log_requested) = match parse_args(env::args().skip(1)) {
        CliAction::Run { args, log } => (args, log),
        CliAction::Help => {
            print_cli_output(&help_text(), false);
            std::process::exit(0);
        }
        CliAction::Version => {
            print_cli_output(&format!("Hawk {}\n", env!("CARGO_PKG_VERSION")), false);
            std::process::exit(0);
        }
        CliAction::Error(msg) => {
            print_cli_output(&format!("hawk: {msg}\nTry 'hawk --help' for usage.\n"), true);
            std::process::exit(2);
        }
    };

    if log_requested {
        LOGGING_ENABLED.store(true, Ordering::Relaxed);
        if let Ok(exe) = env::current_exe() {
            if let Some(dir) = exe.parent() {
                let log_path = dir.join("hawk.log");
                // Keep the log from growing unbounded: start fresh if it's
                // already over 1 MB by the time the app launches again.
                if let Ok(meta) = fs::metadata(&log_path) {
                    if meta.len() > 1_000_000 {
                        let _ = fs::remove_file(&log_path);
                    }
                }
                let _ = LOG_PATH.set(log_path);
            }
        }

        app_log(&format!("=== Hawk v{} starting ===", env!("CARGO_PKG_VERSION")));
        match tauri::webview_version() {
            Ok(v) => app_log(&format!("WebView2 Runtime version: {v}")),
            Err(e) => app_log(&format!("WebView2 Runtime version: unknown ({e})")),
        }
        #[cfg(target_os = "windows")]
        app_log("Workarounds enabled: cursor-vanish workaround (msedgewebview2 hide-cursor-while-typing bug)");
        #[cfg(not(target_os = "windows"))]
        app_log("Workarounds enabled: none (the cursor-vanish workaround is Windows-only)");
        app_log(&format!(
            "Launch args: file={} preview={}",
            launch_args.path.as_deref().unwrap_or("(none)"),
            launch_args.preview
        ));
    }

    let app = tauri::Builder::default()
        .manage(launch_args)
        .manage(SettingsState(Mutex::new(AppSettings::default())))
        .invoke_handler(tauri::generate_handler![
            get_launch_args,
            get_settings,
            read_text_file,
            write_text_file,
            log_event,
            spawn_new_instance,
            exit_app,
            zoom_in,
            zoom_out,
            zoom_reset,
            copy_path_to_clipboard,
            open_file_directory,
            set_text_ops_enabled,
            get_webview2_version,
            get_app_version,
            open_linked_file,
            open_external_url
        ])
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // Settings first: the menu's check marks come from them.
            let settings = load_settings_from_disk(&handle);
            if let Some(state) = app.try_state::<SettingsState>() {
                *state.0.lock().unwrap() = settings.clone();
            }

            apply_zoom(&handle, settings.zoom);

            if let Some(window) = app.get_webview_window("main") {
                install_cursor_vanish_workaround(&window);
                place_and_show_window(&window);
            }

            // ---- File menu ----
            let new_item = MenuItemBuilder::with_id("file_new", "&New")
                .accelerator("CmdOrCtrl+N")
                .build(&handle)?;
            let open_item = MenuItemBuilder::with_id("file_open", "&Open...")
                .accelerator("CmdOrCtrl+O")
                .build(&handle)?;
            let save_item = MenuItemBuilder::with_id("file_save", "&Save")
                .accelerator("CmdOrCtrl+S")
                .build(&handle)?;
            let save_as_item =
                MenuItemBuilder::with_id("file_save_as", "Save &As...").build(&handle)?;
            let file_copy_path_item =
                MenuItemBuilder::with_id("file_copy_path", "File Path to &Clipboard")
                    .build(&handle)?;
            let file_open_dir_item =
                MenuItemBuilder::with_id("file_open_dir", "Open File &Directory")
                    .build(&handle)?;
            let exit_item = MenuItemBuilder::with_id("file_exit", "E&xit").build(&handle)?;

            let file_menu = SubmenuBuilder::new(&handle, "&File")
                .item(&new_item)
                .item(&open_item)
                .separator()
                .item(&save_item)
                .item(&save_as_item)
                .separator()
                .item(&file_copy_path_item)
                .item(&file_open_dir_item)
                .separator()
                .item(&exit_item)
                .build()?;

            // ---- Settings menu ----
            let word_wrap_item = CheckMenuItemBuilder::with_id("settings_word_wrap", "&Word Wrap")
                .checked(settings.word_wrap)
                .build(&handle)?;

            let spellcheck_item =
                CheckMenuItemBuilder::with_id("settings_spellcheck", "S&pellcheck")
                    .checked(settings.spellcheck)
                    .build(&handle)?;

            // Tauri has no radio items; the menu handler keeps these
            // (and Display's) mutually exclusive.
            let tabs_none = CheckMenuItemBuilder::with_id("tabs_none", "&None")
                .checked(settings.tab_mode == "None")
                .build(&handle)?;
            let tabs_tab = CheckMenuItemBuilder::with_id("tabs_tab", "&Tab")
                .checked(settings.tab_mode == "Tab")
                .build(&handle)?;
            let tabs_spaces = CheckMenuItemBuilder::with_id("tabs_spaces", "&Spaces")
                .checked(settings.tab_mode == "Spaces")
                .build(&handle)?;
            let tabs_submenu = SubmenuBuilder::new(&handle, "&Tabs")
                .item(&tabs_none)
                .item(&tabs_tab)
                .item(&tabs_spaces)
                .build()?;

            let display_default = CheckMenuItemBuilder::with_id("display_default", "&Default")
                .checked(settings.theme == "Default")
                .build(&handle)?;
            let display_light = CheckMenuItemBuilder::with_id("display_light", "&Light")
                .checked(settings.theme == "Light")
                .build(&handle)?;
            let display_dark = CheckMenuItemBuilder::with_id("display_dark", "D&ark")
                .checked(settings.theme == "Dark")
                .build(&handle)?;
            let display_submenu = SubmenuBuilder::new(&handle, "&Display")
                .item(&display_default)
                .item(&display_light)
                .item(&display_dark)
                .build()?;

            let settings_menu = SubmenuBuilder::new(&handle, "&Settings")
                .item(&word_wrap_item)
                .item(&spellcheck_item)
                .separator()
                .item(&tabs_submenu)
                .item(&display_submenu)
                .build()?;

            // ---- Operations menu ----
            let format_html_item =
                MenuItemBuilder::with_id("ops_format_html", "&HTML").build(&handle)?;
            let format_json_item =
                MenuItemBuilder::with_id("ops_format_json", "&JSON").build(&handle)?;
            let format_submenu = SubmenuBuilder::new(&handle, "&Format")
                .item(&format_html_item)
                .item(&format_json_item)
                .build()?;

            let line_endings_windows_item =
                MenuItemBuilder::with_id("ops_line_endings_windows", "&Windows (CR LF)")
                    .build(&handle)?;
            let line_endings_unix_item =
                MenuItemBuilder::with_id("ops_line_endings_unix", "&Unix (LF)").build(&handle)?;
            let line_endings_submenu = SubmenuBuilder::new(&handle, "Line &Endings")
                .item(&line_endings_windows_item)
                .item(&line_endings_unix_item)
                .build()?;

            let blank_tab_to_spaces_item =
                MenuItemBuilder::with_id("ops_tab_to_spaces", "&Tab to Spaces").build(&handle)?;
            let blank_trim_trailing_item =
                MenuItemBuilder::with_id("ops_trim_trailing", "Trim Trailing &Space")
                    .build(&handle)?;
            let blank_trim_leading_item =
                MenuItemBuilder::with_id("ops_trim_leading", "Trim &Leading Space")
                    .build(&handle)?;
            let blank_remove_dup_space_item = MenuItemBuilder::with_id(
                "ops_remove_dup_space",
                "Remove All &Duplicate Space",
            )
            .build(&handle)?;
            let blank_ops_submenu = SubmenuBuilder::new(&handle, "&Blank Operations")
                .item(&blank_tab_to_spaces_item)
                .item(&blank_trim_trailing_item)
                .item(&blank_trim_leading_item)
                .item(&blank_remove_dup_space_item)
                .build()?;

            let sort_asc_item =
                MenuItemBuilder::with_id("ops_sort_asc", "Sort &Ascending").build(&handle)?;
            let sort_desc_item =
                MenuItemBuilder::with_id("ops_sort_desc", "Sort &Descending").build(&handle)?;
            let remove_dup_lines_item =
                MenuItemBuilder::with_id("ops_remove_dup_lines", "Remove D&uplicate Lines")
                    .build(&handle)?;
            let remove_blank_lines_item =
                MenuItemBuilder::with_id("ops_remove_blank_lines", "Remove &Blank Lines")
                    .build(&handle)?;
            let line_operations_submenu = SubmenuBuilder::new(&handle, "&Line Operations")
                .item(&sort_asc_item)
                .item(&sort_desc_item)
                .item(&remove_dup_lines_item)
                .item(&remove_blank_lines_item)
                .build()?;

            let operations_menu = SubmenuBuilder::new(&handle, "&Operations")
                .item(&format_submenu)
                .item(&line_endings_submenu)
                .item(&line_operations_submenu)
                .item(&blank_ops_submenu)
                .build()?;

            app.manage(TextOpMenus {
                format_submenu: format_submenu.clone(),
                line_endings_submenu: line_endings_submenu.clone(),
                line_operations_submenu: line_operations_submenu.clone(),
                blank_ops_submenu: blank_ops_submenu.clone(),
            });

            // ---- Help menu ----
            let about_item = MenuItemBuilder::with_id("help_about", "&About...").build(&handle)?;
            let help_menu = SubmenuBuilder::new(&handle, "&Help")
                .item(&about_item)
                .build()?;

            let menu = MenuBuilder::new(&handle)
                .item(&file_menu)
                .item(&operations_menu)
                .item(&settings_menu)
                .item(&help_menu)
                .build()?;

            app.set_menu(menu)?;

            let word_wrap_item_h = word_wrap_item.clone();
            let spellcheck_item_h = spellcheck_item.clone();
            let tabs_none_h = tabs_none.clone();
            let tabs_tab_h = tabs_tab.clone();
            let tabs_spaces_h = tabs_spaces.clone();
            let display_default_h = display_default.clone();
            let display_light_h = display_light.clone();
            let display_dark_h = display_dark.clone();

            app.on_menu_event(move |app_handle, event| {
                let id = event.id().0.as_str();
                match id {
                    "file_new" => {
                        let _ = app_handle.emit("menu:new", ());
                    }
                    "file_open" => {
                        let _ = app_handle.emit("menu:open", ());
                    }
                    "file_save" => {
                        let _ = app_handle.emit("menu:save", ());
                    }
                    "file_save_as" => {
                        let _ = app_handle.emit("menu:save-as", ());
                    }
                    "file_exit" => {
                        let _ = app_handle.emit("menu:exit", ());
                    }
                    "file_copy_path" => {
                        let _ = app_handle.emit("menu:copy-path", ());
                    }
                    "file_open_dir" => {
                        let _ = app_handle.emit("menu:open-dir", ());
                    }
                    "ops_format_html" => {
                        let _ = app_handle.emit("menu:format-html", ());
                    }
                    "ops_format_json" => {
                        let _ = app_handle.emit("menu:format-json", ());
                    }
                    "ops_line_endings_windows" => {
                        let _ = app_handle.emit("menu:line-endings-windows", ());
                    }
                    "ops_line_endings_unix" => {
                        let _ = app_handle.emit("menu:line-endings-unix", ());
                    }
                    "ops_tab_to_spaces" => {
                        let _ = app_handle.emit("menu:tab-to-spaces", ());
                    }
                    "ops_trim_trailing" => {
                        let _ = app_handle.emit("menu:trim-trailing", ());
                    }
                    "ops_trim_leading" => {
                        let _ = app_handle.emit("menu:trim-leading", ());
                    }
                    "ops_remove_dup_space" => {
                        let _ = app_handle.emit("menu:remove-dup-spaces", ());
                    }
                    "ops_remove_blank_lines" => {
                        let _ = app_handle.emit("menu:remove-blank-lines", ());
                    }
                    "ops_sort_asc" => {
                        let _ = app_handle.emit("menu:sort-asc", ());
                    }
                    "ops_sort_desc" => {
                        let _ = app_handle.emit("menu:sort-desc", ());
                    }
                    "ops_remove_dup_lines" => {
                        let _ = app_handle.emit("menu:remove-dup-lines", ());
                    }
                    "help_about" => {
                        let _ = app_handle.emit("menu:about", ());
                    }
                    "settings_word_wrap" => {
                        let checked = word_wrap_item_h.is_checked().unwrap_or(true);
                        app_log(&format!("Toggled Word Wrap: {checked}"));
                        if let Some(state) = app_handle.try_state::<SettingsState>() {
                            let mut s = state.0.lock().unwrap();
                            s.word_wrap = checked;
                            persist_settings(app_handle, &s);
                        }
                        let _ = app_handle.emit("menu:word-wrap", checked);
                    }
                    "settings_spellcheck" => {
                        let checked = spellcheck_item_h.is_checked().unwrap_or(true);
                        app_log(&format!("Toggled Spellcheck: {checked}"));
                        if let Some(state) = app_handle.try_state::<SettingsState>() {
                            let mut s = state.0.lock().unwrap();
                            s.spellcheck = checked;
                            persist_settings(app_handle, &s);
                        }
                        let _ = app_handle.emit("menu:spellcheck", checked);
                    }
                    "tabs_none" | "tabs_tab" | "tabs_spaces" => {
                        let mode = match id {
                            "tabs_none" => "None",
                            "tabs_tab" => "Tab",
                            _ => "Spaces",
                        };
                        app_log(&format!("Toggled Tab Mode: {mode}"));
                        let _ = tabs_none_h.set_checked(mode == "None");
                        let _ = tabs_tab_h.set_checked(mode == "Tab");
                        let _ = tabs_spaces_h.set_checked(mode == "Spaces");
                        if let Some(state) = app_handle.try_state::<SettingsState>() {
                            let mut s = state.0.lock().unwrap();
                            s.tab_mode = mode.to_string();
                            persist_settings(app_handle, &s);
                        }
                        let _ = app_handle.emit("menu:tab-mode", mode);
                    }
                    "display_default" | "display_light" | "display_dark" => {
                        let theme = match id {
                            "display_default" => "Default",
                            "display_light" => "Light",
                            _ => "Dark",
                        };
                        app_log(&format!("Toggled Display Theme: {theme}"));
                        let _ = display_default_h.set_checked(theme == "Default");
                        let _ = display_light_h.set_checked(theme == "Light");
                        let _ = display_dark_h.set_checked(theme == "Dark");
                        if let Some(state) = app_handle.try_state::<SettingsState>() {
                            let mut s = state.0.lock().unwrap();
                            s.theme = theme.to_string();
                            persist_settings(app_handle, &s);
                        }
                        let _ = app_handle.emit("menu:theme", theme);
                    }
                    _ => {}
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Hawk");

    app.run(|app_handle, event| match event {
        tauri::RunEvent::Ready => {
            if let Some(window) = app_handle.get_webview_window("main") {
                install_resize_overlay(&window);
                let window_for_resize = window.clone();
                window.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::Resized(_)) {
                        reposition_resize_overlay(&window_for_resize);
                    }
                    // Each dropped file opens in its own new instance,
                    // started SPAWN_STAGGER apart (off the UI thread).
                    if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop {
                        paths,
                        ..
                    }) = event
                    {
                        let paths = paths.clone();
                        std::thread::spawn(move || {
                            for (i, path) in paths.iter().enumerate() {
                                if i > 0 {
                                    std::thread::sleep(SPAWN_STAGGER);
                                }
                                let _ = spawn_new_instance_with_path(path, false, "Drop");
                            }
                        });
                    }
                });
            }
        }
        // Not reached on a crash or forced kill.
        tauri::RunEvent::Exit => {
            app_log("=== Hawk closing ===");
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_paths() {
        let doc = Some("/notes/readme.md");
        let r = |h: &str| resolve_link_path(doc, h).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(r("docs/guide.md#intro").unwrap(), "/notes/docs/guide.md");
        assert_eq!(r("my%20file.md?x=1").unwrap(), "/notes/my file.md");
        assert_eq!(r("caf%C3%A9.md").unwrap(), "/notes/café.md");
        assert_eq!(r("100%.md").unwrap(), "/notes/100%.md");
        assert_eq!(r("%e9").unwrap(), "/notes/\u{FFFD}");
        assert_eq!(r("../other.md").unwrap(), "/notes/../other.md");
        assert_eq!(r("/abs/x.md").unwrap(), "/abs/x.md");
        assert_eq!(r("file:///abs/x.md").unwrap(), "/abs/x.md");
        assert_eq!(r("FILE://localhost/abs/x.md").unwrap(), "/abs/x.md");
        assert_eq!(r("file:///C:/dir/x.md").unwrap(), "/notes/C:/dir/x.md"); // drive paths are only absolute on Windows
        assert_eq!(r("file://server/share/x.md").unwrap(), "//server/share/x.md");
        assert!(r("#only").is_err());
        assert!(resolve_link_path(None, "x.md").is_err());
    }

    #[test]
    fn font_setting() {
        // Settings files from before the font option still load.
        let old = r#"{"word_wrap":true,"spellcheck":true,"tab_mode":"None","theme":"Dark","zoom":1.25}"#;
        let s: AppSettings = serde_json::from_str(old).unwrap();
        assert_eq!(s.font, "Consolas");
        assert_eq!(s.theme, "Dark");
        // A hand-edited font on disk is what gets kept.
        let path = env::temp_dir().join(format!("hawk-font-test-{}.json", std::process::id()));
        fs::write(&path, r#"{"font": "Cascadia Code", "zoom": 1.0}"#).unwrap();
        assert_eq!(font_on_disk(&path).as_deref(), Some("Cascadia Code"));
        fs::write(&path, "not json").unwrap();
        assert_eq!(font_on_disk(&path), None);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cascade() {
        let area = (0, 0, 1920, 1040);
        let size = (900, 650);
        let c = |others: &[(i32, i32)]| cascade_position((510, 195), size, area, 50, others);
        assert_eq!(c(&[]), (510, 195));
        assert_eq!(c(&[(510, 195)]), (560, 245));
        assert_eq!(c(&[(510, 195), (560, 245), (610, 295)]), (660, 345));
        assert_eq!(c(&[(510, 195), (610, 295)]), (560, 245)); // fills a gap
        assert_eq!(c(&[(520, 190)]), (560, 245)); // near-miss counts as occupied
        // 660+650 > 1040 for the next step: wraps to the work area's corner.
        assert_eq!(c(&[(510, 195), (560, 245), (610, 295), (660, 345)]), (0, 0));
        assert_eq!(c(&[(510, 195), (560, 245), (610, 295), (660, 345), (0, 0)]), (50, 50));
        // Window bigger than the screen: ends up in the corner, no hang.
        assert_eq!(cascade_position((0, 0), (3000, 3000), area, 50, &[(0, 0)]), (0, 0));
    }

    #[test]
    fn settings_file_rewrite() {
        // No file: defaults, and write one out.
        let (s, write) = parse_settings(None);
        assert!(write);
        assert_eq!(s.font, "Consolas");
        // Older file without "font": keep its values, add the missing key.
        let (s, write) = parse_settings(Some(r#"{"word_wrap":false,"spellcheck":true,"tab_mode":"Tab","theme":"Dark","zoom":1.5}"#));
        assert!(write);
        assert!(!s.word_wrap);
        assert_eq!(s.zoom, 1.5);
        // Complete file: nothing to write.
        let full = serde_json::to_string(&AppSettings::default()).unwrap();
        assert!(!parse_settings(Some(&full)).1);
        // Broken or mistyped files are never overwritten.
        assert!(!parse_settings(Some("{ not json")).1);
        assert!(!parse_settings(Some(r#"{"zoom":"big"}"#)).1);
    }

    #[test]
    fn cli_args() {
        let p = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
        assert!(matches!(p(&["--preview", "a.md"]), CliAction::Run { args: LaunchArgs { preview: true, .. }, log: false }));
        assert!(matches!(p(&["--", "-x.md"]), CliAction::Run { args: LaunchArgs { path: Some(_), .. }, .. }));
        assert!(matches!(p(&["a", "b"]), CliAction::Error(_)));
        assert!(matches!(p(&["/?"]), CliAction::Help));
    }
}
