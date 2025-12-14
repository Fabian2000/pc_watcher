//! Process Information
//!
//! Reads process name, path, window title, command line and PARENT process.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use windows::Win32::Foundation::{HANDLE, HWND, CloseHandle, MAX_PATH};
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowTextW, GetWindowTextLengthW, GetClassNameW,
    GetWindowThreadProcessId,
};

/// Process information
#[derive(Debug, Default)]
pub struct ProcessInfo {
    pub process_name: String,
    pub process_id: u32,
    pub process_path: String,
    pub window_title: String,
    pub window_class: String,
    pub command_line: Option<String>,
    // Parent process (who started this process?)
    pub parent_process_name: String,
    pub parent_process_id: u32,
    pub parent_process_path: String,
    // Grandparent process (who started the parent?)
    pub grandparent_process_name: String,
    pub grandparent_process_id: u32,
    pub grandparent_process_path: String,
    // Great-grandparent process (level 3)
    pub greatgrandparent_process_name: String,
    pub greatgrandparent_process_id: u32,
    pub greatgrandparent_process_path: String,
}

/// Reads all process information for a window
pub fn get_process_info(hwnd: HWND) -> ProcessInfo {
    let mut info = ProcessInfo::default();

    // Get process ID
    let mut process_id: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    info.process_id = process_id;

    if process_id == 0 {
        info.process_name = "Unknown".to_string();
        info.process_path = "Unknown".to_string();
        return info;
    }

    // Window title
    info.window_title = get_window_title(hwnd);

    // Window class
    info.window_class = get_window_class(hwnd);

    // Open process handle
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            process_id,
        );

        match handle {
            Ok(h) if !h.is_invalid() => {
                // Process path
                info.process_path = get_process_path(h);

                // Extract process name from path
                info.process_name = Path::new(&info.process_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Unknown")
                    .to_string();

                // Try to read command line
                info.command_line = get_command_line(process_id);

                // Get parent process (level 1)
                let (parent_name, parent_id, parent_path) = get_parent_process_info(process_id);
                info.parent_process_name = parent_name;
                info.parent_process_id = parent_id;
                info.parent_process_path = parent_path;

                // Get grandparent process (level 2)
                if parent_id > 0 {
                    let (gp_name, gp_id, gp_path) = get_parent_process_info(parent_id);
                    info.grandparent_process_name = gp_name;
                    info.grandparent_process_id = gp_id;
                    info.grandparent_process_path = gp_path;

                    // Get great-grandparent process (level 3)
                    if gp_id > 0 {
                        let (ggp_name, ggp_id, ggp_path) = get_parent_process_info(gp_id);
                        info.greatgrandparent_process_name = ggp_name;
                        info.greatgrandparent_process_id = ggp_id;
                        info.greatgrandparent_process_path = ggp_path;
                    }
                }

                let _ = CloseHandle(h);
            }
            _ => {
                info.process_name = "Access denied".to_string();
                info.process_path = "Access denied (elevated privileges required)".to_string();

                // Fallback: Try with fewer privileges
                let handle = OpenProcess(PROCESS_QUERY_INFORMATION, false, process_id);
                if let Ok(h) = handle {
                    if !h.is_invalid() {
                        let path = get_process_path(h);
                        if !path.is_empty() {
                            info.process_path = path;
                            info.process_name = Path::new(&info.process_path)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("Unknown")
                                .to_string();
                        }
                        let _ = CloseHandle(h);
                    }
                }

                // Try parent process even with access problems (level 1)
                let (parent_name, parent_id, parent_path) = get_parent_process_info(process_id);
                info.parent_process_name = parent_name;
                info.parent_process_id = parent_id;
                info.parent_process_path = parent_path;

                // Grandparent process (level 2)
                if parent_id > 0 {
                    let (gp_name, gp_id, gp_path) = get_parent_process_info(parent_id);
                    info.grandparent_process_name = gp_name;
                    info.grandparent_process_id = gp_id;
                    info.grandparent_process_path = gp_path;

                    // Great-grandparent process (level 3)
                    if gp_id > 0 {
                        let (ggp_name, ggp_id, ggp_path) = get_parent_process_info(gp_id);
                        info.greatgrandparent_process_name = ggp_name;
                        info.greatgrandparent_process_id = ggp_id;
                        info.greatgrandparent_process_path = ggp_path;
                    }
                }
            }
        }
    }

    info
}

/// Reads the window title
fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return String::new();
        }

        let mut buffer: Vec<u16> = vec![0; (len + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buffer);

        if copied > 0 {
            OsString::from_wide(&buffer[..copied as usize])
                .to_string_lossy()
                .to_string()
        } else {
            String::new()
        }
    }
}

/// Reads the window class
fn get_window_class(hwnd: HWND) -> String {
    unsafe {
        let mut buffer: Vec<u16> = vec![0; 256];
        let len = GetClassNameW(hwnd, &mut buffer);

        if len > 0 {
            OsString::from_wide(&buffer[..len as usize])
                .to_string_lossy()
                .to_string()
        } else {
            String::new()
        }
    }
}

/// Reads the process path
fn get_process_path(handle: HANDLE) -> String {
    unsafe {
        let mut buffer: Vec<u16> = vec![0; MAX_PATH as usize];
        let mut size = buffer.len() as u32;

        // First try QueryFullProcessImageNameW (better for modern processes)
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );

        if result.is_ok() && size > 0 {
            return OsString::from_wide(&buffer[..size as usize])
                .to_string_lossy()
                .to_string();
        }

        // Fallback: GetModuleFileNameExW
        let len = GetModuleFileNameExW(handle, None, &mut buffer);
        if len > 0 {
            OsString::from_wide(&buffer[..len as usize])
                .to_string_lossy()
                .to_string()
        } else {
            String::new()
        }
    }
}

/// Tries to read the command line (via WMI)
fn get_command_line(_process_id: u32) -> Option<String> {
    // WMI query is expensive, only for important processes
    // We could use WMI here, but it's complex in Rust
    // For now we skip it, as the path is usually sufficient

    // Alternative: NtQueryInformationProcess + ReadProcessMemory
    // That's very low-level and requires undocumented APIs

    None
}

/// Gets the parent process ID via Toolhelp Snapshot
fn get_parent_process_id(process_id: u32) -> Option<u32> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if let Ok(handle) = snapshot {
            if handle.is_invalid() {
                return None;
            }

            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };

            if Process32FirstW(handle, &mut entry).is_ok() {
                loop {
                    if entry.th32ProcessID == process_id {
                        let parent_id = entry.th32ParentProcessID;
                        let _ = CloseHandle(handle);
                        return Some(parent_id);
                    }
                    if Process32NextW(handle, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(handle);
        }
    }
    None
}

/// Gets parent process information (name and path)
fn get_parent_process_info(process_id: u32) -> (String, u32, String) {
    if let Some(parent_id) = get_parent_process_id(process_id) {
        if parent_id == 0 {
            return ("System".to_string(), 0, "".to_string());
        }

        unsafe {
            // Try to open process handle
            let handle = OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
                false,
                parent_id,
            );

            match handle {
                Ok(h) if !h.is_invalid() => {
                    let path = get_process_path(h);
                    let name = Path::new(&path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Unknown")
                        .to_string();
                    let _ = CloseHandle(h);
                    return (name, parent_id, path);
                }
                _ => {
                    // Fallback: Name from Toolhelp Snapshot
                    if let Some(name) = get_process_name_from_snapshot(parent_id) {
                        return (name, parent_id, "Access denied".to_string());
                    }
                }
            }
        }
        return ("Access denied".to_string(), parent_id, "".to_string());
    }
    ("Unknown".to_string(), 0, "".to_string())
}

/// Gets process name from Toolhelp Snapshot (fallback)
fn get_process_name_from_snapshot(process_id: u32) -> Option<String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if let Ok(handle) = snapshot {
            if handle.is_invalid() {
                return None;
            }

            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };

            if Process32FirstW(handle, &mut entry).is_ok() {
                loop {
                    if entry.th32ProcessID == process_id {
                        // szExeFile is [u16; 260]
                        let name_len = entry.szExeFile.iter()
                            .position(|&c| c == 0)
                            .unwrap_or(entry.szExeFile.len());
                        let name = OsString::from_wide(&entry.szExeFile[..name_len])
                            .to_string_lossy()
                            .to_string();
                        let _ = CloseHandle(handle);
                        // Remove .exe
                        return Some(Path::new(&name)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(&name)
                            .to_string());
                    }
                    if Process32NextW(handle, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(handle);
        }
    }
    None
}

/// Cache for frequently queried processes
use parking_lot::RwLock;
use std::collections::HashMap;
use std::time::{Duration, Instant};

lazy_static::lazy_static! {
    static ref PROCESS_CACHE: RwLock<HashMap<u32, (ProcessInfo, Instant)>> =
        RwLock::new(HashMap::new());
}

const CACHE_TTL: Duration = Duration::from_secs(5);

/// Reads process info with caching
pub fn get_process_info_cached(hwnd: HWND) -> ProcessInfo {
    let mut process_id: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }

    if process_id == 0 {
        return get_process_info(hwnd);
    }

    // Check cache
    {
        let cache = PROCESS_CACHE.read();
        if let Some((info, timestamp)) = cache.get(&process_id) {
            if timestamp.elapsed() < CACHE_TTL {
                // Window title can change, so read anew
                let mut cached = info.clone();
                cached.window_title = get_window_title(hwnd);
                cached.window_class = get_window_class(hwnd);
                return cached;
            }
        }
    }

    // Query anew
    let info = get_process_info(hwnd);

    // Save to cache
    {
        let mut cache = PROCESS_CACHE.write();
        cache.insert(process_id, (info.clone(), Instant::now()));

        // Clean up cache if too large
        if cache.len() > 100 {
            cache.retain(|_, (_, ts)| ts.elapsed() < CACHE_TTL);
        }
    }

    info
}

impl Clone for ProcessInfo {
    fn clone(&self) -> Self {
        ProcessInfo {
            process_name: self.process_name.clone(),
            process_id: self.process_id,
            process_path: self.process_path.clone(),
            window_title: self.window_title.clone(),
            window_class: self.window_class.clone(),
            command_line: self.command_line.clone(),
            parent_process_name: self.parent_process_name.clone(),
            parent_process_id: self.parent_process_id,
            parent_process_path: self.parent_process_path.clone(),
            grandparent_process_name: self.grandparent_process_name.clone(),
            grandparent_process_id: self.grandparent_process_id,
            grandparent_process_path: self.grandparent_process_path.clone(),
            greatgrandparent_process_name: self.greatgrandparent_process_name.clone(),
            greatgrandparent_process_id: self.greatgrandparent_process_id,
            greatgrandparent_process_path: self.greatgrandparent_process_path.clone(),
        }
    }
}
