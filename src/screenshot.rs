//! Screenshot Module
//!
//! Takes screenshots on alerts and saves them as JPEG in the log directory.
//! 3 screenshots with delay: immediately, +200ms, +500ms
//! Captures only the focused window, not the entire screen.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::fs;
use chrono::Local;
use tracing::{info, error};
use image::{ImageBuffer, Rgb};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetDC, ReleaseDC, CreateCompatibleDC, CreateCompatibleBitmap,
    SelectObject, GetDIBits, DeleteDC, DeleteObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowRect,
};

/// Screenshot directory (in log folder)
fn get_screenshot_dir() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            return exe_dir.join("logs");
        }
    }
    PathBuf::from(".").join("logs")
}

/// Deletes all screenshot subfolders (called at startup)
pub fn cleanup_screenshots() {
    let dir = get_screenshot_dir();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            // Delete subfolders (those starting with date)
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    // Folders starting with date (e.g., "2025-12-14_...")
                    if name.len() >= 10 && name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        if let Err(e) = fs::remove_dir_all(&path) {
                            error!("Could not delete screenshot folder: {} - {}", path.display(), e);
                        }
                    }
                }
            }
            // Also delete old JPGs directly in folder (compatibility)
            if let Some(ext) = path.extension() {
                if ext == "jpg" || ext == "jpeg" {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
    info!("Screenshots cleaned up");
}

/// Starts screenshot thread for an alert
/// Takes 3 screenshots: immediately, +200ms, +500ms
/// Screenshots are saved in subfolder: logs/YYYY-MM-DD_HH-MM-SS_ProcessName/
pub fn capture_alert_screenshots(process_name: String) {
    thread::spawn(move || {
        let base_dir = get_screenshot_dir();

        // Subfolder with date, time and process name
        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let folder_name = format!("{}_{}", timestamp, sanitize_filename(&process_name));
        let screenshot_dir = base_dir.join(&folder_name);

        if let Err(e) = fs::create_dir_all(&screenshot_dir) {
            error!("Could not create screenshot folder: {}", e);
            return;
        }

        // Screenshot 1: Immediately - also send to GUI
        match capture_foreground_window() {
            Ok((pixels, width, height)) => {
                // Send to GUI for preview + folder path
                crate::alert_window::set_screenshot_with_folder(
                    pixels.clone(),
                    width as u32,
                    height as u32,
                    screenshot_dir.clone()
                );

                // Save as JPEG
                if let Err(e) = save_screenshot(&screenshot_dir, "screenshot_1", &pixels, width, height) {
                    error!("Screenshot 1 save failed: {}", e);
                }
            }
            Err(e) => error!("Screenshot 1 failed: {}", e),
        }

        // Screenshot 2: +200ms
        thread::sleep(Duration::from_millis(200));
        if let Err(e) = capture_and_save(&screenshot_dir, "screenshot_2") {
            error!("Screenshot 2 failed: {}", e);
        }

        // Screenshot 3: +500ms (300ms after screenshot 2)
        thread::sleep(Duration::from_millis(300));
        if let Err(e) = capture_and_save(&screenshot_dir, "screenshot_3") {
            error!("Screenshot 3 failed: {}", e);
        }

        info!("3 screenshots created in: {}", screenshot_dir.display());
    });
}

/// Sanitizes filename
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(30)
        .collect()
}

/// Takes a screenshot and saves it as JPEG
fn capture_and_save(dir: &PathBuf, name: &str) -> Result<(), String> {
    let (pixels, width, height) = capture_foreground_window()?;
    save_screenshot(dir, name, &pixels, width, height)
}

/// Saves pixel data as JPEG
fn save_screenshot(dir: &PathBuf, name: &str, pixels: &[u8], width: i32, height: i32) -> Result<(), String> {
    // Create ImageBuffer (RGB)
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_raw(
        width as u32,
        height as u32,
        pixels.to_vec(),
    ).ok_or("Could not create ImageBuffer")?;

    // Save as JPEG
    let path = dir.join(format!("{}.jpg", name));
    img.save(&path).map_err(|e| format!("JPEG save failed: {}", e))?;

    Ok(())
}

/// Gets the size of the focused window
fn get_window_size(hwnd: HWND) -> Result<(i32, i32, i32, i32), String> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)
            .map_err(|_| "GetWindowRect failed".to_string())?;

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        // Check minimum size
        if width <= 0 || height <= 0 {
            return Err("Window has invalid size".to_string());
        }

        Ok((rect.left, rect.top, width, height))
    }
}

/// Takes a screenshot of the focused window
/// Uses PrintWindow to capture only the window itself (without overlapping windows)
fn capture_foreground_window() -> Result<(Vec<u8>, i32, i32), String> {
    unsafe {
        // Get focused window
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Err("No focused window".to_string());
        }

        let (_x, _y, width, height) = get_window_size(hwnd)?;

        // Get device context of window
        let hdc_window = GetDC(hwnd);
        if hdc_window.is_invalid() {
            return Err("GetDC failed".to_string());
        }

        // Create compatible DC
        let hdc_mem = CreateCompatibleDC(hdc_window);
        if hdc_mem.is_invalid() {
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleDC failed".to_string());
        }

        // Create bitmap
        let hbitmap = CreateCompatibleBitmap(hdc_window, width, height);
        if hbitmap.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleBitmap failed".to_string());
        }

        // Select bitmap
        let old_bitmap = SelectObject(hdc_mem, hbitmap);

        // PrintWindow: Draws the window directly to our DC
        // PW_RENDERFULLCONTENT (2) for better compatibility with modern apps
        let print_result = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(2));

        if !print_result.as_bool() {
            // Fallback: Try again without flag
            let _ = PrintWindow(hwnd, hdc_mem, PRINT_WINDOW_FLAGS(0));
        }

        // Extract pixel data
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // Negative = Top-Down
                biPlanes: 1,
                biBitCount: 24, // RGB
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let row_size = ((width * 3 + 3) / 4) * 4; // DWORD-aligned
        let mut pixels: Vec<u8> = vec![0; (row_size * height) as usize];

        let lines = GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // Cleanup
        SelectObject(hdc_mem, old_bitmap);
        let _ = DeleteObject(hbitmap);
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);

        if lines == 0 {
            return Err("GetDIBits failed".to_string());
        }

        // Convert BGR to RGB and remove padding
        let mut rgb_pixels: Vec<u8> = Vec::with_capacity((width * height * 3) as usize);
        for row in 0..height {
            let row_start = (row * row_size) as usize;
            for col in 0..width {
                let pixel_start = row_start + (col * 3) as usize;
                // BGR -> RGB
                rgb_pixels.push(pixels[pixel_start + 2]); // R
                rgb_pixels.push(pixels[pixel_start + 1]); // G
                rgb_pixels.push(pixels[pixel_start]);     // B
            }
        }

        Ok((rgb_pixels, width, height))
    }
}
