//! Permanent Alert Window
//!
//! A window that lives on the second monitor and visually changes
//! when suspicious processes are detected - without stealing focus.
//! Features: Dragging, position saving, log display, transparency, right-click for log
//! Screenshot preview on alerts, minimize/pin buttons, details window

use std::sync::atomic::{AtomicBool, AtomicUsize, AtomicI32, Ordering};
use std::thread;
use std::time::Duration;
use std::path::PathBuf;
use std::fs;
use std::collections::{VecDeque, HashMap};
use parking_lot::Mutex;
use tracing::{info, error};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM, LRESULT, RECT, COLORREF, POINT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, InvalidateRect,
    BeginPaint, EndPaint, FillRect, SetBkMode, SetTextColor,
    TextOutW, DrawTextW, PAINTSTRUCT, HGDIOBJ, TRANSPARENT,
    CreateCompatibleDC, CreateDIBSection, SelectObject, StretchBlt,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY, DeleteDC,
    CreateRoundRectRgn, SetWindowRgn, RoundRect, CreatePen, PS_SOLID,
    SelectClipRgn,
    DT_CENTER, DT_VCENTER, DT_SINGLELINE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetCapture, ReleaseCapture};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::ExtractIconExW;

// Colors (BGR Format!)
const COLOR_NORMAL: u32 = 0x00228B22;     // Green (Forest Green) - all OK
const COLOR_ALERT: u32 = 0x000000FF;       // Red - Warning!
const COLOR_TEXT: u32 = 0x00FFFFFF;        // White
const COLOR_LOG_BG: u32 = 0x00202020;      // Dark gray for log area
const COLOR_BUTTON_BG: u32 = 0x00333333;   // Button background
const COLOR_BUTTON_ACTIVE: u32 = 0x00004400; // Active button (dark green)
const COLOR_DETAILS_BG: u32 = 0x00181818;  // Details window background

// Colors for event types (BGR Format!)
const COLOR_FOCUS: u32 = 0x0000FFFF;       // Yellow
const COLOR_CREATED: u32 = 0x00FFFF00;     // Cyan
const COLOR_SHOWN: u32 = 0x0000FF00;       // Green
const COLOR_MINIMIZED: u32 = 0x00808080;   // Gray
const COLOR_RESTORED: u32 = 0x00FF00FF;    // Magenta
const COLOR_ZORDER: u32 = 0x000000FF;      // Red

// Layout constants
const WINDOW_WIDTH: i32 = 720;
const WINDOW_HEIGHT: i32 = 340;
const HEADER_HEIGHT: i32 = 35;
const SCREENSHOT_WIDTH: i32 = 200;
const SCREENSHOT_HEIGHT: i32 = 130;
const LOG_AREA_WIDTH: i32 = WINDOW_WIDTH - SCREENSHOT_WIDTH - 20;
const MAX_LOG_ENTRIES: usize = 13;
const CORNER_RADIUS: i32 = 12;

// Button constants
const BTN_HEIGHT: i32 = 20;

// Details window constants
const DETAILS_WIDTH: i32 = 550;
const DETAILS_HEIGHT: i32 = 400;

// Global states
static ALERT_ACTIVE: AtomicBool = AtomicBool::new(false);
static WINDOW_HWND: AtomicUsize = AtomicUsize::new(0);
static DETAILS_HWND: AtomicUsize = AtomicUsize::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static DRAG_START_X: AtomicI32 = AtomicI32::new(0);
static DRAG_START_Y: AtomicI32 = AtomicI32::new(0);
static EVENT_COUNT: AtomicUsize = AtomicUsize::new(0);
static WINDOW_PINNED: AtomicBool = AtomicBool::new(true);
static WINDOW_MINIMIZED: AtomicBool = AtomicBool::new(false);
static SCREENSHOT_HIDDEN: AtomicBool = AtomicBool::new(false);

/// Screenshot data for display
#[derive(Clone)]
pub struct ScreenshotData {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// GUI log entry with event type for color coding and details
#[derive(Clone)]
pub struct GuiLogEntry {
    pub text: String,
    pub event_type: String,
    pub details: String,
    pub process_path: String,
}

/// Icon cache (max 50 entries, LRU-like)
const MAX_ICON_CACHE: usize = 50;
const ICON_SIZE: i32 = 16;

// DrawIconEx Flags
const DI_NORMAL: u32 = 0x0003;

lazy_static::lazy_static! {
    static ref ALERT_MESSAGE: Mutex<String> = Mutex::new("PC Watcher - Waiting...".to_string());
    static ref LOG_ENTRIES: Mutex<VecDeque<GuiLogEntry>> = Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES));
    static ref LOG_FILE_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
    static ref CURRENT_SCREENSHOT: Mutex<Option<ScreenshotData>> = Mutex::new(None);
    static ref CURRENT_DETAILS: Mutex<String> = Mutex::new(String::new());
    static ref CURRENT_SCREENSHOT_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);
    // Icon cache: Path -> HICON (stored as usize)
    static ref ICON_CACHE: Mutex<HashMap<String, usize>> = Mutex::new(HashMap::with_capacity(MAX_ICON_CACHE));
    static ref ICON_CACHE_ORDER: Mutex<VecDeque<String>> = Mutex::new(VecDeque::with_capacity(MAX_ICON_CACHE));
}

/// Saves the position to a file
fn save_position(x: i32, y: i32) {
    let config_path = get_config_path();
    if let Some(parent) = config_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let content = format!("{},{}", x, y);
    let _ = fs::write(&config_path, content);
}

/// Loads the position from a file
fn load_position() -> Option<(i32, i32)> {
    let config_path = get_config_path();
    if let Ok(content) = fs::read_to_string(&config_path) {
        let parts: Vec<&str> = content.trim().split(',').collect();
        if parts.len() == 2 {
            if let (Ok(x), Ok(y)) = (parts[0].parse(), parts[1].parse()) {
                return Some((x, y));
            }
        }
    }
    None
}

/// Path to configuration file
fn get_config_path() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            return exe_dir.join("pcwatcher_window.cfg");
        }
    }
    PathBuf::from("pcwatcher_window.cfg")
}

/// Sets the path to the log file (called by logger)
pub fn set_log_file_path(path: PathBuf) {
    let mut log_path = LOG_FILE_PATH.lock();
    *log_path = Some(path);
}

/// Sets the current screenshot with folder path for display
pub fn set_screenshot_with_folder(pixels: Vec<u8>, width: u32, height: u32, folder: PathBuf) {
    {
        let mut screenshot = CURRENT_SCREENSHOT.lock();
        *screenshot = Some(ScreenshotData { pixels, width, height });
    }
    {
        let mut folder_path = CURRENT_SCREENSHOT_FOLDER.lock();
        *folder_path = Some(folder);
    }
    SCREENSHOT_HIDDEN.store(false, Ordering::SeqCst);
    redraw_window();
}

/// Opens the current screenshot folder in Explorer
fn open_screenshot_folder() {
    if let Some(folder) = CURRENT_SCREENSHOT_FOLDER.lock().clone() {
        info!("Opening screenshot folder: {}", folder.display());
        let _ = std::process::Command::new("explorer.exe")
            .arg(&folder)
            .spawn();
    }
}


/// Extracts an icon from an EXE file and caches it
fn get_cached_icon(path: &str) -> Option<HICON> {
    if path.is_empty() || path == "Access denied" {
        return None;
    }

    // Check cache
    {
        let cache = ICON_CACHE.lock();
        if let Some(&icon_ptr) = cache.get(path) {
            if icon_ptr != 0 {
                return Some(HICON(icon_ptr as *mut _));
            }
            return None;
        }
    }

    // Extract icon
    let icon = extract_icon(path);
    let icon_ptr = icon.map(|h| h.0 as usize).unwrap_or(0);

    // Save to cache
    {
        let mut cache = ICON_CACHE.lock();
        let mut order = ICON_CACHE_ORDER.lock();

        // Limit cache size (remove oldest)
        while order.len() >= MAX_ICON_CACHE {
            if let Some(old_path) = order.pop_front() {
                if let Some(old_icon) = cache.remove(&old_path) {
                    if old_icon != 0 {
                        unsafe { let _ = DestroyIcon(HICON(old_icon as *mut _)); }
                    }
                }
            }
        }

        cache.insert(path.to_string(), icon_ptr);
        order.push_back(path.to_string());
    }

    icon
}

/// Extracts the icon from an EXE file
fn extract_icon(path: &str) -> Option<HICON> {
    unsafe {
        let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut small_icon: HICON = HICON::default();

        let count = ExtractIconExW(
            windows::core::PCWSTR(path_wide.as_ptr()),
            0,
            None,
            Some(&mut small_icon),
            1,
        );

        if count > 0 && !small_icon.is_invalid() {
            Some(small_icon)
        } else {
            None
        }
    }
}

/// Extracts the large icon (32x32) from an EXE file
fn extract_large_icon(path: &str) -> Option<HICON> {
    if path.is_empty() || path == "Access denied" {
        return None;
    }
    unsafe {
        let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut large_icon: HICON = HICON::default();

        let count = ExtractIconExW(
            windows::core::PCWSTR(path_wide.as_ptr()),
            0,
            Some(&mut large_icon),
            None,
            1,
        );

        if count > 0 && !large_icon.is_invalid() {
            Some(large_icon)
        } else {
            None
        }
    }
}

/// Extracts all process paths from details (main + parent hierarchy)
fn extract_paths_from_details(details: &str) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    let mut current_label = String::new();
    let mut found_main_path = false;

    for line in details.lines() {
        // Remove characters like │ ├ └ for easier parsing
        let cleaned: String = line.chars()
            .filter(|c| !['│', '├', '└', '─'].contains(c))
            .collect();
        let trimmed = cleaned.trim();

        // Detect parent hierarchy labels (BEFORE path check!)
        if trimmed.contains("Parent:") && !trimmed.contains("Grandparent") && !trimmed.contains("Great-Grandparent") {
            current_label = "Parent".to_string();
        }
        else if trimmed.contains("Grandparent:") && !trimmed.contains("Great-Grandparent") {
            current_label = "Grandparent".to_string();
        }
        else if trimmed.contains("Great-Grandparent:") {
            current_label = "Great-Grandparent".to_string();
        }
        // Extract path
        else if trimmed.starts_with("Path:") {
            if let Some(path) = trimmed.strip_prefix("Path:") {
                let path = path.trim();
                if !path.is_empty() && path != "Access denied" {
                    if !current_label.is_empty() {
                        // Hierarchy path
                        paths.push((current_label.clone(), path.to_string()));
                        current_label.clear();
                    } else if !found_main_path {
                        // Main process path (first path without label)
                        paths.push(("Process".to_string(), path.to_string()));
                        found_main_path = true;
                    }
                }
            }
        }
    }
    paths
}

/// Adds a log entry (called by logger)
pub fn add_log_entry(text: String, event_type: String, details: String, process_path: String) {
    let count = EVENT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;

    if !ALERT_ACTIVE.load(Ordering::SeqCst) {
        let mut msg = ALERT_MESSAGE.lock();
        *msg = format!("PC Watcher - {} Events", count);
    }

    // Pre-cache icon (in background, non-blocking)
    if !process_path.is_empty() {
        let path_clone = process_path.clone();
        std::thread::spawn(move || {
            let _ = get_cached_icon(&path_clone);
        });
    }

    let mut entries = LOG_ENTRIES.lock();
    if entries.len() >= MAX_LOG_ENTRIES {
        entries.pop_front();
    }
    entries.push_back(GuiLogEntry { text, event_type, details, process_path });
    redraw_window();
}

/// Starts the alert window
pub fn start_alert_window() {
    thread::spawn(|| {
        if let Err(e) = create_window() {
            error!("Could not create alert window: {}", e);
        }
    });
    thread::sleep(Duration::from_millis(100));
}

/// Sets the alert status (changes color and text)
pub fn set_alert(process_name: &str, _process_path: &str) {
    ALERT_ACTIVE.store(true, Ordering::SeqCst);
    {
        let mut msg = ALERT_MESSAGE.lock();
        *msg = format!("!! {} !!", process_name);
    }
    redraw_window();

    thread::spawn(|| {
        thread::sleep(Duration::from_secs(5));
        clear_alert();
    });
}

/// Clears the alert status
pub fn clear_alert() {
    ALERT_ACTIVE.store(false, Ordering::SeqCst);
    {
        let count = EVENT_COUNT.load(Ordering::SeqCst);
        let mut msg = ALERT_MESSAGE.lock();
        *msg = format!("PC Watcher - {} Events", count);
    }
    // Screenshot is now preserved!
    redraw_window();
}

/// Redraws the window
fn redraw_window() {
    let hwnd = WINDOW_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let _ = InvalidateRect(hwnd, None, true);
        }
    }
}

/// Creates the window
fn create_window() -> Result<(), String> {
    unsafe {
        let instance = GetModuleHandleW(None)
            .map_err(|e| format!("GetModuleHandle: {}", e))?;

        // Main window class
        let class_name = w!("PCWatcherAlert");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
            lpszClassName: class_name,
            ..Default::default()
        };
        let atom = RegisterClassW(&wc);
        if atom == 0 {
            info!("Window class already registered");
        }

        // Details window class
        let details_class = w!("PCWatcherDetails");
        let wc_details = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(details_window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: details_class,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc_details);

        let (x, y) = load_position().unwrap_or((0, 0));
        info!("Window position loaded: ({}, {})", x, y);

        let title = w!("PC Watcher");

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class_name,
            title,
            WS_POPUP | WS_VISIBLE,
            x, y,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            instance,
            None,
        );

        let hwnd = match hwnd {
            Ok(h) => h,
            Err(e) => {
                error!("CreateWindowExW failed: {}", e);
                return Err(format!("CreateWindowExW: {}", e));
            }
        };

        if hwnd.0.is_null() {
            return Err("Window handle is NULL".to_string());
        }

        WINDOW_HWND.store(hwnd.0 as usize, Ordering::SeqCst);

        // Rounded corners
        let rgn = CreateRoundRectRgn(0, 0, WINDOW_WIDTH + 1, WINDOW_HEIGHT + 1, CORNER_RADIUS, CORNER_RADIUS);
        let _ = SetWindowRgn(hwnd, rgn, true);

        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 230, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, WINDOW_WIDTH, WINDOW_HEIGHT, SWP_SHOWWINDOW | SWP_NOACTIVATE);

        // Timer for regular TOPMOST check (every 3 seconds)
        const TOPMOST_TIMER_ID: usize = 1;
        let _ = SetTimer(hwnd, TOPMOST_TIMER_ID, 3000, None);

        info!("Alert window created");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }

    Ok(())
}

/// Opens the log file in the default editor
fn open_log_file() {
    if let Some(path) = LOG_FILE_PATH.lock().clone() {
        info!("Opening log file: {}", path.display());
        let _ = std::process::Command::new("notepad.exe")
            .arg(&path)
            .spawn();
    }
}

/// Shows the details window
unsafe fn show_details_window(details: String) {
    let instance = GetModuleHandleW(None).unwrap_or_default();
    let details_class = w!("PCWatcherDetails");
    let title = w!("PC Watcher - Details");

    // Save details
    {
        let mut d = CURRENT_DETAILS.lock();
        *d = details;
    }

    // Window position (next to main window)
    let main_hwnd = WINDOW_HWND.load(Ordering::SeqCst);
    let (dx, dy) = if main_hwnd != 0 {
        let mut rect = RECT::default();
        let _ = GetWindowRect(HWND(main_hwnd as *mut _), &mut rect);
        (rect.right + 10, rect.top)
    } else {
        (100, 100)
    };

    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_LAYERED,
        details_class,
        title,
        WS_POPUP | WS_VISIBLE,
        dx, dy,
        DETAILS_WIDTH,
        DETAILS_HEIGHT,
        None,
        None,
        instance,
        None,
    );

    if let Ok(hwnd) = hwnd {
        // Rounded corners
        let rgn = CreateRoundRectRgn(0, 0, DETAILS_WIDTH + 1, DETAILS_HEIGHT + 1, CORNER_RADIUS, CORNER_RADIUS);
        let _ = SetWindowRgn(hwnd, rgn, true);

        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 240, LWA_ALPHA);
        DETAILS_HWND.store(hwnd.0 as usize, Ordering::SeqCst);

        // Load and set icon from EXE resources
        let icon = LoadImageW(
            instance,
            windows::core::PCWSTR(1 as *const u16),
            IMAGE_ICON,
            32, 32,
            LR_DEFAULTCOLOR,
        ).ok().map(|h| HICON(h.0));

        if let Some(icon) = icon {
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(0), LPARAM(icon.0 as isize)); // ICON_SMALL
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(icon.0 as isize)); // ICON_BIG
        }
    }
}

/// Draws a rounded button with text
unsafe fn draw_button(hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, w: i32, h: i32, text: &str, active: bool) {
    let color = if active { COLOR_BUTTON_ACTIVE } else { COLOR_BUTTON_BG };
    let brush = CreateSolidBrush(COLORREF(color));
    let pen = CreatePen(PS_SOLID, 1, COLORREF(color));

    // Save old objects and select new ones
    let old_brush = SelectObject(hdc, brush);
    let old_pen = SelectObject(hdc, pen);

    // Draw rounded rectangle (radius 6)
    let _ = RoundRect(hdc, x, y, x + w, y + h, 6, 6);

    // Restore and delete objects
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));

    // Draw text centered with DrawTextW for true centering
    let _ = SetTextColor(hdc, COLORREF(COLOR_TEXT));
    let mut text_wide: Vec<u16> = text.encode_utf16().collect();
    let mut text_rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let _ = DrawTextW(hdc, &mut text_wide, &mut text_rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

/// Draws the legend with full names
unsafe fn draw_legend(hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32) {
    let items = [
        (COLOR_FOCUS, "Focus"),
        (COLOR_CREATED, "New"),
        (COLOR_SHOWN, "Shown"),
        (COLOR_MINIMIZED, "Min"),
        (COLOR_RESTORED, "Restore"),
        (COLOR_ZORDER, "Z-Order"),
    ];

    let mut offset = 0i32;
    for (color, label) in items {
        // Colored dot
        let dot_rect = RECT { left: x + offset, top: y, right: x + offset + 8, bottom: y + 8 };
        let brush = CreateSolidBrush(COLORREF(color));
        let _ = FillRect(hdc, &dot_rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));

        // Label
        let _ = SetTextColor(hdc, COLORREF(color));
        let label_wide: Vec<u16> = label.encode_utf16().collect();
        let _ = TextOutW(hdc, x + offset + 10, y - 2, &label_wide);

        offset += 10 + (label.len() as i32 * 7) + 8;
    }
}

/// Draws the screenshot thumbnail with rounded corners
unsafe fn draw_screenshot(hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, max_w: i32, max_h: i32) -> bool {
    let screenshot = CURRENT_SCREENSHOT.lock();
    let corner_radius = 8; // Rounding for screenshot preview

    if let Some(ref ss) = *screenshot {
        if SCREENSHOT_HIDDEN.load(Ordering::SeqCst) {
            // Hidden - placeholder with rounded corners
            let clip_rgn = CreateRoundRectRgn(x, y, x + max_w + 1, y + max_h + 1, corner_radius, corner_radius);
            SelectClipRgn(hdc, clip_rgn);

            let placeholder_rect = RECT { left: x, top: y, right: x + max_w, bottom: y + max_h };
            let brush = CreateSolidBrush(COLORREF(0x00303030));
            let _ = FillRect(hdc, &placeholder_rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            let _ = SetTextColor(hdc, COLORREF(0x00888888));
            let text: Vec<u16> = "[Hidden]".encode_utf16().collect();
            let _ = TextOutW(hdc, x + 65, y + max_h / 2 - 20, &text);
            let text2: Vec<u16> = "Click: Show".encode_utf16().collect();
            let _ = TextOutW(hdc, x + 55, y + max_h / 2, &text2);

            // Reset clipping
            SelectClipRgn(hdc, None);
            let _ = DeleteObject(HGDIOBJ(clip_rgn.0 as *mut _));
            return true;
        }

        // Calculate scaling
        let scale_w = max_w as f32 / ss.width as f32;
        let scale_h = max_h as f32 / ss.height as f32;
        let scale = scale_w.min(scale_h);
        let dst_w = (ss.width as f32 * scale) as i32;
        let dst_h = (ss.height as f32 * scale) as i32;

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: ss.width as i32,
                biHeight: -(ss.height as i32),
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hdc_mem = CreateCompatibleDC(hdc);
        let hbm = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0);

        if let Ok(hbm) = hbm {
            if !bits.is_null() {
                let row_size = ((ss.width * 3 + 3) / 4) * 4;
                let dst_ptr = bits as *mut u8;

                for row in 0..ss.height {
                    for col in 0..ss.width {
                        let src_idx = ((row * ss.width + col) * 3) as usize;
                        let dst_idx = (row * row_size + col * 3) as usize;
                        if src_idx + 2 < ss.pixels.len() {
                            *dst_ptr.add(dst_idx) = ss.pixels[src_idx + 2];
                            *dst_ptr.add(dst_idx + 1) = ss.pixels[src_idx + 1];
                            *dst_ptr.add(dst_idx + 2) = ss.pixels[src_idx];
                        }
                    }
                }

                // Set clipping region for rounded corners
                let clip_rgn = CreateRoundRectRgn(x, y, x + dst_w + 1, y + dst_h + 1, corner_radius, corner_radius);
                SelectClipRgn(hdc, clip_rgn);

                let old_bm = SelectObject(hdc_mem, hbm);
                let _ = StretchBlt(hdc, x, y, dst_w, dst_h, hdc_mem, 0, 0, ss.width as i32, ss.height as i32, SRCCOPY);
                SelectObject(hdc_mem, old_bm);

                // Reset clipping
                SelectClipRgn(hdc, None);
                let _ = DeleteObject(HGDIOBJ(clip_rgn.0 as *mut _));
            }
            let _ = DeleteObject(HGDIOBJ(hbm.0));
        }
        let _ = DeleteDC(hdc_mem);
        return true;
    }

    // No screenshot - also with rounded corners
    let clip_rgn = CreateRoundRectRgn(x, y, x + max_w + 1, y + max_h + 1, corner_radius, corner_radius);
    SelectClipRgn(hdc, clip_rgn);

    let placeholder_rect = RECT { left: x, top: y, right: x + max_w, bottom: y + max_h };
    let brush = CreateSolidBrush(COLORREF(0x00303030));
    let _ = FillRect(hdc, &placeholder_rect, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));

    let _ = SetTextColor(hdc, COLORREF(0x00666666));
    let text: Vec<u16> = "(No screenshot)".encode_utf16().collect();
    let _ = TextOutW(hdc, x + 45, y + max_h / 2 - 8, &text);

    // Reset clipping
    SelectClipRgn(hdc, None);
    let _ = DeleteObject(HGDIOBJ(clip_rgn.0 as *mut _));
    false
}

/// Window Procedure for main window
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);

            // === HEADER ===
            let header_rect = RECT { left: 0, top: 0, right: rect.right, bottom: HEADER_HEIGHT };
            let header_color = if ALERT_ACTIVE.load(Ordering::SeqCst) { COLOR_ALERT } else { COLOR_NORMAL };
            let brush = CreateSolidBrush(COLORREF(header_color));
            let _ = FillRect(hdc, &header_rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, COLORREF(COLOR_TEXT));

            // Header text
            let text = ALERT_MESSAGE.lock().clone();
            let text_wide: Vec<u16> = text.encode_utf16().collect();
            let _ = TextOutW(hdc, 10, 10, &text_wide);

            // Buttons in header: [TRAY] [MINIMIZE] [PINNED/UNPIN]
            let is_pinned = WINDOW_PINNED.load(Ordering::SeqCst);
            let pin_btn_w = if is_pinned { 70 } else { 60 };
            let min_btn_w = 80;
            let tray_btn_w = 50;
            let right_margin = 10;
            let pin_btn_x = rect.right - pin_btn_w - right_margin;
            let min_btn_x = pin_btn_x - min_btn_w - 5;
            let tray_btn_x = min_btn_x - tray_btn_w - 5;
            let btn_y = (HEADER_HEIGHT - BTN_HEIGHT) / 2;

            // Tray button
            draw_button(hdc, tray_btn_x, btn_y, tray_btn_w, BTN_HEIGHT, "TRAY", false);

            // Minimize button
            draw_button(hdc, min_btn_x, btn_y, min_btn_w, BTN_HEIGHT, "MINIMIZE", false);

            // Pin button
            let pin_text = if is_pinned { "PINNED" } else { "UNPIN" };
            draw_button(hdc, pin_btn_x, btn_y, pin_btn_w, BTN_HEIGHT, pin_text, is_pinned);

            // === LOG AREA (left) ===
            let log_rect = RECT { left: 0, top: HEADER_HEIGHT, right: LOG_AREA_WIDTH, bottom: rect.bottom };
            let log_brush = CreateSolidBrush(COLORREF(COLOR_LOG_BG));
            let _ = FillRect(hdc, &log_rect, log_brush);
            let _ = DeleteObject(HGDIOBJ(log_brush.0));

            // Legend with full names
            draw_legend(hdc, 5, HEADER_HEIGHT + 5);

            // Log entries with icons
            let entries = LOG_ENTRIES.lock();
            let mut y = HEADER_HEIGHT + 22;
            for entry in entries.iter() {
                let color = match entry.event_type.as_str() {
                    "FOCUS" => COLOR_FOCUS,
                    "CREATED" => COLOR_CREATED,
                    "SHOWN" => COLOR_SHOWN,
                    "MINIMIZED" => COLOR_MINIMIZED,
                    "RESTORED" => COLOR_RESTORED,
                    "Z-ORDER" => COLOR_ZORDER,
                    _ => COLOR_TEXT,
                };
                let _ = SetTextColor(hdc, COLORREF(color));

                // Draw icon (if available)
                let text_x = if let Some(icon) = get_cached_icon(&entry.process_path) {
                    let _ = DrawIconEx(hdc, 5, y, icon, ICON_SIZE, ICON_SIZE, 0, None, DI_FLAGS(DI_NORMAL));
                    5 + ICON_SIZE + 4 // After icon: 4px spacing
                } else {
                    5 + ICON_SIZE + 4 // Same spacing without icon for alignment
                };

                let max_chars = 54; // Slightly less due to icon
                let display = if entry.text.len() > max_chars {
                    format!("{}...", &entry.text[..max_chars - 3])
                } else {
                    entry.text.clone()
                };
                let entry_wide: Vec<u16> = display.encode_utf16().collect();
                let _ = TextOutW(hdc, text_x, y, &entry_wide);
                y += 18;
            }
            drop(entries);

            // === SCREENSHOT AREA (right) ===
            let ss_x = LOG_AREA_WIDTH + 10;
            let ss_y = HEADER_HEIGHT + 5;

            // Frame
            let ss_frame = RECT {
                left: ss_x - 2, top: ss_y - 2,
                right: ss_x + SCREENSHOT_WIDTH + 2, bottom: ss_y + SCREENSHOT_HEIGHT + 2,
            };
            let frame_brush = CreateSolidBrush(COLORREF(0x00444444));
            let _ = FillRect(hdc, &ss_frame, frame_brush);
            let _ = DeleteObject(HGDIOBJ(frame_brush.0));

            // Fill area below screenshot (first, then draw over)
            let bottom_rect = RECT {
                left: LOG_AREA_WIDTH, top: HEADER_HEIGHT,
                right: rect.right, bottom: rect.bottom,
            };
            let bottom_brush = CreateSolidBrush(COLORREF(COLOR_LOG_BG));
            let _ = FillRect(hdc, &bottom_rect, bottom_brush);
            let _ = DeleteObject(HGDIOBJ(bottom_brush.0));

            // Draw screenshot
            let has_screenshot = draw_screenshot(hdc, ss_x, ss_y, SCREENSHOT_WIDTH, SCREENSHOT_HEIGHT);

            // Text below screenshot
            let _ = SetTextColor(hdc, COLORREF(0x00888888));

            // If screenshot visible: "(Hide)" link + "Click: Open folder"
            let is_hidden = SCREENSHOT_HIDDEN.load(Ordering::SeqCst);
            if has_screenshot && !is_hidden {
                let hide_text: Vec<u16> = "(Hide)".encode_utf16().collect();
                let _ = TextOutW(hdc, ss_x + 75, ss_y + SCREENSHOT_HEIGHT + 8, &hide_text);

                let click_text: Vec<u16> = "Click: Folder".encode_utf16().collect();
                let _ = TextOutW(hdc, ss_x + 55, ss_y + SCREENSHOT_HEIGHT + 26, &click_text);
            }

            // General info
            let info1: Vec<u16> = "Double-click: Details".encode_utf16().collect();
            let _ = TextOutW(hdc, ss_x, ss_y + SCREENSHOT_HEIGHT + 50, &info1);
            let info2: Vec<u16> = "Right-click: Log".encode_utf16().collect();
            let _ = TextOutW(hdc, ss_x, ss_y + SCREENSHOT_HEIGHT + 68, &info2);

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            // Calculate button positions (as in WM_PAINT)
            let is_pinned = WINDOW_PINNED.load(Ordering::SeqCst);
            let pin_btn_w = if is_pinned { 70 } else { 60 };
            let min_btn_w = 80;
            let tray_btn_w = 50;
            let right_margin = 10;
            let pin_btn_x = WINDOW_WIDTH - pin_btn_w - right_margin;
            let min_btn_x = pin_btn_x - min_btn_w - 5;
            let tray_btn_x = min_btn_x - tray_btn_w - 5;
            let btn_y = (HEADER_HEIGHT - BTN_HEIGHT) / 2;

            // Screenshot area positions
            let ss_x = LOG_AREA_WIDTH + 10;
            let ss_y = HEADER_HEIGHT + 5;

            // "(Hide)" link below screenshot clicked?
            let hide_link_y = ss_y + SCREENSHOT_HEIGHT + 8;
            if x >= ss_x + 60 && x <= ss_x + 160 && y >= hide_link_y && y <= hide_link_y + 16 {
                if !SCREENSHOT_HIDDEN.load(Ordering::SeqCst) {
                    SCREENSHOT_HIDDEN.store(true, Ordering::SeqCst);
                    let _ = InvalidateRect(hwnd, None, true);
                    return LRESULT(0);
                }
            }

            // Screenshot image clicked? -> Open folder
            if x >= ss_x && x <= ss_x + SCREENSHOT_WIDTH && y >= ss_y && y <= ss_y + SCREENSHOT_HEIGHT {
                if SCREENSHOT_HIDDEN.load(Ordering::SeqCst) {
                    // Hidden -> show again
                    SCREENSHOT_HIDDEN.store(false, Ordering::SeqCst);
                    let _ = InvalidateRect(hwnd, None, true);
                } else {
                    // Visible -> open folder
                    open_screenshot_folder();
                }
                return LRESULT(0);
            }

            // Minimize button? (normal taskbar minimization)
            if x >= min_btn_x && x <= min_btn_x + min_btn_w && y >= btn_y && y <= btn_y + BTN_HEIGHT {
                WINDOW_MINIMIZED.store(true, Ordering::SeqCst);
                // Hide window, change style, then show minimized again
                // This forces Windows to update the taskbar icon
                let _ = ShowWindow(hwnd, SW_HIDE);
                // Remove TOOLWINDOW AND add APPWINDOW for taskbar
                let current_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
                let new_style = (current_style & !(WS_EX_TOOLWINDOW.0 as i32)) | (WS_EX_APPWINDOW.0 as i32);
                SetWindowLongW(hwnd, GWL_EXSTYLE, new_style);
                // Show window minimized again - now with taskbar icon
                let _ = ShowWindow(hwnd, SW_SHOWMINIMIZED);
                return LRESULT(0);
            }

            // Tray button? (minimize to tray - hide window)
            if x >= tray_btn_x && x <= tray_btn_x + tray_btn_w && y >= btn_y && y <= btn_y + BTN_HEIGHT {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return LRESULT(0);
            }

            // Pin button? (far right)
            if x >= pin_btn_x && x <= pin_btn_x + pin_btn_w && y >= btn_y && y <= btn_y + BTN_HEIGHT {
                let was_pinned = WINDOW_PINNED.load(Ordering::SeqCst);
                WINDOW_PINNED.store(!was_pinned, Ordering::SeqCst);
                let z_order = if !was_pinned { HWND_TOPMOST } else { HWND_NOTOPMOST };
                let _ = SetWindowPos(hwnd, z_order, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                let _ = InvalidateRect(hwnd, None, true);
                return LRESULT(0);
            }

            // Start dragging
            DRAGGING.store(true, Ordering::SeqCst);
            DRAG_START_X.store(x, Ordering::SeqCst);
            DRAG_START_Y.store(y, Ordering::SeqCst);
            let _ = SetCapture(hwnd);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            if DRAGGING.load(Ordering::SeqCst) {
                let mut cursor_pos = POINT::default();
                let _ = GetCursorPos(&mut cursor_pos);
                let new_x = cursor_pos.x - DRAG_START_X.load(Ordering::SeqCst);
                let new_y = cursor_pos.y - DRAG_START_Y.load(Ordering::SeqCst);
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, new_x, new_y, WINDOW_WIDTH, WINDOW_HEIGHT, SWP_NOACTIVATE | SWP_NOZORDER);
            }
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            if DRAGGING.load(Ordering::SeqCst) {
                DRAGGING.store(false, Ordering::SeqCst);
                let _ = ReleaseCapture();
                let mut rect = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rect);
                save_position(rect.left, rect.top);
            }
            LRESULT(0)
        }

        WM_RBUTTONUP => {
            open_log_file();
            LRESULT(0)
        }

        WM_LBUTTONDBLCLK => {
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

            if y > HEADER_HEIGHT + 22 {
                let entry_index = ((y - HEADER_HEIGHT - 22) / 18) as usize;
                let entries = LOG_ENTRIES.lock();
                if entry_index < entries.len() {
                    let details = entries[entry_index].details.clone();
                    drop(entries);
                    show_details_window(details);
                }
            }
            LRESULT(0)
        }

        WM_SIZE => {
            // Restore from minimized
            if wparam.0 == 0 && WINDOW_MINIMIZED.load(Ordering::SeqCst) {
                WINDOW_MINIMIZED.store(false, Ordering::SeqCst);
                // Back to TOOLWINDOW (no taskbar icon) and remove APPWINDOW
                let current_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
                let new_style = (current_style | (WS_EX_TOOLWINDOW.0 as i32)) & !(WS_EX_APPWINDOW.0 as i32);
                SetWindowLongW(hwnd, GWL_EXSTYLE, new_style);
                if WINDOW_PINNED.load(Ordering::SeqCst) {
                    let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        WM_TIMER => {
            // Timer 1: Check and restore TOPMOST status
            if wparam.0 == 1 && WINDOW_PINNED.load(Ordering::SeqCst) && !WINDOW_MINIMIZED.load(Ordering::SeqCst) {
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            let _ = KillTimer(hwnd, 1);
            PostQuitMessage(0);
            LRESULT(0)
        }

        WM_MOUSEACTIVATE => {
            if WINDOW_PINNED.load(Ordering::SeqCst) {
                LRESULT(3)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Draws a row in the details window with label and value
unsafe fn draw_detail_row(hdc: windows::Win32::Graphics::Gdi::HDC, y: i32, label: &str, value: &str, label_color: u32, value_color: u32) {
    let _ = SetTextColor(hdc, COLORREF(label_color));
    let label_wide: Vec<u16> = label.encode_utf16().collect();
    let _ = TextOutW(hdc, 15, y, &label_wide);

    let _ = SetTextColor(hdc, COLORREF(value_color));
    // Truncate value if too long
    let max_len = 60;
    let display_val = if value.len() > max_len {
        format!("{}...", &value[..max_len])
    } else {
        value.to_string()
    };
    let val_wide: Vec<u16> = display_val.encode_utf16().collect();
    let _ = TextOutW(hdc, 130, y, &val_wide);
}

/// Window Procedure for details window
unsafe extern "system" fn details_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);

            // Background with gradient effect (two areas)
            let brush = CreateSolidBrush(COLORREF(COLOR_DETAILS_BG));
            let _ = FillRect(hdc, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            // Header
            let header_rect = RECT { left: 0, top: 0, right: rect.right, bottom: 35 };
            let header_brush = CreateSolidBrush(COLORREF(COLOR_NORMAL));
            let _ = FillRect(hdc, &header_rect, header_brush);
            let _ = DeleteObject(HGDIOBJ(header_brush.0));

            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, COLORREF(COLOR_TEXT));

            let title: Vec<u16> = "Event Details".encode_utf16().collect();
            let _ = TextOutW(hdc, 15, 10, &title);

            // Close button hint on right
            let close_hint: Vec<u16> = "[X] Close".encode_utf16().collect();
            let _ = SetTextColor(hdc, COLORREF(0x00AAAAAA));
            let _ = TextOutW(hdc, rect.right - 120, 10, &close_hint);

            // Parse and display details structured
            let details = CURRENT_DETAILS.lock().clone();
            let label_color = 0x0088AACC;  // Light blue for labels
            let value_color = 0x00FFFFFF;  // White for values
            let section_color = 0x0000FF88; // Green for sections

            // Extract and display icons (32x32)
            let paths = extract_paths_from_details(&details);
            let icon_size: i32 = 32;
            let icon_spacing: i32 = 40;
            let icons_y: i32 = 45;

            let mut icon_x: i32 = 15;
            let mut icons_drawn = Vec::new();
            for (label, path) in &paths {
                if let Some(icon) = extract_large_icon(path) {
                    let _ = DrawIconEx(hdc, icon_x, icons_y, icon, icon_size, icon_size, 0, None, DI_FLAGS(DI_NORMAL));
                    icons_drawn.push((icon_x, label.clone(), icon));
                    icon_x += icon_spacing;
                }
            }

            // Labels below icons
            let _ = SetTextColor(hdc, COLORREF(0x00888888));
            for (x, label, icon) in &icons_drawn {
                let label_short = match label.as_str() {
                    "Process" => "App",
                    "Parent" => "Par",
                    "Grandparent" => "G-P",
                    "Great-Grandparent" => "G-G",
                    _ => &label[..3.min(label.len())],
                };
                let label_wide: Vec<u16> = label_short.encode_utf16().collect();
                let _ = TextOutW(hdc, *x, icons_y + icon_size + 2, &label_wide);
                // Free icon (not cached for large icons)
                let _ = DestroyIcon(*icon);
            }

            let mut y = if icons_drawn.is_empty() { 50 } else { icons_y + icon_size + 22 };
            let line_height = 20;

            for line in details.lines() {
                if line.trim().is_empty() {
                    y += 8; // Empty line = small spacing
                    continue;
                }

                // Detect section headers (e.g., "=== Process ===")
                if line.contains("===") || line.starts_with("---") {
                    y += 5;
                    // Separator line
                    let sep_rect = RECT { left: 10, top: y, right: rect.right - 10, bottom: y + 1 };
                    let sep_brush = CreateSolidBrush(COLORREF(0x00444444));
                    let _ = FillRect(hdc, &sep_rect, sep_brush);
                    let _ = DeleteObject(HGDIOBJ(sep_brush.0));
                    y += 8;

                    let _ = SetTextColor(hdc, COLORREF(section_color));
                    let section_text = line.replace("=", "").replace("-", "").trim().to_string();
                    let section_wide: Vec<u16> = section_text.encode_utf16().collect();
                    let _ = TextOutW(hdc, 15, y, &section_wide);
                    y += line_height + 5;
                } else if line.contains(":") {
                    // Key: Value line
                    let parts: Vec<&str> = line.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        draw_detail_row(hdc, y, parts[0].trim(), parts[1].trim(), label_color, value_color);
                    } else {
                        let _ = SetTextColor(hdc, COLORREF(value_color));
                        let line_wide: Vec<u16> = line.encode_utf16().collect();
                        let _ = TextOutW(hdc, 15, y, &line_wide);
                    }
                    y += line_height;
                } else {
                    // Normal line
                    let _ = SetTextColor(hdc, COLORREF(0x00CCCCCC));
                    let line_wide: Vec<u16> = line.encode_utf16().collect();
                    let _ = TextOutW(hdc, 15, y, &line_wide);
                    y += line_height;
                }

                if y > rect.bottom - 30 {
                    // Hint that more text is available
                    let _ = SetTextColor(hdc, COLORREF(0x00888888));
                    let more: Vec<u16> = "... (more)".encode_utf16().collect();
                    let _ = TextOutW(hdc, 15, rect.bottom - 25, &more);
                    break;
                }
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }

        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            // Close window on click
            let _ = DestroyWindow(hwnd);
            DETAILS_HWND.store(0, Ordering::SeqCst);
            LRESULT(0)
        }

        WM_DESTROY => {
            DETAILS_HWND.store(0, Ordering::SeqCst);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Closes the alert window
pub fn close_alert_window() {
    let hwnd = WINDOW_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(HWND(hwnd as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Restores the alert window from tray
pub fn restore_from_tray() {
    let hwnd_val = WINDOW_HWND.load(Ordering::SeqCst);
    if hwnd_val != 0 {
        let is_pinned = WINDOW_PINNED.load(Ordering::SeqCst);
        unsafe {
            let hwnd = HWND(hwnd_val as *mut _);
            // Show window
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            // Set TOPMOST multiple times for reliability
            if is_pinned {
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
            }
            let _ = InvalidateRect(hwnd, None, true);
        }
        // Delayed re-setting of TOPMOST (Windows is sometimes slow)
        if is_pinned {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(50));
                unsafe {
                    let hwnd = HWND(hwnd_val as *mut _);
                    let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            });
        }
    }
}
