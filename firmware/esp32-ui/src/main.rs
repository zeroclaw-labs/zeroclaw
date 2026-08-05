//! ZeroClaw ESP32 UI firmware scaffold.

use anyhow::Context;
use log::info;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Starting ZeroClaw ESP32 UI scaffold");

    let window = MainWindow::new().context("failed to create MainWindow")?;
    window.run().context("MainWindow event loop failed")?;

    Ok(())
}
