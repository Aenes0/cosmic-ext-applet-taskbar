// Copyright 2023 System76 <info@system76.com>, 2026 Aenes0
// SPDX-License-Identifier: GPL-3.0-only

mod capture_wait;

use crate::wayland_subscription::{
    OutputUpdate, ToplevelRequest, ToplevelUpdate, WaylandImage, WaylandRequest, WaylandUpdate,
};
use std::{
    os::{
        fd::{AsFd, FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    sync::Arc,
};

use cctk::{
    screencopy::{
        CaptureFrame, CaptureOptions, CaptureSession, CaptureSource, Capturer, FailureReason,
        Formats, Frame, ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler,
        ScreencopySessionData, ScreencopySessionDataExt, ScreencopyState,
    },
    sctk::{
        self,
        activation::{RequestData, RequestDataExt},
        output::{OutputHandler, OutputState},
        reexports::{calloop, calloop_wayland_source::WaylandSource},
        seat::{SeatHandler, SeatState},
        shm::{Shm, ShmHandler},
    },
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        Connection, Dispatch, QueueHandle, WEnum,
        globals::registry_queue_init,
        protocol::{
            wl_buffer, wl_output,
            wl_seat::WlSeat,
            wl_shm::{self, WlShm},
            wl_shm_pool,
            wl_surface::WlSurface,
        },
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::State as WorkspaceUpdateState,
    },
    workspace::{WorkspaceHandler, WorkspaceState},
};
use cosmic_protocols::{
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
};
use futures::channel::mpsc::UnboundedSender;
use sctk::{
    activation::{ActivationHandler, ActivationState},
    registry::{ProvidesRegistryState, RegistryState},
};
struct AppData {
    exit: bool,
    tx: UnboundedSender<WaylandUpdate>,
    conn: Connection,
    queue_handle: QueueHandle<Self>,
    output_state: OutputState,
    workspace_state: WorkspaceState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: ToplevelManagerState,
    screencopy_state: ScreencopyState,
    registry_state: RegistryState,
    seat_state: SeatState,
    shm_state: Shm,
    activation_state: Option<ActivationState>,
}

// Workspace and toplevel handling

// Need to bind output globals just so workspace can get output events
impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(info) = self.output_state.info(&output) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::Output(OutputUpdate::Add(output, info)));
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(info) = self.output_state.info(&output) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::Output(OutputUpdate::Update(output, info)));
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let _ = self
            .tx
            .unbounded_send(WaylandUpdate::Output(OutputUpdate::Remove(output)));
    }
}

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        let active_workspaces = self
            .workspace_state
            .workspace_groups()
            .filter_map(|x| {
                x.workspaces
                    .iter()
                    .filter_map(|handle| self.workspace_state.workspace_info(handle))
                    .find(|w| w.state.contains(WorkspaceUpdateState::Active))
                    .map(|workspace| workspace.handle.clone())
            })
            .collect::<Vec<_>>();
        let _ = self
            .tx
            .unbounded_send(WaylandUpdate::Workspace(active_workspaces));
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!();
}

struct ExecRequestData {
    data: RequestData,
    exec: String,
    gpu_idx: Option<usize>,
    terminal: bool,
}

impl RequestDataExt for ExecRequestData {
    fn app_id(&self) -> Option<&str> {
        self.data.app_id()
    }

    fn seat_and_serial(&self) -> Option<(&WlSeat, u32)> {
        self.data.seat_and_serial()
    }

    fn surface(&self) -> Option<&WlSurface> {
        self.data.surface()
    }
}

impl ActivationHandler for AppData {
    type RequestData = ExecRequestData;
    fn new_token(&mut self, token: String, data: &ExecRequestData) {
        let _ = self.tx.unbounded_send(WaylandUpdate::ActivationToken {
            token: Some(token),
            app_id: data.app_id().map(String::from),
            exec: data.exec.clone(),
            gpu_idx: data.gpu_idx,
            terminal: data.terminal,
        });
    }
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut sctk::seat::SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        _: sctk::seat::Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut cctk::toplevel_management::ToplevelManagerState {
        &mut self.toplevel_manager_state
    }

    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
        // TODO capabilities could affect the options in the applet
    }
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::Toplevel(ToplevelUpdate::Add(info.clone())));
        }
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.toplevel_info_state.info(toplevel) {
            let _ = self
                .tx
                .unbounded_send(WaylandUpdate::Toplevel(ToplevelUpdate::Update(
                    info.clone(),
                )));
        }
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        toplevel: &ExtForeignToplevelHandleV1,
    ) {
        let _ = self
            .tx
            .unbounded_send(WaylandUpdate::Toplevel(ToplevelUpdate::Remove(
                toplevel.clone(),
            )));
    }
}

// Screencopy handling

// TODO: dmabuf? need to handle modifier negotation
type Session = capture_wait::Shared<Formats>;

#[derive(Default)]
struct SessionData {
    session: Arc<Session>,
    session_data: ScreencopySessionData,
}

struct FrameData {
    frame_data: ScreencopyFrameData,
    // Not the CaptureSession: that would keep it alive until the frame ends.
    session: Arc<Session>,
}

fn session_of(session: &CaptureSession) -> Option<&Session> {
    Some(&session.data::<SessionData>()?.session)
}

impl ScreencopySessionDataExt for SessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.session_data
    }
}

impl ScreencopyFrameDataExt for FrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.frame_data
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppData {
    fn event(
        _app_data: &mut Self,
        _buffer: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        (): &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppData {
    fn event(
        _app_data: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        (): &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

struct CaptureData {
    qh: QueueHandle<AppData>,
    conn: Connection,
    wl_shm: WlShm,
    capturer: Capturer,
}

impl CaptureData {
    pub fn capture_source_shm_fd<Fd: AsFd>(
        &self,
        source: &ExtForeignToplevelHandleV1,
        fd: Fd,
    ) -> Result<ShmImage<Fd>, String> {
        // TODO: way to get cursor metadata?
        let session = Arc::new(Session::default());
        let capture_session = self
            .capturer
            .create_session(
                &CaptureSource::Toplevel(source.clone()),
                CaptureOptions::empty(),
                &self.qh,
                SessionData {
                    session: session.clone(),
                    session_data: ScreencopySessionData::default(),
                },
            )
            .map_err(|_| "cannot capture this window".to_string())?;
        self.conn.flush().map_err(|e| format!("flush: {e}"))?;

        let formats = session.wait_formats(capture_wait::TIMEOUT)?;
        let (width, height) = formats.buffer_size;
        if width == 0 || height == 0 {
            return Err("window has no size".to_string());
        }
        // XXX
        if !formats.shm_formats.contains(&wl_shm::Format::Abgr8888) {
            tracing::warn!("Available formats: {:#?}", formats);
            return Err("no suitable buffer format found".to_string());
        }
        let buf_len = u64::from(width) * u64::from(height) * 4;
        let pool_len = i32::try_from(buf_len).map_err(|_| "window too big".to_string())?;
        // A failed resize shows up as a failed capture.
        let _ = rustix::fs::ftruncate(&fd, buf_len);
        let shot = self.shoot(&capture_session, &session, fd.as_fd(), (width, height), pool_len);
        match shot {
            Ok(()) => Ok(ShmImage { fd, width, height }),
            Err(why) => {
                if !capture_wait::should_release(&Err(why.clone())) {
                    // The compositor may still write into it: never close it.
                    std::mem::forget(fd);
                }
                Err(why)
            }
        }
    }

    /// Asks for one frame and waits for it; the pool and buffer are always destroyed.
    fn shoot(
        &self,
        capture_session: &CaptureSession,
        session: &Arc<Session>,
        fd: std::os::fd::BorrowedFd<'_>,
        (width, height): (u32, u32),
        pool_len: i32,
    ) -> Result<(), String> {
        // Sizes are checked: both fit in an i32 and `width * 4` cannot overflow it.
        let (w, h) = (width as i32, height as i32);
        let pool = self.wl_shm.create_pool(fd, pool_len, &self.qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Abgr8888, &self.qh, ());
        capture_session.capture(
            &buffer,
            &[],
            &self.qh,
            FrameData {
                frame_data: ScreencopyFrameData::default(),
                session: session.clone(),
            },
        );
        // TODO: wait for server to release buffer?
        let res = self
            .conn
            .flush()
            .map_err(|e| format!("flush: {e}"))
            .and_then(|()| session.wait_result(capture_wait::TIMEOUT));
        // After a timeout the frame is still pending: destroying its buffer
        // could make the compositor crash, so the pool and buffer leak.
        if capture_wait::should_release(&res) {
            pool.destroy();
            buffer.destroy();
        }
        res
    }
}

pub struct ShmImage<T: AsFd> {
    fd: T,
    pub width: u32,
    pub height: u32,
}

impl<T: AsFd> ShmImage<T> {
    /// The picture, at most `max` pixels on its longer side: shrunk straight
    /// from the shared memory, with no copy of the whole window first.
    pub fn thumbnail(&self, max: u32) -> anyhow::Result<image::RgbaImage> {
        let mmap = unsafe { memmap2::Mmap::map(&self.fd.as_fd())? };
        let len = (self.width * self.height * 4) as usize;
        let full = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
            self.width,
            self.height,
            mmap.get(..len).ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))?,
        )
        .ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))?;
        let longer = self.width.max(self.height);
        if longer <= max {
            return Ok(image::RgbaImage::from_raw(self.width, self.height, full.to_vec())
                .ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))?);
        }
        // Most of the window's pixels never read: a 2×2 average every k
        // pixels takes it to about twice the thumbnail, the box filter does
        // the rest. Ten times faster than resizing it all, the same at
        // this size.
        let k = (longer / (max * 2)).max(1);
        let (dw, dh) = (self.width / k, self.height / k);
        let next = if k > 1 { 1 } else { 0 };
        let mut small = image::RgbaImage::new(dw, dh);
        for (x, y, px) in small.enumerate_pixels_mut() {
            let (x0, y0) = (x * k, y * k);
            let corners = [
                full.get_pixel(x0, y0),
                full.get_pixel(x0 + next, y0),
                full.get_pixel(x0, y0 + next),
                full.get_pixel(x0 + next, y0 + next),
            ];
            for ch in 0..4 {
                let sum: u16 = corners.iter().map(|p| u16::from(p.0[ch])).sum();
                px.0[ch] = (sum / 4) as u8;
            }
        }
        let scale = max as f32 / dw.max(dh) as f32;
        let width = ((dw as f32 * scale).round() as u32).clamp(1, dw);
        let height = ((dh as f32 * scale).round() as u32).clamp(1, dh);
        Ok(image::imageops::thumbnail(&small, width, height))
    }
}

impl AppData {
    fn cosmic_toplevel(
        &self,
        handle: &ExtForeignToplevelHandleV1,
    ) -> Option<ZcosmicToplevelHandleV1> {
        self.toplevel_info_state
            .info(handle)?
            .cosmic_toplevel
            .clone()
    }

    fn send_image(&self, handle: ExtForeignToplevelHandleV1) {
        let tx = self.tx.clone();
        let capture_data = CaptureData {
            qh: self.queue_handle.clone(),
            conn: self.conn.clone(),
            wl_shm: self.shm_state.wl_shm().clone(),
            capturer: self.screencopy_state.capturer().clone(),
        };
        let job_handle = handle.clone();
        // `done` answers exactly once: image or failure, also if the thread never runs.
        capture_wait::run_once(
            move || capture_thumbnail(&capture_data, &job_handle),
            move |img| {
                let update = match img {
                    Some(img) => WaylandUpdate::Image(handle, WaylandImage::new(img)),
                    None => WaylandUpdate::ImageFailed(handle),
                };
                if let Err(err) = tx.unbounded_send(update) {
                    tracing::error!("Failed to send image event to subscription {err:?}");
                }
            },
            |task| {
                let spawned = std::thread::Builder::new()
                    .name("taskbar-capture".into())
                    .spawn(task);
                if let Err(err) = spawned {
                    // The task is dropped unrun, which reports the failure.
                    tracing::warn!("cannot start the capture thread: {err}");
                }
            },
        );
    }
}

/// One thumbnail of a window, `None` (and a log line) when it cannot be taken.
fn capture_thumbnail(
    capture_data: &CaptureData,
    handle: &ExtForeignToplevelHandleV1,
) -> Option<image::RgbaImage> {
    let name = c"taskbar-screencopy";
    let result = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| format!("no memfd: {e}"))
        .and_then(|fd| capture_data.capture_source_shm_fd(handle, fd))
        // Small enough for a thumbnail, sharp on a scaled screen.
        .and_then(|img| img.thumbnail(400).map_err(|e| e.to_string()));
    match result {
        Ok(img) => Some(img),
        Err(why) => {
            tracing::debug!("no picture of a window: {why}");
            None
        }
    }
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl ScreencopyHandler for AppData {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy_state
    }

    fn init_done(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        session: &CaptureSession,
        formats: &Formats,
    ) {
        if let Some(shared) = session_of(session) {
            shared.update(|data| data.formats = Some(formats.clone()));
        }
    }

    fn ready(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        screencopy_frame: &CaptureFrame,
        _frame: Frame,
    ) {
        if let Some(frame) = screencopy_frame.data::<FrameData>() {
            frame.session.update(|data| data.res = Some(Ok(())));
        }
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        screencopy_frame: &CaptureFrame,
        reason: WEnum<FailureReason>,
    ) {
        if let Some(frame) = screencopy_frame.data::<FrameData>() {
            frame
                .session
                .update(|data| data.res = Some(Err(format!("capture failed: {reason:?}"))));
        }
    }

    fn stopped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, session: &CaptureSession) {
        // A stopped session will never answer: release the waiting thread.
        if let Some(shared) = session_of(session) {
            shared.update(|data| data.stopped = true);
        }
    }
}

pub(crate) fn wayland_handler(
    tx: UnboundedSender<WaylandUpdate>,
    rx: calloop::channel::Channel<WaylandRequest>,
) {
    let socket = std::env::var("X_PRIVILEGED_WAYLAND_SOCKET")
        .ok()
        .and_then(|fd| {
            fd.parse::<RawFd>()
                .ok()
                .map(|fd| unsafe { UnixStream::from_raw_fd(fd) })
        });

    let conn = if let Some(socket) = socket {
        Connection::from_socket(socket).unwrap()
    } else {
        Connection::connect_to_env().unwrap()
    };
    let (globals, event_queue) = registry_queue_init(&conn).unwrap();

    let mut event_loop = calloop::EventLoop::<AppData>::try_new().unwrap();
    let qh = event_queue.handle();
    let wayland_source = WaylandSource::new(conn.clone(), event_queue);
    let handle = event_loop.handle();
    wayland_source
        .insert(handle.clone())
        .expect("Failed to insert wayland source.");

    if handle
        .insert_source(rx, |event, (), state| match event {
            calloop::channel::Event::Msg(req) => match req {
                WaylandRequest::Screencopy(handle) => {
                    state.send_image(handle.clone());
                }
                WaylandRequest::Toplevel(req) => match req {
                    ToplevelRequest::Activate(handle) => {
                        if let Some(cosmic_toplevel) = state.cosmic_toplevel(&handle) {
                            if let Some(seat) = state.seat_state.seats().next() {
                                let manager = &state.toplevel_manager_state.manager;
                                manager.activate(&cosmic_toplevel, &seat);
                            }
                        }
                    }
                    ToplevelRequest::Minimize(handle) => {
                        if let Some(cosmic_toplevel) = state.cosmic_toplevel(&handle) {
                            let manager = &state.toplevel_manager_state.manager;
                            manager.set_minimized(&cosmic_toplevel);
                        }
                    }
                    ToplevelRequest::Quit(handle) => {
                        if let Some(cosmic_toplevel) = state.cosmic_toplevel(&handle) {
                            let manager = &state.toplevel_manager_state.manager;
                            manager.close(&cosmic_toplevel);
                        }
                    }
                },
                WaylandRequest::TokenRequest {
                    app_id,
                    exec,
                    gpu_idx,
                    terminal,
                } => {
                    if let Some(activation_state) = state.activation_state.as_ref() {
                        activation_state.request_token_with_data(
                            &state.queue_handle,
                            ExecRequestData {
                                data: RequestData {
                                    app_id: Some(app_id),
                                    seat_and_serial: state
                                        .seat_state
                                        .seats()
                                        .next()
                                        .map(|seat| (seat, 0)),
                                    surface: None,
                                },
                                exec,
                                gpu_idx,
                                terminal,
                            },
                        );
                    } else {
                        let _ = state.tx.unbounded_send(WaylandUpdate::ActivationToken {
                            token: None,
                            app_id: Some(app_id),
                            exec,
                            gpu_idx,
                            terminal,
                        });
                    }
                }
            },
            calloop::channel::Event::Closed => {
                state.exit = true;
            }
        })
        .is_err()
    {
        return;
    }
    let registry_state = RegistryState::new(&globals);

    let mut app_data = AppData {
        exit: false,
        tx,
        conn,
        output_state: OutputState::new(&globals, &qh),
        workspace_state: WorkspaceState::new(&registry_state, &qh),
        toplevel_info_state: ToplevelInfoState::new(&registry_state, &qh),
        toplevel_manager_state: ToplevelManagerState::new(&registry_state, &qh),
        screencopy_state: ScreencopyState::new(&globals, &qh),
        registry_state,
        seat_state: SeatState::new(&globals, &qh),
        shm_state: Shm::bind(&globals, &qh).unwrap(),
        activation_state: ActivationState::bind::<AppData>(&globals, &qh).ok(),
        queue_handle: qh,
    };

    loop {
        if app_data.exit {
            break;
        }
        event_loop.dispatch(None, &mut app_data).unwrap();
    }
}

sctk::delegate_seat!(AppData);
sctk::delegate_registry!(AppData);
sctk::delegate_shm!(AppData);
cctk::delegate_toplevel_info!(AppData);
cctk::delegate_workspace!(AppData);
cctk::delegate_toplevel_manager!(AppData);
cctk::delegate_screencopy!(AppData);

sctk::delegate_activation!(AppData, ExecRequestData);

sctk::delegate_output!(AppData);
