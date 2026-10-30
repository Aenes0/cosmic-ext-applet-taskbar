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
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,cosmic_ext_applet_taskbar=info".into()),
        )
        .init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<app::Taskbar>(())
}
