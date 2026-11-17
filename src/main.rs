#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod diff;
mod git;
mod graph;
mod icon;
mod prefs;
mod repo;
mod theme;
mod widgets;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use eframe::egui;

/// `spoon --export-logo <dir>` writes the logo files generated from src/icon.rs.
fn export_logo(dir: &str) -> std::io::Result<()> {
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("spoon.svg"), icon::svg())?;
    std::fs::write(dir.join("spoon.ico"), icon::ico(&[16, 24, 32, 48, 64, 256]))
}

fn main() -> eframe::Result {
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

    let size = 128;
    let window_icon = egui::IconData { rgba: icon::render(size, icon::BRAND_BG, icon::BRAND_FG), width: size as u32, height: size as u32 };
    let viewport = egui::ViewportBuilder::default()
        .with_title("Spoon")
        .with_app_id("spoon")
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([980.0, 620.0])
        .with_icon(window_icon);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let open = args.first().map(PathBuf::from);
    eframe::run_native("Spoon", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}
