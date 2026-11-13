//! RSyncStat: dual-root treemap comparison and sync.

mod app;
mod files;
mod keyboard;
mod platform;
mod presets;
mod ui;
mod volume;

fn main() -> eframe::Result<()> {
    app::run()
}
