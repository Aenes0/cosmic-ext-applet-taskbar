// SPDX-License-Identifier: GPL-3.0-only

mod anim;
mod app;
mod config;
mod fx;
mod launcher_entry;
mod texts;
mod wayland_handler;
mod wayland_subscription;

pub const APP_ID: &str = "io.github.Aenes0.CosmicTaskbar";

fn main() -> cosmic::iced::Result {
    // The panel may run us with RUST_LOG=debug; the bar and libcosmic then
    // log on every frame of an animation, which is megabytes of journal.
    // Their per-frame targets stay at warn whatever the environment says.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,cosmic_ext_applet_taskbar=info".into());
    let filter = ["cosmic::applet", "iced_core::clipboard", "cosmic_config", "sctk"]
        .into_iter()
        .filter_map(|target| format!("{target}=warn").parse().ok())
        .fold(filter, |f, d| f.add_directive(d));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<app::Taskbar>(())
}
