//! PC Watcher - Window Focus Monitoring with GUI
//!
//! Runs as a normal application with tray icon.
//! For autostart: Use Task Scheduler.

// Only show console in console mode
#![windows_subsystem = "windows"]

mod alert_window;
mod event_hook;
mod logger;
mod notification;
mod process_info;
mod screenshot;
mod tray;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;
use windows::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};

/// PC Watcher - Captures all window focus events
#[derive(Parser)]
#[command(name = "pc_watcher")]
#[command(about = "Window focus monitoring with GUI and tray icon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run with console window (for debugging)
    Console,
    /// Set up Task Scheduler autostart
    Install,
    /// Remove Task Scheduler autostart
    Uninstall,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Console) => {
            // Create own console (don't attach to parent)
            // User can close console with X button
            unsafe {
                let _ = AllocConsole();
            }

            // Initialize console logger
            logger::init_console_logger()?;
            info!("PC Watcher started in console mode");
            info!("Close this window to exit");

            run_app()?;
        }
        Some(Commands::Install) => {
            install_autostart()?;
        }
        Some(Commands::Uninstall) => {
            uninstall_autostart()?;
        }
        None => {
            // Normal start (without console) - for autostart
            logger::init_file_logger()?;
            info!("PC Watcher started");

            run_app()?;
        }
    }

    Ok(())
}

/// Main application logic
fn run_app() -> Result<()> {
    // Delete old screenshots
    screenshot::cleanup_screenshots();

    // Start tray icon
    tray::start_tray();

    // Start alert window
    alert_window::start_alert_window();

    // Start info
    notification::show_start_notification();

    // Start event loop (blocks until CTRL+C or tray exit)
    event_hook::run_with_tray_check()?;

    // Cleanup
    tray::stop_tray();
    alert_window::close_alert_window();
    notification::show_stop_notification();

    info!("PC Watcher ended");
    Ok(())
}

/// Sets up autostart via Task Scheduler
fn install_autostart() -> Result<()> {
    // Console for output
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }

    let exe_path = std::env::current_exe()?;
    let exe_str = exe_path.to_string_lossy();

    println!("Setting up autostart...");

    // Create task with schtasks
    let output = std::process::Command::new("schtasks")
        .args([
            "/Create",
            "/TN", "PCWatcher",
            "/TR", &format!("\"{}\"", exe_str),
            "/SC", "ONLOGON",
            "/RL", "HIGHEST",
            "/F",
        ])
        .output()?;

    if output.status.success() {
        println!("Autostart configured!");
        println!("PC Watcher will start automatically at logon.");
        println!();
        println!("Starting PC Watcher now...");

        // Start program directly (no arguments = normal mode)
        let _ = std::process::Command::new(&exe_path)
            .spawn();

        println!("PC Watcher is running! (Check tray icon)");
        println!();
        println!("To remove: pc_watcher uninstall");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("Error setting up: {}", stderr);
        println!();
        println!("Tip: Run as administrator!");
    }

    Ok(())
}

/// Removes autostart
fn uninstall_autostart() -> Result<()> {
    // Console for output
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }

    println!("Removing autostart...");

    let output = std::process::Command::new("schtasks")
        .args(["/Delete", "/TN", "PCWatcher", "/F"])
        .output()?;

    if output.status.success() {
        println!("Autostart removed!");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("existiert nicht") || stderr.contains("does not exist") {
            println!("No autostart task found.");
        } else {
            println!("Error: {}", stderr);
        }
    }

    Ok(())
}
