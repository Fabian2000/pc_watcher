//! Event Hook System
//!
//! Uses Windows SetWinEventHook to capture all window events.

use anyhow::Result;
use crossbeam_channel::{bounded, Sender, Receiver};
use once_cell::sync::OnceCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{
    SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, TranslateMessage, DispatchMessageW, PostThreadMessageW,
    MSG, WM_QUIT, GetForegroundWindow, IsWindowVisible, IsIconic,
    SetWindowsHookExW, UnhookWindowsHookEx, CallNextHookEx,
    HHOOK, WH_MOUSE_LL,
    WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_MBUTTONDOWN,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use std::sync::atomic::AtomicU64;

// Windows Event constants (must be defined as u32)
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_OBJECT_CREATE: u32 = 0x8000;
const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_FOCUS: u32 = 0x8005;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const EVENT_OBJECT_REORDER: u32 = 0x8004;  // Z-Order change (Topmost!)
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

use crate::logger::LogEntry;
use crate::process_info;

/// Global channel sender for event data
static EVENT_SENDER: OnceCell<Sender<WindowEvent>> = OnceCell::new();

/// Shutdown flag
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Thread ID for message loop
static MESSAGE_THREAD_ID: OnceCell<u32> = OnceCell::new();

/// Timestamp of last mouse click (in milliseconds since program start)
static LAST_MOUSE_CLICK_MS: AtomicU64 = AtomicU64::new(0);

/// Mouse hook handle (as usize because HHOOK is not Sync)
static MOUSE_HOOK_PTR: AtomicUsize = AtomicUsize::new(0);

/// Time window for "recently clicked" (in milliseconds)
const CLICK_WINDOW_MS: u64 = 500; // 500ms

/// Window event types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Foreground,
    Created,
    Shown,
    Focus,
    Minimized,
    Restored,
    ZOrderChanged,  // Topmost/Z-Order change
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Foreground => "FOCUS",
            EventType::Created => "CREATED",
            EventType::Shown => "SHOWN",
            EventType::Focus => "FOCUS",
            EventType::Minimized => "MINIMIZED",
            EventType::Restored => "RESTORED",
            EventType::ZOrderChanged => "Z-ORDER",
        }
    }
}

/// Window event data
#[derive(Debug, Clone)]
pub struct WindowEvent {
    pub event_type: EventType,
    pub hwnd: isize,
    pub timestamp: chrono::DateTime<chrono::Local>,
}

/// Checks if a mouse click occurred recently
fn was_recent_mouse_click() -> bool {
    let last_click = LAST_MOUSE_CLICK_MS.load(Ordering::SeqCst);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // If the last click was within the time window
    now.saturating_sub(last_click) < CLICK_WINDOW_MS
}

/// Low-Level Mouse Hook Callback
unsafe extern "system" fn mouse_hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        // On mouse click (left, right, middle) save timestamp
        if msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN || msg == WM_MBUTTONDOWN {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            LAST_MOUSE_CLICK_MS.store(now, Ordering::SeqCst);
        }
    }

    // Forward event to next hook
    CallNextHookEx(None, code, wparam, lparam)
}

/// Callback function for Windows Events
unsafe extern "system" fn win_event_proc(
    _h_win_event_hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _dw_event_thread: u32,
    _dwms_event_time: u32,
) {
    // Only top-level windows (id_object == 0)
    if id_object != 0 {
        return;
    }

    let event_type = match event {
        x if x == EVENT_SYSTEM_FOREGROUND => EventType::Foreground,
        x if x == EVENT_OBJECT_CREATE => EventType::Created,
        x if x == EVENT_OBJECT_SHOW => EventType::Shown,
        x if x == EVENT_OBJECT_FOCUS => EventType::Focus,
        x if x == EVENT_SYSTEM_MINIMIZESTART => EventType::Minimized,
        x if x == EVENT_SYSTEM_MINIMIZEEND => EventType::Restored,
        x if x == EVENT_OBJECT_REORDER => EventType::ZOrderChanged,
        _ => return,
    };

    // For CREATE/SHOW: Only visible, non-minimized windows
    if matches!(event_type, EventType::Created | EventType::Shown) {
        if !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        // Check if window is minimized (then ignore for SHOW)
        if IsIconic(hwnd).as_bool() {
            return;
        }
    }

    let window_event = WindowEvent {
        event_type,
        hwnd: hwnd.0 as isize,
        timestamp: chrono::Local::now(),
    };

    // Send event to worker thread
    if let Some(sender) = EVENT_SENDER.get() {
        let _ = sender.try_send(window_event);
    }
}

/// Worker thread that processes and logs events
fn event_worker(receiver: Receiver<WindowEvent>, log_sender: Sender<LogEntry>) {
    info!("Event worker started");

    // Duplicate filter: Remember last events
    let mut last_events: Vec<(isize, EventType, i64)> = Vec::with_capacity(10);

    while !SHUTDOWN.load(Ordering::Relaxed) {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                // Duplicate check (same window + event within 100ms)
                let now_ms = event.timestamp.timestamp_millis();
                let is_duplicate = last_events.iter().any(|(hwnd, etype, time)| {
                    *hwnd == event.hwnd && *etype == event.event_type && (now_ms - time).abs() < 100
                });

                if is_duplicate {
                    continue;
                }

                // Remember event
                last_events.push((event.hwnd, event.event_type, now_ms));
                if last_events.len() > 10 {
                    last_events.remove(0);
                }

                // Collect process information (with cache for performance)
                let hwnd = HWND(event.hwnd as *mut _);
                let proc_info = process_info::get_process_info_cached(hwnd);

                // Warning for suspicious processes (on FOCUS, SHOWN, CREATED)
                let dominated_event = matches!(
                    event.event_type,
                    EventType::Foreground | EventType::Shown | EventType::Created
                );

                // Check for suspicious processes
                let is_suspicious_process = crate::notification::is_suspicious_process(&proc_info.process_name);

                // Check for focus change without mouse click (suspicious!)
                let focus_without_click = event.event_type == EventType::Foreground && !was_recent_mouse_click();

                if dominated_event && is_suspicious_process {
                    warn!("!!! SUSPICIOUS PROCESS: {} - {} !!!",
                        proc_info.process_name, proc_info.process_path);
                    crate::alert_window::set_alert(
                        &proc_info.process_name,
                        &proc_info.process_path
                    );
                    // Take screenshots (3 with delay)
                    crate::screenshot::capture_alert_screenshots(proc_info.process_name.clone());
                } else if focus_without_click {
                    // Focus change without mouse click - suspicious!
                    // But not for own windows or desktop
                    let proc_lower = proc_info.process_name.to_lowercase();
                    let is_ignored = proc_lower == "pc_watcher"
                        || proc_lower == "pc_watcher.exe"
                        || proc_lower == "explorer"
                        || proc_lower == "explorer.exe"
                        || proc_info.window_class == "Shell_TrayWnd"
                        || proc_info.window_class == "Progman"
                        || proc_info.window_class == "PCWatcherAlert"
                        || proc_info.window_class == "PCWatcherDetails"
                        || proc_info.window_class == "PCWatcherTray";

                    if !is_ignored {
                        warn!("!!! FOCUS WITHOUT CLICK: {} - {} !!!",
                            proc_info.process_name, proc_info.process_path);
                        crate::alert_window::set_alert(
                            &format!("{} (no click!)", proc_info.process_name),
                            &proc_info.process_path
                        );
                        // Take screenshots (3 with delay)
                        crate::screenshot::capture_alert_screenshots(proc_info.process_name.clone());
                    }
                }

                // Create log entry
                let log_entry = LogEntry {
                    timestamp: event.timestamp,
                    event_type: event.event_type.as_str().to_string(),
                    process_name: proc_info.process_name,
                    process_id: proc_info.process_id,
                    process_path: proc_info.process_path,
                    window_title: proc_info.window_title,
                    window_class: proc_info.window_class,
                    command_line: proc_info.command_line,
                    parent_process_name: proc_info.parent_process_name,
                    parent_process_id: proc_info.parent_process_id,
                    parent_process_path: proc_info.parent_process_path,
                    grandparent_process_name: proc_info.grandparent_process_name,
                    grandparent_process_id: proc_info.grandparent_process_id,
                    grandparent_process_path: proc_info.grandparent_process_path,
                    greatgrandparent_process_name: proc_info.greatgrandparent_process_name,
                    greatgrandparent_process_id: proc_info.greatgrandparent_process_id,
                    greatgrandparent_process_path: proc_info.greatgrandparent_process_path,
                };

                // Send to logger
                let _ = log_sender.try_send(log_entry);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }

    info!("Event worker ended");
}

/// Sets all Windows event hooks
fn set_hooks() -> Result<Vec<HWINEVENTHOOK>> {
    let mut hooks = Vec::new();
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;

    unsafe {
        // Foreground focus (most important hook!)
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(win_event_proc),
            0,
            0,
            flags,
        );
        if hook.is_invalid() {
            error!("Could not set FOREGROUND hook");
        } else {
            hooks.push(hook);
            debug!("FOREGROUND hook set");
        }

        // Window creation
        let hook = SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_CREATE,
            None,
            Some(win_event_proc),
            0,
            0,
            flags,
        );
        if hook.is_invalid() {
            warn!("Could not set CREATE hook");
        } else {
            hooks.push(hook);
            debug!("CREATE hook set");
        }

        // Window shown
        let hook = SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            None,
            Some(win_event_proc),
            0,
            0,
            flags,
        );
        if hook.is_invalid() {
            warn!("Could not set SHOW hook");
        } else {
            hooks.push(hook);
            debug!("SHOW hook set");
        }

        // Focus within windows
        let hook = SetWinEventHook(
            EVENT_OBJECT_FOCUS,
            EVENT_OBJECT_FOCUS,
            None,
            Some(win_event_proc),
            0,
            0,
            flags,
        );
        if hook.is_invalid() {
            warn!("Could not set FOCUS hook");
        } else {
            hooks.push(hook);
            debug!("FOCUS hook set");
        }

        // Minimize/Restore
        let hook = SetWinEventHook(
            EVENT_SYSTEM_MINIMIZESTART,
            EVENT_SYSTEM_MINIMIZEEND,
            None,
            Some(win_event_proc),
            0,
            0,
            flags,
        );
        if hook.is_invalid() {
            warn!("Could not set MINIMIZE hook");
        } else {
            hooks.push(hook);
            debug!("MINIMIZE hook set");
        }

        // Z-Order changes (Topmost!)
        let hook = SetWinEventHook(
            EVENT_OBJECT_REORDER,
            EVENT_OBJECT_REORDER,
            None,
            Some(win_event_proc),
            0,
            0,
            flags,
        );
        if hook.is_invalid() {
            warn!("Could not set REORDER hook");
        } else {
            hooks.push(hook);
            debug!("REORDER hook set (Z-Order/Topmost)");
        }

        // Low-Level Mouse Hook for click detection
        let mouse_hook = SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(mouse_hook_proc),
            None,
            0,
        );
        match mouse_hook {
            Ok(h) => {
                MOUSE_HOOK_PTR.store(h.0 as usize, Ordering::SeqCst);
                info!("Mouse hook set (click detection)");
            }
            Err(e) => {
                warn!("Could not set mouse hook: {}", e);
            }
        }
    }

    if hooks.is_empty() {
        anyhow::bail!("No hooks could be set!");
    }

    info!("{} event hooks active", hooks.len());
    Ok(hooks)
}

/// Removes all hooks
fn unhook_all(hooks: Vec<HWINEVENTHOOK>) {
    unsafe {
        for hook in hooks {
            let _ = UnhookWinEvent(hook);
        }
        // Remove mouse hook
        let mouse_ptr = MOUSE_HOOK_PTR.load(Ordering::SeqCst);
        if mouse_ptr != 0 {
            let mouse_hook = HHOOK(mouse_ptr as *mut _);
            let _ = UnhookWindowsHookEx(mouse_hook);
        }
    }
    info!("All hooks removed");
}

/// Logs the current foreground window
fn log_current_foreground(sender: &Sender<WindowEvent>) {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 as usize != 0 {
            let event = WindowEvent {
                event_type: EventType::Foreground,
                hwnd: hwnd.0 as isize,
                timestamp: chrono::Local::now(),
            };
            let _ = sender.try_send(event);
        }
    }
}

/// Windows Message Loop
fn message_loop() {
    unsafe {
        // Save thread ID for later shutdown
        let thread_id = GetCurrentThreadId();
        let _ = MESSAGE_THREAD_ID.set(thread_id);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
    debug!("Message loop ended");
}

/// Runs with tray icon (checks periodically for exit)
pub fn run_with_tray_check() -> Result<()> {
    info!("Starting event hooks with tray check...");

    // Create channels
    let (event_tx, event_rx) = bounded::<WindowEvent>(1000);
    let (log_tx, log_rx) = bounded::<LogEntry>(1000);

    // Set event sender globally
    EVENT_SENDER.set(event_tx.clone()).ok();

    // Start logger thread
    let logger_handle = thread::spawn(move || {
        crate::logger::log_worker(log_rx, true);
    });

    // Start event worker
    let worker_handle = thread::spawn(move || {
        event_worker(event_rx, log_tx);
    });

    // Set hooks
    let hooks = set_hooks()?;

    // Log current window
    log_current_foreground(&event_tx);

    // CTRL+C Handler
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_flag_clone = shutdown_flag.clone();

    // ctrlc handler - can fail with windows_subsystem="windows"
    let _ = ctrlc::set_handler(move || {
        info!("CTRL+C received, shutting down...");
        shutdown_flag_clone.store(true, Ordering::Relaxed);
        SHUTDOWN.store(true, Ordering::Relaxed);

        // End message loop
        unsafe {
            if let Some(&thread_id) = MESSAGE_THREAD_ID.get() {
                PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)).ok();
            }
        }
    });

    // Tray exit checker thread
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(200));
            if crate::tray::should_exit() || SHUTDOWN.load(Ordering::Relaxed) {
                info!("Exit signal detected");
                SHUTDOWN.store(true, Ordering::Relaxed);

                // End message loop
                unsafe {
                    if let Some(&thread_id) = MESSAGE_THREAD_ID.get() {
                        PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)).ok();
                    }
                }
                break;
            }
        }
    });

    // Message Loop (blocks)
    message_loop();

    // Cleanup
    SHUTDOWN.store(true, Ordering::Relaxed);
    unhook_all(hooks);

    // Let threads finish
    drop(event_tx);
    let _ = worker_handle.join();
    let _ = logger_handle.join();

    info!("Event hooks ended");
    Ok(())
}
