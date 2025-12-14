//! System Tray Icon
//!
//! Shows a tray icon with context menu to exit the application.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tracing::{info, error};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM, LRESULT, POINT};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::LoadImageW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

const WM_TRAYICON: u32 = WM_USER + 1;
const ID_TRAY_EXIT: u32 = 1001;

static TRAY_HWND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SHOULD_EXIT: AtomicBool = AtomicBool::new(false);

/// Checks if exit was requested
pub fn should_exit() -> bool {
    SHOULD_EXIT.load(Ordering::SeqCst)
}

/// Requests exit (callable from outside)
pub fn request_exit() {
    SHOULD_EXIT.store(true, Ordering::SeqCst);
}

/// Starts the tray icon in its own thread
pub fn start_tray() {
    thread::spawn(|| {
        if let Err(e) = create_tray_window() {
            error!("Tray window error: {}", e);
        }
    });
}

/// Removes the tray icon
pub fn stop_tray() {
    let hwnd = TRAY_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(HWND(hwnd as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Creates the invisible window for tray messages
fn create_tray_window() -> Result<(), String> {
    unsafe {
        let instance = GetModuleHandleW(None)
            .map_err(|e| format!("GetModuleHandle: {}", e))?;

        let class_name = w!("PCWatcherTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };

        let atom = RegisterClassW(&wc);
        if atom == 0 {
            // Class already exists - OK
        }

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("PC Watcher Tray"),
            WINDOW_STYLE(0),
            0, 0, 0, 0,
            None,
            None,
            instance,
            None,
        );

        let hwnd = match hwnd {
            Ok(h) => h,
            Err(e) => return Err(format!("CreateWindowExW: {}", e)),
        };

        TRAY_HWND.store(hwnd.0 as usize, Ordering::SeqCst);

        // Add tray icon
        add_tray_icon(hwnd)?;

        info!("Tray icon created");

        // Message Loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }

        // Remove icon when exiting
        remove_tray_icon(hwnd);
    }

    Ok(())
}

/// Adds the tray icon
unsafe fn add_tray_icon(hwnd: HWND) -> Result<(), String> {
    let instance = GetModuleHandleW(None).unwrap_or_default();

    // Load icon from EXE resources (ID 1 is the main icon)
    let icon = LoadImageW(
        instance,
        windows::core::PCWSTR(1 as *const u16), // Resource ID 1
        IMAGE_ICON,
        32, 32,
        LR_DEFAULTCOLOR,
    ).ok()
        .map(|h| HICON(h.0))
        .unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());

    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: icon,
        ..Default::default()
    };

    // Set tooltip
    let tip = "PC Watcher - Right-click to exit";
    let tip_wide: Vec<u16> = tip.encode_utf16().collect();
    for (i, &c) in tip_wide.iter().enumerate() {
        if i < 127 {
            nid.szTip[i] = c;
        }
    }

    if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
        return Err("Shell_NotifyIconW ADD failed".to_string());
    }

    Ok(())
}

/// Removes the tray icon
unsafe fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

/// Shows the context menu
unsafe fn show_context_menu(hwnd: HWND) {
    let menu = CreatePopupMenu().unwrap_or_default();

    let exit_text = w!("Exit");
    let _ = AppendMenuW(menu, MF_STRING, ID_TRAY_EXIT as usize, exit_text);

    // Get cursor position
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    // Bring window to foreground (needed for correct menu behavior)
    let _ = SetForegroundWindow(hwnd);

    // Show menu
    let _ = TrackPopupMenu(
        menu,
        TPM_BOTTOMALIGN | TPM_LEFTALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );

    let _ = DestroyMenu(menu);
}

/// Window Procedure for tray messages
unsafe extern "system" fn tray_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_LBUTTONDBLCLK {
                // Double-click: Restore GUI from tray
                crate::alert_window::restore_from_tray();
            } else if event == WM_RBUTTONUP {
                // Right-click: Context menu
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }

        WM_COMMAND => {
            let cmd = (wparam.0 & 0xFFFF) as u32;
            if cmd == ID_TRAY_EXIT {
                info!("Exit requested via tray menu");
                SHOULD_EXIT.store(true, Ordering::SeqCst);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
