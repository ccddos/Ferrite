// Hide console window on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Ferrite - Main Entry Point
//!
//! A fast, lightweight text editor for Markdown, JSON, and more. Built with Rust and egui.

// ============================================================================
// Global Memory Allocator Configuration
// ============================================================================
// Use high-performance allocators to reduce heap fragmentation and memory usage.
// - Windows: mimalloc - Microsoft's compact, fast allocator
// - Unix (Linux/macOS): jemalloc - battle-tested allocator from Facebook/Meta
//
// These allocators are enabled by the "high-perf-alloc" feature (default on).
// Disable with: cargo build --no-default-features --features bundle-icon
// ============================================================================

#[cfg(all(feature = "high-perf-alloc", target_os = "windows"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(all(feature = "high-perf-alloc", unix))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Initialize internationalization system
// The locales directory contains YAML files for each supported language (e.g., en.yaml, zh.yaml)
// Usage: rust_i18n::t!("menu.file.open") returns the translated string
rust_i18n::i18n!("locales", fallback = "en");

mod app;
mod config;
mod editor;
mod error;
mod export;
mod files;
mod fonts;
mod lsp;
mod markdown;
mod path_utils;
mod platform;
mod preview;
mod single_instance;
mod state;
mod string_utils;
mod terminal;
mod theme;
mod ui;
mod update;
mod vcs;
#[cfg(feature = "async-workers")]
mod workers;
mod workspaces;

use app::FerriteApp;
use clap::Parser;
use config::{ load_config, LogLevel, WindowSize };
use log::{info, warn};
use rust_i18n::{ set_locale, t };
use std::path::PathBuf;
use ui::get_app_icon;

/// Get current process memory usage in MB (for diagnostics).
/// Returns (working_set_mb, private_bytes_mb) on Windows.
#[cfg(target_os = "windows")]
pub fn get_memory_usage_mb() -> (f64, f64) {
    use std::mem::MaybeUninit;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            pmc: *mut ProcessMemoryCounters,
            cb: u32
        ) -> i32;
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }

    unsafe {
        let mut pmc = MaybeUninit::<ProcessMemoryCounters>::uninit();
        (*pmc.as_mut_ptr()).cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;

        if
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                pmc.as_mut_ptr(),
                std::mem::size_of::<ProcessMemoryCounters>() as u32
            ) != 0
        {
            let pmc = pmc.assume_init();
            let working_set_mb = (pmc.working_set_size as f64) / (1024.0 * 1024.0);
            let private_mb = (pmc.pagefile_usage as f64) / (1024.0 * 1024.0);
            (working_set_mb, private_mb)
        } else {
            (0.0, 0.0)
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn get_memory_usage_mb() -> (f64, f64) {
    // On non-Windows, return 0 (could implement /proc/self/status parsing for Linux)
    (0.0, 0.0)
}

#[derive(Debug, Clone, Copy)]
struct DisplayBounds {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn rects_intersect(a: DisplayBounds, b: DisplayBounds) -> bool {
    a.x < b.x + b.width
        && a.x + a.width > b.x
        && a.y < b.y + b.height
        && a.y + a.height > b.y
}

#[cfg(target_os = "macos")]
fn active_display_bounds() -> Vec<DisplayBounds> {
    type CGDirectDisplayID = u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut CGDirectDisplayID,
            display_count: *mut u32,
        ) -> i32;
        fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    }

    const MAX_DISPLAYS: usize = 16;

    let mut displays = [0_u32; MAX_DISPLAYS];
    let mut display_count = 0_u32;

    let err = unsafe {
        CGGetActiveDisplayList(
            MAX_DISPLAYS as u32,
            displays.as_mut_ptr(),
            &mut display_count,
        )
    };

    if err != 0 {
        return Vec::new();
    }

    displays[..display_count as usize]
        .iter()
        .map(|display_id| unsafe { CGDisplayBounds(*display_id) })
        .map(|bounds| DisplayBounds {
            x: bounds.origin.x as f32,
            y: bounds.origin.y as f32,
            width: bounds.size.width as f32,
            height: bounds.size.height as f32,
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn active_display_bounds() -> Vec<DisplayBounds> {
    Vec::new()
}

fn sanitize_saved_window_position_for_current_displays(window_size: &mut WindowSize) {
    let (Some(x), Some(y)) = (window_size.x, window_size.y) else {
        return;
    };

    let displays = active_display_bounds();
    if displays.is_empty() {
        return;
    }

    let saved_rect = DisplayBounds {
        x,
        y,
        width: window_size.width.max(1.0),
        height: window_size.height.max(1.0),
    };

    let is_visible = displays
        .iter()
        .copied()
        .any(|display| rects_intersect(saved_rect, display));

    if !is_visible {
        warn!(
            "Saved window position ({}, {}) with size {}x{} is outside active displays; resetting position",
            x,
            y,
            window_size.width,
            window_size.height
        );
        window_size.x = None;
        window_size.y = None;
    }
}

/// Log current memory usage with a label.
pub fn log_memory(label: &str) {
    let (working_set, private) = get_memory_usage_mb();
    info!("[MEM] {}: {:.1} MB (working set), {:.1} MB (private)", label, working_set, private);
}

/// Ferrite - A fast, lightweight text editor for Markdown, JSON, and more.
#[derive(Parser, Debug)]
#[command(name = "ferrite", version, about, long_about = None)]
struct Cli {
    /// Files or directory to open on startup.
    ///
    /// Pass one or more file paths to open them as tabs.
    /// Pass a directory path to open it as a workspace.
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Set the log level for debugging.
    ///
    /// Overrides the log_level setting in config.json.
    /// Valid values: debug, info, warn, error, off
    #[arg(long, value_name = "LEVEL", value_parser = parse_log_level)]
    log_level: Option<LogLevel>,
}

/// Parse a log level string into a LogLevel enum.
fn parse_log_level(s: &str) -> Result<LogLevel, String> {
    match s.to_lowercase().as_str() {
        "debug" => Ok(LogLevel::Debug),
        "info" => Ok(LogLevel::Info),
        "warn" | "warning" => Ok(LogLevel::Warn),
        "error" => Ok(LogLevel::Error),
        "off" | "none" => Ok(LogLevel::Off),
        _ => Err(format!("Invalid log level '{}'. Valid values: debug, info, warn, error, off", s)),
    }
}

// Note: Native window decorations are disabled for custom title bar styling.
// This provides consistent appearance across all platforms (Windows, macOS, Linux).

/// Application name constant.
const APP_NAME: &str = "Ferrite";

fn main() -> eframe::Result<()> {
    // Initialize macOS app delegate FIRST, before anything else
    // This must happen very early to catch Apple Events for "Open With" functionality
    #[cfg(target_os = "macos")]
    platform::macos::init_app_delegate();

    // Parse CLI arguments first (before logging, so --help/--version work without config)
    let cli = Cli::parse();

    // Combine CLI paths with any paths received via macOS Apple Events ("Open With")
    let mut initial_paths = cli.paths;
    let apple_event_paths = platform::get_open_file_paths();
    if !apple_event_paths.is_empty() {
        initial_paths.extend(apple_event_paths);
    }

    // Single-instance check EARLY — before heavy initialization (config, icons, logging).
    // When the user double-clicks a file while Ferrite is already running, the secondary
    // process should forward paths and exit as fast as possible (<100ms).
    let instance_listener = match single_instance::try_acquire_instance(&initial_paths) {
        Some(listener) => listener,
        None => {
            // Paths were forwarded to the existing instance — exit cleanly.
            // No logging here since logger isn't initialized yet.
            return Ok(());
        }
    };

    // Set up Ctrl+C handler to prevent the app from closing when running from console.
    // We handle Ctrl+C internally in the integrated terminal.
    let _ = ctrlc::set_handler(|| {
        log::debug!("Ctrl+C received in console, ignoring to prevent app exit");
    });

    // Load settings to get configuration (including log level and language)
    let mut settings = load_config();

    // Apply saved language setting for i18n
    set_locale(settings.language.locale_code());

    // Determine effective log level: CLI > config > default (Warn)
    let effective_log_level = cli.log_level.unwrap_or(settings.log_level);

    // Initialize logging with the effective log level
    env_logger::Builder::new().filter_level(effective_log_level.to_level_filter()).init();

    info!("Starting {}", APP_NAME);
    log_memory("After logging init");
    sanitize_saved_window_position_for_current_displays(&mut settings.window_size);
    info!("Language: {} ({})", settings.language.native_name(), settings.language.locale_code());
    info!("i18n initialized: {}", t!("app.name"));
    info!("Log level: {} (source: {})", effective_log_level.display_name(), if
        cli.log_level.is_some()
    {
        "CLI flag"
    } else {
        "config"
    });

    if !initial_paths.is_empty() {
        info!("CLI paths provided: {:?}", initial_paths);
    }
    let window_size = &settings.window_size;

    info!(
        "Window configuration: {}x{}, maximized: {}",
        window_size.width,
        window_size.height,
        window_size.maximized
    );

    // Load application icon
    let app_icon = get_app_icon();
    if app_icon.is_some() {
        info!("Application icon loaded successfully");
    }

    // Configure the native window options with custom title bar (no native decorations)
    let mut viewport = eframe::egui::ViewportBuilder
        ::default()
        .with_title(APP_NAME)
        .with_app_id("ferrite")
        .with_decorations(false)
        .with_inner_size([window_size.width, window_size.height])
        .with_min_inner_size([400.0, 300.0]);

    // Set application icon if available
    if let Some(icon) = app_icon {
        viewport = viewport.with_icon(icon);
    }

    // Apply position if saved
    let viewport = if let (Some(x), Some(y)) = (window_size.x, window_size.y) {
        viewport.with_position([x, y])
    } else {
        viewport
    };

    // Apply maximized state
    let viewport = if window_size.maximized { viewport.with_maximized(true) } else { viewport };

    let native_options = eframe::NativeOptions {
        viewport,
        vsync: true,
        run_and_return: true,
        ..Default::default()
    };

    log_memory("Before eframe::run_native");

    // Run the application
    eframe::run_native(
        APP_NAME,
        native_options,
        Box::new(move |cc| {
            // Configure egui visuals based on theme (basic setup)
            // Full theme support will be implemented in a later task
            let mut app = FerriteApp::new(cc);

            // Store the single-instance listener for polling in the update loop
            app.set_instance_listener(instance_listener);

            // Open files/directories from CLI arguments and Apple Events
            let has_initial_paths = !initial_paths.is_empty();
            app.open_initial_paths(initial_paths);

            // Only show welcome screen when no files were passed via CLI
            if !has_initial_paths {
                app.open_welcome_on_startup();
            }

            log_memory("After app creation and initial paths");

            Ok(Box::new(app))
        })
    )
}
