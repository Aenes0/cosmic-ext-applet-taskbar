// Copyright 2023 System76 <info@system76.com>, 2026 Aenes0
// SPDX-License-Identifier: GPL-3.0-only

//! The windows, workspaces and outputs, from the compositor's thread.
use cctk::{
    sctk::{output::OutputInfo, reexports::calloop},
    toplevel_info::ToplevelInfo,
    wayland_client::protocol::wl_output::WlOutput,
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    },
};
use cosmic::{
    iced::core::Bytes,
    iced::{self, Subscription, stream},
    widget::image::Handle,
};

use futures::{
    SinkExt, StreamExt,
    channel::mpsc::{UnboundedReceiver, unbounded},
};
use std::fmt::Debug;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::wayland_handler::wayland_handler;

pub static WAYLAND_RX: LazyLock<Mutex<Option<UnboundedReceiver<WaylandUpdate>>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn wayland_subscription() -> iced::Subscription<WaylandUpdate> {
    Subscription::run_with(std::any::TypeId::of::<WaylandUpdate>(), |_| {
        stream::channel(50, move |mut output| async move {
            let mut state = State::Waiting;

            loop {
                state = start_listening(state, &mut output).await;
            }
        })
    })
}

pub enum State {
    Waiting,
    Finished,
}

/// A window's picture, made into a handle once: a new handle each frame
/// would upload it again each frame.
#[derive(Debug, Clone)]
pub struct WaylandImage {
    pub handle: Handle,
    pub width: u32,
    pub height: u32,
}

impl WaylandImage {
    pub fn new(img: image::RgbaImage) -> Self {
        let (width, height) = (img.width(), img.height());
        Self {
            handle: Handle::from_rgba(width, height, Bytes::from_owner(img.into_vec())),
            width,
            height,
        }
    }
}

async fn start_listening(
    state: State,
    output: &mut futures::channel::mpsc::Sender<WaylandUpdate>,
) -> State {
    match state {
        State::Waiting => {
            let mut guard = WAYLAND_RX.lock().await;
            let rx = {
                if guard.is_none() {
                    let (calloop_tx, calloop_rx) = calloop::channel::channel();
                    let (toplevel_tx, toplevel_rx) = unbounded();
                    let _ = std::thread::spawn(move || {
                        wayland_handler(toplevel_tx, calloop_rx);
                    });
                    *guard = Some(toplevel_rx);
                    _ = output.send(WaylandUpdate::Init(calloop_tx)).await;
                }
                guard.as_mut().unwrap()
            };
            if let Some(u) = rx.next().await {
                _ = output.send(u).await;
                State::Waiting
            } else {
                _ = output.send(WaylandUpdate::Finished).await;
                tracing::error!("Wayland handler thread died");
                State::Finished
            }
        }
        State::Finished => iced::futures::future::pending().await,
    }
}

#[derive(Clone, Debug)]
pub enum WaylandUpdate {
    Init(calloop::channel::Sender<WaylandRequest>),
    Finished,
    Toplevel(ToplevelUpdate),
    Workspace(Vec<ExtWorkspaceHandleV1>),
    Output(OutputUpdate),
    ActivationToken {
        token: Option<String>,
        app_id: Option<String>,
        exec: String,
        gpu_idx: Option<usize>,
        terminal: bool,
    },
    Image(ExtForeignToplevelHandleV1, WaylandImage),
    /// A picture asked for that won't come.
    ImageFailed(ExtForeignToplevelHandleV1),
}

#[derive(Clone, Debug)]
pub enum ToplevelUpdate {
    Add(ToplevelInfo),
    Update(ToplevelInfo),
    Remove(ExtForeignToplevelHandleV1),
}

#[derive(Clone, Debug)]
pub enum OutputUpdate {
    Add(WlOutput, OutputInfo),
    Update(WlOutput, OutputInfo),
    Remove(WlOutput),
}

#[derive(Clone, Debug)]
pub enum WaylandRequest {
    Toplevel(ToplevelRequest),
    TokenRequest {
        app_id: String,
        exec: String,
        gpu_idx: Option<usize>,
        terminal: bool,
    },
    Screencopy(ExtForeignToplevelHandleV1),
}

#[derive(Debug, Clone)]
pub enum ToplevelRequest {
    Activate(ExtForeignToplevelHandleV1),
    Minimize(ExtForeignToplevelHandleV1),
    Quit(ExtForeignToplevelHandleV1),
}
