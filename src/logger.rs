//! Logging System
//!
//! Multi-threaded log writer with channel-based queue.

use anyhow::Result;
use chrono::{DateTime, Local};
use crossbeam_channel::Receiver;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use tracing::{info, error};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Log directory (in project folder next to EXE)
fn get_log_dir() -> PathBuf {
    // Try to determine EXE directory
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            return exe_dir.join("logs");
        }
    }
    // Fallback: current working directory
    PathBuf::from(".").join("logs")
}

/// Initializes the console logger
pub fn init_console_logger() -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).compact())
        .with(filter)
        .init();

    Ok(())
}

/// Initializes the file logger (app.log for debug messages)
pub fn init_file_logger() -> Result<()> {
    let log_dir = get_log_dir();
    fs::create_dir_all(&log_dir)?;

    // Clean up old app.log files (keep only 2)
    cleanup_old_logs(&log_dir, 2, "app.log");

    let file_appender = tracing_appender::rolling::daily(&log_dir, "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Guard must stay alive - we intentionally leak it for app lifetime
    Box::leak(Box::new(_guard));

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(non_blocking).with_target(false))
        .with(filter)
        .init();

    Ok(())
}

/// Log entry structure
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Local>,
    pub event_type: String,
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
    // Grandparent process (level 2)
    pub grandparent_process_name: String,
    pub grandparent_process_id: u32,
    pub grandparent_process_path: String,
    // Great-grandparent process (level 3)
    pub greatgrandparent_process_name: String,
    pub greatgrandparent_process_id: u32,
    pub greatgrandparent_process_path: String,
}

impl LogEntry {
    /// Formats the entry for file output
    pub fn format_file(&self) -> String {
        let mut output = String::with_capacity(512);

        output.push_str(&format!(
            "────────────────────────────────────────────────────────────────────────────────\n"
        ));
        output.push_str(&format!(
            "[{}] ══ {} ══\n",
            self.timestamp.format("%Y-%m-%d %H:%M:%S%.3f"),
            self.event_type
        ));
        output.push_str(&format!(
            "  Process:     {} (PID: {})\n",
            self.process_name, self.process_id
        ));
        output.push_str(&format!("  Path:        {}\n", self.process_path));
        output.push_str(&format!(
            "  Title:       {}\n",
            if self.window_title.is_empty() {
                "(no title)"
            } else {
                &self.window_title
            }
        ));
        output.push_str(&format!("  Class:       {}\n", self.window_class));

        if let Some(ref cmd) = self.command_line {
            if !cmd.is_empty() {
                output.push_str(&format!("  Command:     {}\n", cmd));
            }
        }

        // Show process hierarchy (THE CULPRIT!)
        if self.parent_process_id > 0 {
            output.push_str("  ── PROCESS HIERARCHY ──\n");

            // Parent (level 1)
            output.push_str(&format!(
                "  ├─ Parent:           {} (PID: {})\n",
                self.parent_process_name, self.parent_process_id
            ));
            if !self.parent_process_path.is_empty() && self.parent_process_path != "Access denied" {
                output.push_str(&format!("  │  Path:             {}\n", self.parent_process_path));
            }

            // Grandparent (level 2)
            if self.grandparent_process_id > 0 && !self.grandparent_process_name.is_empty() {
                output.push_str(&format!(
                    "  ├─ Grandparent:      {} (PID: {})\n",
                    self.grandparent_process_name, self.grandparent_process_id
                ));
                if !self.grandparent_process_path.is_empty() && self.grandparent_process_path != "Access denied" {
                    output.push_str(&format!("  │  Path:             {}\n", self.grandparent_process_path));
                }
            }

            // Great-grandparent (level 3)
            if self.greatgrandparent_process_id > 0 && !self.greatgrandparent_process_name.is_empty() {
                output.push_str(&format!(
                    "  └─ Great-Grandparent: {} (PID: {})\n",
                    self.greatgrandparent_process_name, self.greatgrandparent_process_id
                ));
                if !self.greatgrandparent_process_path.is_empty() && self.greatgrandparent_process_path != "Access denied" {
                    output.push_str(&format!("     Path:             {}\n", self.greatgrandparent_process_path));
                }
            }
        }

        output
    }

    /// Formats the entry for console (compact, with paths)
    pub fn format_console(&self) -> String {
        let title = if self.window_title.len() > 40 {
            format!("{}...", &self.window_title[..37])
        } else {
            self.window_title.clone()
        };

        // Add parent info with path
        let parent_info = if self.parent_process_id > 0 && !self.parent_process_name.is_empty() {
            if !self.parent_process_path.is_empty() && self.parent_process_path != "Access denied" {
                format!(" [from: {} ({})]", self.parent_process_name, self.parent_process_path)
            } else {
                format!(" [from: {}]", self.parent_process_name)
            }
        } else {
            String::new()
        };

        format!(
            "[{}] {:<12} {:<20} {}\n                     Path: {}{}",
            self.timestamp.format("%H:%M:%S%.3f"),
            self.event_type,
            self.process_name,
            title,
            self.process_path,
            parent_info
        )
    }

    /// Formats the entry for GUI (with event type)
    pub fn format_gui(&self) -> String {
        // Shorten event type
        let event = match self.event_type.as_str() {
            "FOCUS" => "FOC",
            "CREATED" => "NEW",
            "SHOWN" => "SHW",
            "MINIMIZED" => "MIN",
            "RESTORED" => "RST",
            "Z-ORDER" => "Z-O",
            _ => &self.event_type[..3.min(self.event_type.len())],
        };

        // Shorten process name if needed
        let name = if self.process_name.len() > 20 {
            format!("{}...", &self.process_name[..17])
        } else {
            self.process_name.clone()
        };

        // Only show parent if it exists, is not empty, AND is different from the process itself
        let parent = if !self.parent_process_name.is_empty()
            && self.parent_process_name != "Unknown"
            && self.parent_process_name.to_lowercase() != self.process_name.to_lowercase()
        {
            let parent_short = if self.parent_process_name.len() > 15 {
                format!("{}...", &self.parent_process_name[..12])
            } else {
                self.parent_process_name.clone()
            };
            format!(" (from {})", parent_short)
        } else {
            String::new()
        };

        // Shorten title for GUI
        let title = if !self.window_title.is_empty() {
            let t = if self.window_title.len() > 25 {
                format!("{}...", &self.window_title[..22])
            } else {
                self.window_title.clone()
            };
            format!(": {}", t)
        } else {
            String::new()
        };

        format!(
            "{} [{:3}] {}{}{}",
            self.timestamp.format("%H:%M:%S"),
            event,
            name,
            title,
            parent
        )
    }
}

/// Deletes old log files with specific prefix, keeps only the newest N
fn cleanup_old_logs(log_dir: &PathBuf, keep_count: usize, prefix: &str) {
    if let Ok(entries) = fs::read_dir(log_dir) {
        let mut log_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                // Only files with the correct prefix
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    name.starts_with(prefix)
                } else {
                    false
                }
            })
            .collect();

        // Sort by modification time (newest first)
        log_files.sort_by(|a, b| {
            let time_a = a.metadata().and_then(|m| m.modified()).ok();
            let time_b = b.metadata().and_then(|m| m.modified()).ok();
            time_b.cmp(&time_a)
        });

        // Delete old files (all except the newest keep_count)
        for old_file in log_files.iter().skip(keep_count) {
            if let Err(e) = fs::remove_file(old_file.path()) {
                error!("Could not delete old log file: {}", e);
            } else {
                info!("Old log file deleted: {}", old_file.path().display());
            }
        }
    }
}

/// Log worker thread
pub fn log_worker(receiver: Receiver<LogEntry>, console_output: bool) {
    info!("Log worker started");

    // Create log directory
    let log_dir = get_log_dir();
    if let Err(e) = fs::create_dir_all(&log_dir) {
        error!("Could not create log directory: {}", e);
        return;
    }

    // Clean up old event logs (keep only 2)
    cleanup_old_logs(&log_dir, 2, "event_");

    // Open log file
    let log_file_path = log_dir.join(format!(
        "event_{}.log",
        Local::now().format("%Y-%m-%d_%H-%M-%S")
    ));

    let file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
    {
        Ok(f) => f,
        Err(e) => {
            error!("Could not open log file: {}", e);
            return;
        }
    };

    let mut writer = BufWriter::new(file);

    // Write header
    let header = format!(
        "════════════════════════════════════════════════════════════════════════════════\n\
         PC Watcher Log started: {}\n\
         Computer: {}\n\
         User: {}\n\
         ════════════════════════════════════════════════════════════════════════════════\n\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        hostname::get().map(|h| h.to_string_lossy().to_string()).unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );

    if let Err(e) = writer.write_all(header.as_bytes()) {
        error!("Error writing header: {}", e);
    }
    let _ = writer.flush();

    // Send log file path to GUI
    crate::alert_window::set_log_file_path(log_file_path.clone());

    if console_output {
        println!("\n{}", "═".repeat(80));
        println!("Log file: {}", log_file_path.display());
        println!("{}\n", "═".repeat(80));
    }

    info!("Log file: {}", log_file_path.display());

    // Receive and write entries
    let mut entry_count = 0u64;
    let flush_interval = 10; // Flush every 10 entries

    while let Ok(entry) = receiver.recv() {
        // Write to file
        let formatted = entry.format_file();
        if let Err(e) = writer.write_all(formatted.as_bytes()) {
            error!("Error writing: {}", e);
        }

        // Update GUI (compact line with event type for color and details for double-click)
        let gui_line = entry.format_gui();
        let details = entry.format_file(); // Full details for double-click
        crate::alert_window::add_log_entry(gui_line, entry.event_type.clone(), details, entry.process_path.clone());

        // Console output
        if console_output {
            // Colored output based on event type
            let console_line = entry.format_console();

            match entry.event_type.as_str() {
                "FOCUS" => println!("\x1b[93m{}\x1b[0m", console_line), // Yellow
                "CREATED" => println!("\x1b[96m{}\x1b[0m", console_line), // Cyan
                "SHOWN" => println!("\x1b[92m{}\x1b[0m", console_line), // Green
                "MINIMIZED" => println!("\x1b[90m{}\x1b[0m", console_line), // Gray
                "RESTORED" => println!("\x1b[95m{}\x1b[0m", console_line), // Magenta
                "Z-ORDER" => println!("\x1b[91m{}\x1b[0m", console_line), // Red - Topmost!
                _ => println!("{}", console_line),
            }
        }

        entry_count += 1;

        // Periodically flush
        if entry_count % flush_interval == 0 {
            let _ = writer.flush();
        }
    }

    // Final flush and footer
    let footer = format!(
        "\n════════════════════════════════════════════════════════════════════════════════\n\
         PC Watcher Log ended: {}\n\
         Total entries: {}\n\
         ════════════════════════════════════════════════════════════════════════════════\n",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        entry_count
    );

    let _ = writer.write_all(footer.as_bytes());
    let _ = writer.flush();

    info!("Log worker ended ({} entries)", entry_count);
}
