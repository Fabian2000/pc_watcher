//! Notifications and Warnings
//!
//! Detects suspicious processes.

use tracing::info;

// List of suspicious processes
const SUSPICIOUS_PROCESSES: &[&str] = &[
    "powershell",
    "pwsh",
    "cmd",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
];

/// Checks if a process name is suspicious
pub fn is_suspicious_process(process_name: &str) -> bool {
    let name_lower = process_name.to_lowercase();
    SUSPICIOUS_PROCESSES.iter().any(|&p| name_lower.contains(p))
}

/// Shows start info (log only)
pub fn show_start_notification() {
    info!("=== PC Watcher started ===");
    info!("Monitoring window focus events...");
    info!("Alert on: {:?}", SUSPICIOUS_PROCESSES);
}

/// Shows stop info (log only)
pub fn show_stop_notification() {
    info!("=== PC Watcher ended ===");
}
