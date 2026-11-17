#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod conflict;
mod dialogs;
mod diff;
#[cfg(all(windows, target_pointer_width = "64"))]
mod dpi_drag;
mod git;
mod graph;
mod icon;
mod merge_editor;
mod open_in;
mod prefs;
mod repo;
mod theme;
mod widgets;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use eframe::egui;

/// `spoon --export-logo <dir>` writes the logo files generated from src/icon.rs.
fn export_logo(dir: &str) -> std::io::Result<()> {
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("spoon.svg"), icon::svg())?;
    std::fs::write(dir.join("spoon.ico"), icon::ico(&[16, 24, 32, 48, 64, 256]))
}

/// The icon every Spoon window carries, rendered once.
fn window_icon() -> Arc<egui::IconData> {
    static ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();
    let size = 128;
    ICON.get_or_init(|| Arc::new(egui::IconData { rgba: icon::render(size, icon::BRAND_BG, icon::BRAND_FG), width: size as u32, height: size as u32 })).clone()
}

/// Release builds have no console, so a panic would vanish. Append it, with a
/// backtrace, to `crash.log` in the settings folder before the default handling.
fn install_crash_log() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let entry = format!("--- spoon {} at unix time {when}\n{info}\n{}\n", env!("CARGO_PKG_VERSION"), std::backtrace::Backtrace::force_capture());
        // Failing to write the log must not mask the panic itself.
        let log = prefs::config_dir().and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log")).ok()
        });
        if let Some(mut file) = log {
            let _ = file.write_all(entry.as_bytes());
        }
        default(info);
    }));
}

fn main() -> eframe::Result {
    install_crash_log();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, dir] = args.as_slice()
        && flag == "--export-logo"
    {
        if let Err(e) = export_logo(dir) {
            eprintln!("spoon: cannot export logo to {dir}: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let viewport = egui::ViewportBuilder::default()
        .with_title("Spoon")
        .with_app_id("spoon")
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([980.0, 620.0])
        .with_icon(window_icon());
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let open = args.first().map(PathBuf::from);
    eframe::run_native("Spoon", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}
