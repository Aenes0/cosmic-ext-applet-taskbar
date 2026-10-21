// Copyright 2023 System76 <info@system76.com>, 2026 Aenes0
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    config::{APP_ID as LIST_ID, AppListConfig, ToplevelFilter},
    texts::texts,
    wayland_subscription::{
        OutputUpdate, ToplevelRequest, ToplevelUpdate, WaylandImage, WaylandRequest, WaylandUpdate,
        wayland_subscription,
    },
};
use crate::launcher_entry::{self, Badge, BadgeUpdate, Badges};
use cctk::{
    sctk::{output::OutputInfo, reexports::calloop::channel::Sender},
    toplevel_info::ToplevelInfo,
    wayland_client::protocol::{
        wl_data_device_manager::DndAction, wl_output::WlOutput, wl_seat::WlSeat,
    },
    wayland_protocols::ext::{
        foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        workspace::v1::client::ext_workspace_handle_v1::ExtWorkspaceHandleV1,
    },
};
use cosmic::{
    Apply, Element, Task, app,
    applet::{
        Context, Size,
        cosmic_panel_config::{PanelAnchor, PanelSize},
    },
    cosmic_config::{Config, CosmicConfigEntry},
    desktop::IconSourceExt,
    iced::{
        self, Alignment, Background, Border, Length, Limits, Padding, Subscription,
        advanced::text::{Ellipsize, EllipsizeHeightLimit},
        clipboard::mime::{AllowedMimeTypes, AsMimeTypes},
        event::listen_with,
        platform_specific::shell::commands::popup::destroy_popup,
        runtime::{core::event, dnd::peek_dnd},
        widget::{
            Column, Row, column, mouse_area, row,
            space::{horizontal as horizontal_space, vertical as vertical_space},
        },
        window,
    },
    surface::{self, action::LiveSettings},
    theme::{self, Button, Container},
    widget::{
        DndDestination, Image, button, container, divider, dnd_source,
        icon::{self, from_name},
        menu,
        rectangle_tracker::{RectangleTracker, RectangleUpdate, rectangle_tracker_subscription},
        text,
    },
};
use cosmic::{
    desktop::fde::{self, DesktopEntry, get_languages_from_env, unicase::Ascii},
    widget::DndSource,
};
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::State;
use futures::future::pending;
use rustc_hash::FxHashMap;
use std::{borrow::Cow, path::PathBuf, str::FromStr, time::Duration};
use switcheroo_control::Gpu;
use tokio::time::sleep;
use url::Url;

static MIME_TYPE: &str = "text/uri-list";

#[derive(Debug, Clone)]
struct AppletIconData {
    icon_size: u16,
    icon_spacing: f32,
    /// How thick the line under a running app is, and how long: short when
    /// its windows are in the back, long when one of them is in front, as
    /// on Windows 11.
    bar_thickness: f32,
    bar_short: f32,
    bar_long: f32,
    bar_gap: f32,
    padding: Padding,
}

static DND_FAVORITES: u64 = u64::MAX;

impl AppletIconData {
    fn new(applet: &Context) -> Self {
        let icon_size = applet.suggested_size(false).0;
        let (major_padding, cross_padding) = applet.suggested_padding(false);
        let (h_padding, v_padding) = if applet.is_horizontal() {
            (major_padding as f32, cross_padding as f32)
        } else {
            (cross_padding as f32, major_padding as f32)
        };
        let icon_spacing = applet.spacing as f32;

        let (bar_thickness, bar_short, bar_long): (f32, f32, f32) = match applet.size {
            Size::Hardcoded(_) => (3.0, 6.0, 16.0),
            Size::PanelSize(ref s) => {
                let size = s.get_applet_icon_size_with_padding(false);
                let medium_size_threshold = PanelSize::M.get_applet_icon_size_with_padding(false);
                if size <= medium_size_threshold {
                    (3.0, 6.0, 16.0)
                } else {
                    (4.0, 8.0, 22.0)
                }
            }
        };
        // The line takes its room from the padding on the panel's edge; on
        // a thin panel, it thins to fit.
        let edge_padding = if applet.is_horizontal() { v_padding } else { h_padding };
        let bar_thickness = bar_thickness.min(edge_padding - 1.).max(2.);
        // Off the screen's edge by a pixel at least, and off the icon by one
        // when there is room for both.
        let bar_gap = if edge_padding >= bar_thickness + 2. { 1. } else { 0. };
        let room = bar_thickness + bar_gap;
        let padding = match applet.anchor {
            PanelAnchor::Top => [v_padding - room, h_padding, v_padding, h_padding],
            PanelAnchor::Bottom => [v_padding, h_padding, v_padding - room, h_padding],
            PanelAnchor::Left => [v_padding, h_padding, v_padding, h_padding - room],
            PanelAnchor::Right => [v_padding, h_padding - room, v_padding, h_padding],
        };
        AppletIconData {
            icon_size,
            icon_spacing,
            bar_thickness,
            bar_short,
            bar_long,
            bar_gap,
            padding: padding.map(|p| p.max(0.)).into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockItemId {
    Item(u32),
    ActiveOverflow,
    FavoritesOverflow,
}

impl From<u32> for DockItemId {
    fn from(id: u32) -> Self {
        DockItemId::Item(id)
    }
}

impl From<usize> for DockItemId {
    fn from(id: usize) -> Self {
        DockItemId::Item(id as u32)
    }
}

#[derive(Debug, Clone)]
struct DockItem {
    // ID used internally in the applet. Each dock item
    // have an unique id
    id: u32,
    toplevels: Vec<(ToplevelInfo, Option<WaylandImage>)>,
    // Information found in the .desktop file
    desktop_info: DesktopEntry,
    // We must use this because the id in `DesktopEntry` is an estimation.
    // Thus, if we unpin an item, we want to be sure to use the real id
    original_app_id: String,
}

impl DockItem {
    fn as_icon(
        &self,
        applet: &Context,
        rectangle_tracker: Option<&RectangleTracker<DockItemId>>,
        interaction_enabled: bool,
        dnd_source_enabled: bool,
        gpus: Option<&[Gpu]>,
        is_focused: bool,
        badge: &Badge,
        window_id: window::Id,
        filter: Option<&dyn Fn(&ToplevelInfo) -> bool>,
    ) -> Element<'_, Message> {
        let Self {
            toplevels,
            desktop_info,
            id,
            ..
        } = self;

        let filtered_toplevels: Vec<_> = if let Some(filter_fn) = filter {
            toplevels
                .iter()
                .filter(|(info, _)| filter_fn(info))
                .collect()
        } else {
            toplevels.iter().collect()
        };
        let toplevel_count = filtered_toplevels.len();

        let app_icon = AppletIconData::new(applet);
        let icon_size = f32::from(app_icon.icon_size);

        let cosmic_icon = cosmic::widget::icon(
            fde::IconSource::from_unknown(desktop_info.icon().unwrap_or_default()).as_cosmic_icon(),
        )
        // sets the preferred icon size variant
        .size(128)
        .width(app_icon.icon_size.into())
        .height(app_icon.icon_size.into());

        // Over the icon, as on Windows: the count at the top right, the
        // progress along the bottom.
        let mut layers: Vec<Element<_>> = vec![cosmic_icon.clone().into()];
        if let Some(progress) = badge.progress {
            let track = (icon_size - 2.0).max(1.0);
            layers.push(
                container(
                    container(horizontal_space().width(Length::Fixed(0.0)))
                        .width(Length::Fixed((track * progress as f32).max(2.0)))
                        .height(Length::Fixed(3.0))
                        .class(theme::Container::custom(|theme| container::Style {
                            background: Some(Background::Color(
                                theme.cosmic().success_color().into(),
                            )),
                            border: Border {
                                radius: 2.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        })),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([0.0, 1.0])
                .align_x(Alignment::Start)
                .align_y(Alignment::End)
                .into(),
            );
        }
        if let Some(count) = badge.count {
            let label = if count > 99 {
                "99+".to_string()
            } else {
                count.to_string()
            };
            layers.push(
                container(
                    container(text(label).size(9.0).line_height(1.0))
                        .center_x(Length::Shrink)
                        .padding([1.0, 3.5])
                        .class(theme::Container::custom(|theme| container::Style {
                            text_color: Some(theme.cosmic().accent_text_color().into()),
                            background: Some(Background::Color(
                                theme.cosmic().accent_color().into(),
                            )),
                            border: Border {
                                radius: 8.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        })),
                )
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::End)
                .align_y(Alignment::Start)
                .into(),
            );
        }
        let icon: Element<_> = if layers.len() == 1 {
            cosmic_icon.clone().into()
        } else {
            container(iced::widget::Stack::with_children(layers))
                .width(Length::Fixed(icon_size))
                .height(Length::Fixed(icon_size))
                .into()
        };

        // The line under a running app: always there, so the icons don't
        // move; long and in the accent color under the app in front.
        let bar_length = match (toplevel_count, is_focused) {
            (0, _) => 0.0,
            (_, true) => app_icon.bar_long,
            (_, false) => app_icon.bar_short,
        };
        let (bar_w, bar_h) = if applet.is_horizontal() {
            (bar_length, app_icon.bar_thickness)
        } else {
            (app_icon.bar_thickness, bar_length)
        };
        let thickness = app_icon.bar_thickness;
        let indicator = container(horizontal_space().width(Length::Fixed(0.0)))
            .width(Length::Fixed(bar_w))
            .height(Length::Fixed(bar_h))
            .class(theme::Container::custom(move |theme| container::Style {
                background: (bar_length > 0.0).then(|| {
                    Background::Color(if is_focused {
                        theme.cosmic().accent_color().into()
                    } else {
                        let mut c: iced::Color = theme.cosmic().on_bg_color().into();
                        c.a = 0.55;
                        c
                    })
                }),
                border: Border {
                    radius: (thickness / 2.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }));
        let indicator = if applet.is_horizontal() {
            container(indicator).center_x(Length::Fixed(icon_size))
        } else {
            container(indicator).center_y(Length::Fixed(icon_size))
        };

        let gap = || -> Element<_> {
            if applet.is_horizontal() {
                vertical_space().height(Length::Fixed(app_icon.bar_gap)).into()
            } else {
                horizontal_space().width(Length::Fixed(app_icon.bar_gap)).into()
            }
        };
        let icon_wrapper: Element<_> = match applet.anchor {
            PanelAnchor::Left => row([indicator.into(), gap(), icon])
                .align_y(Alignment::Center)
                .into(),
            PanelAnchor::Right => row([icon, gap(), indicator.into()])
                .align_y(Alignment::Center)
                .into(),
            PanelAnchor::Top => column([indicator.into(), gap(), icon])
                .align_x(Alignment::Center)
                .into(),
            PanelAnchor::Bottom => column([icon, gap(), indicator.into()])
                .align_x(Alignment::Center)
                .into(),
        };

        let icon_button = button::custom(icon_wrapper)
            .padding(app_icon.padding)
            .selected(is_focused)
            .class(app_list_icon_style(is_focused, badge.urgent && !is_focused));

        let icon_button: Element<_> = if interaction_enabled {
            mouse_area(
                icon_button
                    .on_press_maybe(if toplevel_count == 0 {
                        launch_on_preferred_gpu(desktop_info, gpus)
                    } else if toplevel_count == 1 {
                        filtered_toplevels
                            .first()
                            .map(|t| Message::Toggle(t.0.foreign_toplevel.clone()))
                    } else {
                        // As on Windows: a group's click shows its windows.
                        Some(Message::GroupClick(*id, window_id))
                    })
                    .width(Length::Shrink)
                    .height(Length::Shrink),
            )
            .on_right_release(Message::Popup(*id, window_id))
            .on_middle_release({
                launch_on_preferred_gpu(desktop_info, gpus)
                    .unwrap_or(Message::Popup(*id, window_id))
            })
            .on_enter(Message::IconHover(*id, window_id, true))
            .on_exit(Message::IconHover(*id, window_id, false))
            .into()
        } else {
            icon_button.into()
        };

        let path = desktop_info.path.clone();
        let icon_button = if dnd_source_enabled && interaction_enabled {
            DndSource::with_id(icon_button, cosmic::widget::Id::new("asdfasdfadfs"))
                .window(window_id)
                .drag_icon(move |_| {
                    (
                        cosmic_icon.clone().into(),
                        iced::core::widget::tree::State::None,
                        iced::Vector::ZERO,
                    )
                })
                .drag_threshold(16.)
                .drag_content(move || DndPathBuf(path.clone()))
                .on_start(Some(Message::StartDrag(*id)))
                .on_cancel(Some(Message::DragFinished))
                .on_finish(Some(Message::DragFinished))
        } else {
            dnd_source(icon_button)
        };

        if let Some(tracker) = rectangle_tracker {
            tracker.container((*id).into(), icon_button).into()
        } else {
            icon_button.into()
        }
    }
}

/// A message, some time from now.
fn after(wait: Duration, message: Message) -> Task<cosmic::Action<Message>> {
    Task::perform(sleep(wait), move |()| cosmic::Action::App(message.clone()))
}

#[derive(Debug, Clone, Default)]
struct DndOffer {
    dock_item: Option<DockItem>,
    preview_index: usize,
}

#[derive(Debug, Clone)]
pub struct Popup {
    parent: window::Id,
    id: window::Id,
    dock_item: DockItem,
    popup_type: PopupType,
}

#[derive(Clone, Default)]
pub struct Taskbar {
    core: cosmic::app::Core,
    popup: Option<Popup>,
    subscription_ctr: u32,
    item_ctr: u32,
    desktop_entries: Vec<DesktopEntry>,
    active_list: Vec<DockItem>,
    pinned_list: Vec<DockItem>,
    dnd_source: Option<(window::Id, DockItem, DndAction, Option<usize>)>,
    config: AppListConfig,
    wayland_sender: Option<Sender<WaylandRequest>>,
    seat: Option<WlSeat>,
    rectangle_tracker: Option<RectangleTracker<DockItemId>>,
    rectangles: FxHashMap<DockItemId, iced::Rectangle>,
    dnd_offer: Option<DndOffer>,
    is_listening_for_dnd: bool,
    gpus: Option<Vec<Gpu>>,
    active_workspaces: Vec<ExtWorkspaceHandleV1>,
    output_list: FxHashMap<WlOutput, OutputInfo>,
    locales: Vec<String>,
    hovered_toplevel: Option<ExtForeignToplevelHandleV1>,
    overflow_favorites_popup: Option<window::Id>,
    overflow_active_popup: Option<window::Id>,
    /// The app button the pointer is on, and the surface it is in.
    hover: Option<(u32, window::Id)>,
    /// Whether the pointer is on the windows' previews.
    preview_hovered: bool,
    /// Counts the pointer's moves: a wait that finds it moved since does
    /// nothing.
    hover_gen: u64,
    badges: Badges,
    /// `COSMIC_TASKBAR_PREVIEW`: an app whose previews open by themselves
    /// once, to take screenshots.
    preview_on_start: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PopupType {
    RightClickMenu,
    /// The app's windows, as pictures, from the pointer on its button: it
    /// takes no keyboard and closes when the pointer leaves, as on Windows.
    Preview,
}

#[derive(Debug, Clone)]
pub enum Message {
    Wayland(WaylandUpdate),
    PinApp(u32),
    UnpinApp(u32),
    Popup(u32, window::Id),
    Pressed(window::Id),
    GroupClick(u32, window::Id),
    IconHover(u32, window::Id, bool),
    PreviewHover(bool),
    PointerLeft(window::Id),
    HoverSettled(u64),
    RefreshPreview,
    Badge(BadgeUpdate),
    ToplevelHoverChanged(ExtForeignToplevelHandleV1, bool),
    GpuRequest(Option<Vec<Gpu>>),
    CloseRequested(window::Id),
    ClosePopup,
    Toggle(ExtForeignToplevelHandleV1),
    Exec(String, Option<usize>, bool),
    CloseToplevel(ExtForeignToplevelHandleV1),
    Quit(String),
    NewSeat(WlSeat),
    RemovedSeat,
    Rectangle(RectangleUpdate<DockItemId>),
    StartDrag(u32),
    DragFinished,
    DndEnter(f64, f64),
    DndLeave,
    DndMotion(f64, f64),
    DndDropFinished,
    DndData(Option<DndPathBuf>),
    StartListeningForDnd,
    StopListeningForDnd,
    IncrementSubscriptionCtr,
    ConfigUpdated(AppListConfig),
    OpenFavorites,
    OpenActive,
    Surface(surface::Action<Message>),
}

fn index_in_list(
    mut list_len: usize,
    item_size: f32,
    divider_size: f32,
    existing_preview: Option<usize>,
    pos_in_list: f32,
) -> usize {
    if existing_preview.is_some() {
        list_len += 1;
    }

    let index = if (list_len == 0) || (pos_in_list < item_size / 2.0) {
        0
    } else {
        let mut i = 1;
        let mut pos = item_size / 2.0;
        while i < list_len {
            let next_pos = pos + item_size + divider_size;
            if pos < pos_in_list && pos_in_list < next_pos {
                break;
            }
            pos = next_pos;
            i += 1;
        }
        i
    };

    if let Some(existing_preview) = existing_preview {
        if index >= existing_preview {
            index.saturating_sub(1)
        } else {
            index
        }
    } else {
        index
    }
}

async fn try_get_gpus() -> Option<Vec<Gpu>> {
    let connection = zbus::Connection::system().await.ok()?;
    let proxy = switcheroo_control::SwitcherooControlProxy::new(&connection)
        .await
        .ok()?;

    if !proxy.has_dual_gpu().await.ok()? {
        return None;
    }

    let gpus = proxy.get_gpus().await.ok()?;
    if gpus.is_empty() {
        return None;
    }

    Some(gpus)
}

const TOPLEVEL_BUTTON_WIDTH: f32 = 224.0;
const TOPLEVEL_BUTTON_HEIGHT: f32 = 164.0;
/// How long the pointer rests on a button before its windows show, and
/// how long they stay after it leaves, so it can get to them.
const HOVER_OPEN: Duration = Duration::from_millis(400);
const HOVER_CLOSE: Duration = Duration::from_millis(300);
/// How often the previews' pictures are taken again while they show.
const PREVIEW_REFRESH: Duration = Duration::from_millis(1000);

/// A window, as Windows 11 previews it: the app's icon, the title and the
/// close button on top, the window's picture under them.
fn toplevel_button<'a>(
    img: Option<&WaylandImage>,
    app_icon: icon::Handle,
    title: String,
    handle: ExtForeignToplevelHandleV1,
    is_focused: bool,
    is_hovered: bool,
) -> Element<'a, Message> {
    let title = if title.trim().is_empty() {
        texts().untitled.to_string()
    } else {
        title
    };
    let close: Element<_> = if is_hovered {
        button::custom(icon::from_name("window-close-symbolic").size(16))
            .class(Button::Destructive)
            .on_press(Message::CloseToplevel(handle.clone()))
            .padding(4)
            .into()
    } else {
        // Same size, so the title doesn't move.
        horizontal_space().width(Length::Fixed(24.0)).into()
    };
    let header = row![
        icon::icon(app_icon.clone()).size(16),
        text::body(title)
            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
            .width(Length::Fill),
        close,
    ]
    .spacing(8)
    .height(Length::Fixed(24.0))
    .align_y(Alignment::Center);

    // A minimized window has no picture: its app's icon, big, instead.
    let picture: Element<_> = match img {
        Some(img) => Image::new(img.handle.clone())
            .content_fit(iced::ContentFit::Contain)
            .into(),
        None => icon::icon(app_icon).size(48).into(),
    };
    let picture = container(picture)
        .center(Length::Fill)
        .class(Container::custom(move |theme| container::Style {
            border: Border {
                radius: theme.cosmic().radius_s().into(),
                ..Default::default()
            },
            ..Default::default()
        }));

    column![header, picture]
        .spacing(6)
        .apply(button::custom)
        .on_press(Message::Toggle(handle.clone()))
        .class(window_menu_style(is_focused))
        .width(Length::Fixed(TOPLEVEL_BUTTON_WIDTH))
        .height(Length::Fixed(TOPLEVEL_BUTTON_HEIGHT))
        .padding([4, 4, 8, 8])
        .selected(is_focused)
        .apply(mouse_area)
        .on_enter(Message::ToplevelHoverChanged(handle.clone(), true))
        .on_middle_press(Message::CloseToplevel(handle.clone()))
        .on_exit(Message::ToplevelHoverChanged(handle, false))
        .apply(Element::from)
}

fn window_menu_style(selected: bool) -> cosmic::theme::Button {
    let radius = theme::active()
        .cosmic()
        .radius_m()
        .map(|x| if x < 8.0 { x } else { x - 4.0 });

    Button::Custom {
        active: Box::new(move |focused, theme| {
            let a = button::Catalog::active(theme, focused, selected, &Button::AppletMenu);
            button::Style {
                background: if selected {
                    Some(Background::Color(
                        theme.cosmic().icon_button.selected_state_color().into(),
                    ))
                } else {
                    a.background
                },
                border_radius: radius.into(),
                outline_width: 0.0,
                ..a
            }
        }),
        hovered: Box::new(move |focused, theme| {
            let focused = selected || focused;
            let text = button::Catalog::hovered(theme, focused, focused, &Button::AppletMenu);
            button::Style {
                border_radius: radius.into(),
                outline_width: 0.0,
                ..text
            }
        }),
        disabled: Box::new(move |theme| {
            let text = button::Catalog::disabled(theme, &Button::AppletMenu);
            button::Style {
                border_radius: radius.into(),
                outline_width: 0.0,
                ..text
            }
        }),
        pressed: Box::new(move |focused, theme| {
            let focused = selected || focused;
            let text = button::Catalog::pressed(theme, focused, focused, &Button::AppletMenu);
            button::Style {
                border_radius: radius.into(),
                outline_width: 0.0,
                ..text
            }
        }),
    }
}

fn app_list_icon_style(selected: bool, urgent: bool) -> cosmic::theme::Button {
    // An app that wants to be looked at glows orange until it is, as on
    // Windows.
    let urgent_color = |theme: &cosmic::Theme, alpha: f32| {
        let mut c: iced::Color = theme.cosmic().warning_color().into();
        c.a = alpha;
        Some(Background::Color(c))
    };
    Button::Custom {
        active: Box::new(move |focused, theme| {
            let a = button::Catalog::active(theme, focused, selected, &Button::AppletIcon);
            button::Style {
                background: if urgent {
                    urgent_color(theme, 0.45)
                } else if selected {
                    Some(Background::Color(
                        theme.cosmic().icon_button.selected_state_color().into(),
                    ))
                } else {
                    a.background
                },
                ..a
            }
        }),
        hovered: Box::new(move |focused, theme| {
            let h = button::Catalog::hovered(theme, focused, selected, &Button::AppletIcon);
            button::Style {
                background: if urgent { urgent_color(theme, 0.6) } else { h.background },
                ..h
            }
        }),
        disabled: Box::new(|theme| button::Catalog::disabled(theme, &Button::AppletIcon)),
        pressed: Box::new(move |focused, theme| {
            button::Catalog::pressed(theme, focused, selected, &Button::AppletIcon)
        }),
    }
}

#[inline]
pub fn menu_control_padding() -> Padding {
    let spacing = theme::spacing();
    [spacing.space_xxs, spacing.space_s].into()
}

fn find_desktop_entries<'a>(
    desktop_entries: &'a [fde::DesktopEntry],
    app_ids: &'a [String],
) -> impl Iterator<Item = fde::DesktopEntry> + 'a {
    app_ids.iter().map(|fav| {
        let unicase_fav = fde::unicase::Ascii::new(fav.as_str());
        fde::find_app_by_id(desktop_entries, unicase_fav).map_or_else(
            || fde::DesktopEntry::from_appid(fav.clone()),
            ToOwned::to_owned,
        )
    })
}

impl Taskbar {
    // Cache all desktop entries to use when new apps are added to the dock.
    fn update_desktop_entries(&mut self) {
        self.desktop_entries = fde::Iter::new(fde::default_paths())
            .filter_map(|p| fde::DesktopEntry::from_path(p, Some(&self.locales)).ok())
            .collect::<Vec<_>>();
    }

    fn is_on_current_monitor_and_workspace(&self, toplevel_info: &ToplevelInfo) -> bool {
        let on_active_workspace = self.active_workspaces.is_empty()
            || toplevel_info.workspace.is_empty()
            || self
                .active_workspaces
                .iter()
                .any(|workspace| toplevel_info.workspace.contains(workspace));

        match &self.config.filter_top_levels {
            None => true,
            Some(ToplevelFilter::ActiveWorkspace) => on_active_workspace,
            Some(ToplevelFilter::ConfiguredOutput) => {
                let on_active_output = self
                    .output_list
                    .iter()
                    .find(|(_, info)| info.name.as_ref() == Some(&self.core.applet.output_name))
                    .map_or(true, |(active_output, _)| {
                        toplevel_info
                            .output
                            .iter()
                            .any(|output| output == active_output)
                    });

                on_active_output && on_active_workspace
            }
        }
    }

    // Update pinned items using the cached desktop entries as a source.
    fn update_pinned_list(&mut self) {
        self.pinned_list = find_desktop_entries(&self.desktop_entries, &self.config.favorites)
            .zip(&self.config.favorites)
            .enumerate()
            .map(|(pinned_ctr, (e, original_id))| DockItem {
                id: pinned_ctr as u32,
                toplevels: Vec::new(),
                desktop_info: e,
                original_app_id: original_id.clone(),
            })
            .collect();
    }

    /// Close any open popups.
    fn close_popups(&mut self) -> Task<cosmic::Action<Message>> {
        let mut commands = Vec::new();
        if let Some(popup) = self.popup.take() {
            commands.push(destroy_popup(popup.id));
        }
        if let Some(popup) = self.overflow_active_popup.take() {
            commands.push(destroy_popup(popup));
        }
        if let Some(popup) = self.overflow_favorites_popup.take() {
            commands.push(destroy_popup(popup));
        }
        Task::batch(commands)
    }

    /// Returns the length of the group in the favorite list after which items are displayed in a popup.
    /// Shrink the favorite list until it only has active windows, or until it fits in the length provided.
    fn panel_overflow_lengths(&self) -> (Option<usize>, Option<usize>) {
        let mut favorite_index;
        let mut active_index = None;
        let Some(mut max_major_axis_len) = self.core.applet.suggested_bounds.as_ref().map(|c| {
            // if we have a configure for width and height, we're in a overflow popup
            match self.core.applet.anchor {
                PanelAnchor::Top | PanelAnchor::Bottom => c.width as u32,
                PanelAnchor::Left | PanelAnchor::Right => c.height as u32,
            }
        }) else {
            return (None, active_index);
        };
        // tracing::error!("{} {}", max_major_axis_len, self.pinned_list.len());
        // subtract the divider width
        max_major_axis_len -= 1;
        let applet_icon = AppletIconData::new(&self.core.applet);

        let button_total_size = self.core.applet.suggested_size(true).0
            + self.core.applet.suggested_padding(true).0 * 2
            + applet_icon.icon_spacing as u16;

        let favorite_active_cnt = self
            .pinned_list
            .iter()
            .filter(|t| !t.toplevels.is_empty())
            .count();

        // initial calculation of favorite_index
        let btn_count = max_major_axis_len / button_total_size as u32;
        if btn_count >= self.pinned_list.len() as u32 + self.active_list.len() as u32 {
            return (None, active_index);
        } else {
            favorite_index = (btn_count as usize).min(favorite_active_cnt).max(2);
        }

        // calculation of active_index based on favorite_index if there is still not enough space
        let active_index_max = (btn_count as i32)
            - (self.pinned_list.len() as i32).saturating_sub(favorite_index as i32);
        if active_index_max >= self.active_list.len() as i32 {
            active_index = Some(self.active_list.len());
        } else {
            active_index = Some((active_index_max.max(2) as usize).min(self.active_list.len()));
        }

        // final calculation of favorite_index if there is still not enough space
        if let Some(active_index) = active_index {
            let favorite_index_max = (btn_count as i32) - active_index as i32;
            favorite_index = favorite_index_max.max(2) as usize;
        } else {
            favorite_index = (btn_count as usize).min(self.pinned_list.len());
        }
        // tracing::error!("{} {} {:?}", btn_count, favorite_index, active_index);
        (Some(favorite_index), active_index)
    }

    fn currently_active_toplevel(&self) -> Vec<ExtForeignToplevelHandleV1> {
        if self.active_workspaces.is_empty() {
            return Vec::new();
        }
        let current_output = &self.core.applet.output_name;
        let mut focused_toplevels: Vec<ExtForeignToplevelHandleV1> = Vec::new();
        let active_workspaces = &self.active_workspaces;
        for toplevel_list in self.active_list.iter().chain(self.pinned_list.iter()) {
            for (t_info, _) in &toplevel_list.toplevels {
                if t_info.state.contains(&State::Activated)
                    && active_workspaces
                        .iter()
                        .any(|workspace| t_info.workspace.contains(workspace))
                    && t_info.output.iter().any(|x| {
                        self.output_list.get(x).is_some_and(|val| {
                            val.name.as_ref().is_some_and(|n| n == current_output)
                        })
                    })
                {
                    focused_toplevels.push(t_info.foreign_toplevel.clone());
                }
            }
        }
        focused_toplevels
    }

    fn find_desktop_entry_for_toplevel(
        &mut self,
        info: &ToplevelInfo,
        unicase_appid: Ascii<&str>,
    ) -> DesktopEntry {
        if let Some(appid) = fde::find_app_by_id(&self.desktop_entries, unicase_appid) {
            appid.clone()
        } else {
            // Update desktop entries in case it was not found.
            self.update_desktop_entries();
            if let Some(appid) = fde::find_app_by_id(&self.desktop_entries, unicase_appid) {
                appid.clone()
            } else {
                tracing::error!(id = info.app_id, "could not find desktop entry for app");
                let mut fallback_entry = fde::DesktopEntry::from_appid(info.app_id.clone());
                // proton opens games as steam_app_X, where X is either
                // the steam appid or "default". games with a steam appid
                // can have a desktop entry generated elsewhere; this
                // specifically handles non-steam games opened
                // under proton
                // in addition, try to match WINE entries who have its
                // appid = the full name of the executable (incl. .exe)
                let is_proton_game = info.app_id == "steam_app_default";
                if is_proton_game || info.app_id.ends_with(".exe") {
                    for entry in &self.desktop_entries {
                        let localised_name = entry.name(&self.locales).unwrap_or_default();
                        if localised_name == info.title {
                            // if this is a proton game, we only want
                            // to look for game entries
                            if is_proton_game
                                && !entry.categories().unwrap_or_default().contains(&"Game")
                            {
                                continue;
                            }
                            fallback_entry = entry.clone();
                            break;
                        }
                    }
                }
                fallback_entry
            }
        }
    }

    /// The app whose windows' previews show.
    fn preview_of(&self) -> Option<u32> {
        self.popup
            .as_ref()
            .filter(|p| p.popup_type == PopupType::Preview)
            .map(|p| p.dock_item.id)
    }

    /// Whether a right-click menu is open: the buttons wait for it.
    fn menu_open(&self) -> bool {
        self.popup
            .as_ref()
            .is_some_and(|p| p.popup_type != PopupType::Preview)
    }

    fn item(&self, id: u32) -> Option<&DockItem> {
        self.active_list
            .iter()
            .chain(self.pinned_list.iter())
            .find(|t| t.id == id)
    }

    fn has_windows(&self, id: u32) -> bool {
        self.item(id).is_some_and(|item| {
            item.toplevels
                .iter()
                .any(|(info, _)| self.is_on_current_monitor_and_workspace(info))
        })
    }

    /// Asks for a new picture of each of the app's windows.
    fn capture_windows(&self, id: u32) {
        let (Some(item), Some(tx)) = (self.item(id), self.wayland_sender.as_ref()) else {
            return;
        };
        for (info, _) in &item.toplevels {
            if self.is_on_current_monitor_and_workspace(info) {
                let _ = tx.send(WaylandRequest::Screencopy(info.foreign_toplevel.clone()));
            }
        }
    }

    fn close_preview(&mut self) -> Task<cosmic::Action<Message>> {
        self.preview_hovered = false;
        self.hovered_toplevel = None;
        match self.popup.take() {
            Some(p) if p.popup_type == PopupType::Preview => destroy_popup(p.id),
            other => {
                self.popup = other;
                Task::none()
            }
        }
    }

    /// The app's windows, in a row of pictures over its button. Like the
    /// tooltips, it takes no keyboard and no grab: the pointer can go on
    /// to the next button, or into it to pick or close a window.
    fn open_preview(&mut self, id: u32, parent: window::Id) -> Task<cosmic::Action<Message>> {
        let Some(item) = self.item(id).cloned() else {
            return Task::none();
        };
        let Some(rectangle) = self.rectangles.get(&id.into()).copied() else {
            return Task::none();
        };
        self.capture_windows(id);
        let new_id = window::Id::unique();
        self.preview_hovered = false;
        self.popup = Some(Popup {
            parent,
            id: new_id,
            dock_item: item,
            popup_type: PopupType::Preview,
        });
        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
            |_| Default::default(),
            move |app: &mut Self| {
                let mut popup_settings =
                    app.core
                        .applet
                        .get_popup_settings(parent, new_id, None, None, None);
                popup_settings.grab = false;
                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                    x: rectangle.x as i32,
                    y: rectangle.y as i32,
                    width: rectangle.width as i32,
                    height: rectangle.height as i32,
                };
                let max_windows = 7.0;
                let window_spacing = 8.0;
                popup_settings.positioner.size_limits = match app.core.applet.anchor {
                    PanelAnchor::Right | PanelAnchor::Left => Limits::NONE
                        .min_width(100.0)
                        .min_height(30.0)
                        .max_width(window_spacing * 2.0 + TOPLEVEL_BUTTON_WIDTH)
                        .max_height(
                            TOPLEVEL_BUTTON_HEIGHT * max_windows
                                + window_spacing * (max_windows + 1.0),
                        ),
                    PanelAnchor::Bottom | PanelAnchor::Top => Limits::NONE
                        .min_width(30.0)
                        .min_height(100.0)
                        .max_width(
                            TOPLEVEL_BUTTON_WIDTH * max_windows
                                + window_spacing * (max_windows + 1.0),
                        )
                        .max_height(window_spacing * 2.0 + TOPLEVEL_BUTTON_HEIGHT),
                };
                popup_settings
            },
            None,
        ))
    }

    fn preview_on_start(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(app_id) = self.preview_on_start.as_deref() else {
            return Task::none();
        };
        let found = self
            .active_list
            .iter()
            .chain(&self.pinned_list)
            .find(|i| i.desktop_info.id() == app_id)
            .map(|i| i.id);
        match (found, self.core.main_window_id()) {
            (Some(id), Some(main))
                if self.has_windows(id) && self.rectangles.contains_key(&id.into()) =>
            {
                self.preview_on_start = None;
                self.open_preview(id, main)
            }
            _ => Task::none(),
        }
    }

    /// An app's button: the windows' previews on the pointer when it has
    /// windows open, its name when it hasn't, as on Windows.
    fn item_button<'a>(
        &'a self,
        dock_item: &'a DockItem,
        window_id: window::Id,
        focused_item: &[ExtForeignToplevelHandleV1],
        tracked: bool,
        interaction_enabled: bool,
    ) -> Element<'a, Message> {
        let filter = |info: &ToplevelInfo| self.is_on_current_monitor_and_workspace(info);
        let is_focused = dock_item
            .toplevels
            .iter()
            .filter(|(info, _)| filter(info))
            .any(|y| focused_item.contains(&y.0.foreign_toplevel));
        let has_windows = dock_item.toplevels.iter().any(|(info, _)| filter(info));
        let badge = self.badges.get(dock_item.desktop_info.id());
        let button = dock_item.as_icon(
            &self.core.applet,
            if tracked {
                self.rectangle_tracker.as_ref()
            } else {
                None
            },
            interaction_enabled,
            self.config.enable_drag_source,
            self.gpus.as_deref(),
            is_focused,
            &badge,
            window_id,
            Some(&filter),
        );
        if has_windows || !interaction_enabled {
            return button;
        }
        let parent = (self.core.main_window_id() != Some(window_id)).then_some(window_id);
        self.core
            .applet
            .applet_tooltip::<Message>(
                button,
                dock_item
                    .desktop_info
                    .full_name(&self.locales)
                    .unwrap_or_default()
                    .into_owned(),
                self.popup.is_some(),
                Message::Surface,
                parent,
            )
            .into()
    }

    // Check if a specific toplevel is focused
    fn is_focused(&self, handle: &ExtForeignToplevelHandleV1) -> bool {
        self.currently_active_toplevel().contains(handle)
    }

    // Check if a specific toplevel button is currently hovered
    fn is_hovered(&self, handle: &ExtForeignToplevelHandleV1) -> bool {
        self.hovered_toplevel.as_ref() == Some(handle)
    }
}

impl cosmic::Application for Taskbar {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &'static str = crate::APP_ID;

    fn init(core: cosmic::app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let config = Config::new(LIST_ID, AppListConfig::VERSION)
            .ok()
            .and_then(|c| AppListConfig::get_entry(&c).ok())
            .unwrap_or_default();

        let mut app_list = Self {
            core,
            config,
            locales: get_languages_from_env(),
            preview_on_start: std::env::var("COSMIC_TASKBAR_PREVIEW").ok(),
            ..Default::default()
        };

        app_list.update_desktop_entries();
        app_list.update_pinned_list();

        app_list.item_ctr = app_list.pinned_list.len() as u32;

        (
            app_list,
            Task::perform(try_get_gpus(), |gpus| {
                cosmic::Action::App(Message::GpuRequest(gpus))
            }),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::Popup(id, parent_window_id) => {
                let mut closing = Task::none();
                if self.preview_of().is_some() {
                    closing = self.close_preview();
                } else if let Some(Popup {
                    parent,
                    id: popup_id,
                    ..
                }) = self.popup.take()
                {
                    if parent == parent_window_id {
                        return destroy_popup(popup_id);
                    } else {
                        self.overflow_active_popup = None;
                        self.overflow_favorites_popup = None;
                        return Task::batch([destroy_popup(popup_id), destroy_popup(parent)]);
                    }
                }
                if let Some(toplevel_group) = self
                    .active_list
                    .iter()
                    .chain(self.pinned_list.iter())
                    .find(|t| t.id == id)
                    .cloned()
                {
                    let Some(rectangle) = self.rectangles.get(&toplevel_group.id.into()).copied()
                    else {
                        tracing::error!("No rectangle found for toplevel group");
                        return Task::none();
                    };
                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            move |_| LiveSettings::default(),
                            move |app: &mut Self| {
                                let new_id = window::Id::unique();
                                app.popup = Some(Popup {
                                    parent: parent_window_id,
                                    id: new_id,
                                    dock_item: toplevel_group.clone(),
                                    popup_type: PopupType::RightClickMenu,
                                });

                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    parent_window_id,
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );
                                let iced::Rectangle {
                                    x,
                                    y,
                                    width,
                                    height,
                                } = rectangle;
                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };
                                popup_settings
                            },
                            None,
                        ));

                    let gpu_update = Task::perform(try_get_gpus(), |gpus| {
                        cosmic::Action::App(Message::GpuRequest(gpus))
                    });
                    return Task::batch([closing, gpu_update, popup_task]);
                }
            }
            Message::GroupClick(id, parent_window_id) => {
                // The windows are already showing from the pointer: the click
                // keeps them there.
                if self.preview_of() == Some(id) {
                    return Task::none();
                }
                let close = self.close_preview();
                return Task::batch([close, self.open_preview(id, parent_window_id)]);
            }
            Message::IconHover(id, parent_window_id, entering) => {
                self.hover_gen += 1;
                let hover_gen = self.hover_gen;
                if entering {
                    self.hover = Some((id, parent_window_id));
                    // From one button to the next with the previews up, they
                    // follow at once, as on Windows.
                    if let Some(shown) = self.preview_of() {
                        if shown == id {
                            return Task::none();
                        }
                        let close = self.close_preview();
                        if self.has_windows(id) {
                            return Task::batch([close, self.open_preview(id, parent_window_id)]);
                        }
                        return close;
                    }
                    return after(HOVER_OPEN, Message::HoverSettled(hover_gen));
                }
                if self.hover.is_some_and(|(hovered, _)| hovered == id) {
                    self.hover = None;
                }
                return after(HOVER_CLOSE, Message::HoverSettled(hover_gen));
            }
            Message::PreviewHover(entering) => {
                self.hover_gen += 1;
                self.preview_hovered = entering;
                if !entering {
                    self.hovered_toplevel = None;
                    return after(HOVER_CLOSE, Message::HoverSettled(self.hover_gen));
                }
            }
            Message::PointerLeft(id) => {
                // Out of a surface fast, past its edge, its widgets may keep
                // the pointer's last place inside them and never see it
                // leave: the surface's own event says it.
                if self.popup.as_ref().is_some_and(|p| p.id == id && p.popup_type == PopupType::Preview) {
                    return self.update(Message::PreviewHover(false));
                }
                if self.hover.is_some_and(|(_, parent)| parent == id) {
                    self.hover = None;
                    self.hover_gen += 1;
                    return after(HOVER_CLOSE, Message::HoverSettled(self.hover_gen));
                }
            }
            Message::HoverSettled(hover_gen) => {
                if hover_gen != self.hover_gen
                    || (self.preview_hovered && self.preview_of().is_some())
                {
                    return Task::none();
                }
                // A menu or a list that is open stays as it is.
                if self.menu_open() {
                    return Task::none();
                }
                match self.hover {
                    Some((id, parent)) if self.has_windows(id) => {
                        if self.preview_of() != Some(id) {
                            let close = self.close_preview();
                            return Task::batch([close, self.open_preview(id, parent)]);
                        }
                    }
                    _ => return self.close_preview(),
                }
            }
            Message::RefreshPreview => {
                if let Some(id) = self.preview_of() {
                    self.capture_windows(id);
                }
            }
            Message::Badge(update) => {
                self.badges.update(update);
            }
            Message::ToplevelHoverChanged(handle, entering) => {
                match (entering, &self.hovered_toplevel) {
                    (true, _) => self.hovered_toplevel = Some(handle),
                    // prevents race condition
                    (false, Some(h)) if h == &handle => self.hovered_toplevel = None,
                    _ => {}
                }
            }
            Message::PinApp(id) => {
                if let Some(i) = self.active_list.iter().position(|t| t.id == id) {
                    let entry = self.active_list.remove(i);
                    self.config.add_pinned(
                        entry.original_app_id.clone(),
                        &Config::new(LIST_ID, AppListConfig::VERSION).unwrap(),
                    );
                    self.pinned_list.push(entry);
                }
                if let Some(Popup { id: popup_id, .. }) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
            }
            Message::UnpinApp(id) => {
                if let Some(i) = self.pinned_list.iter().position(|t| t.id == id) {
                    let entry = self.pinned_list.remove(i);

                    self.config.remove_pinned(
                        &entry.original_app_id,
                        &Config::new(LIST_ID, AppListConfig::VERSION).unwrap(),
                    );

                    self.rectangles.remove(&entry.id.into());
                    if !entry.toplevels.is_empty() {
                        self.active_list.push(entry);
                    }
                }
                if let Some(Popup { id: popup_id, .. }) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
            }
            Message::Toggle(handle) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::Toplevel(if self.is_focused(&handle) {
                        ToplevelRequest::Minimize(handle)
                    } else {
                        ToplevelRequest::Activate(handle)
                    }));
                }
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p.id);
                }
            }
            Message::CloseToplevel(handle) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::Toplevel(ToplevelRequest::Quit(handle)));
                }
            }
            Message::Quit(id) => {
                if let Some(toplevel_group) = self
                    .active_list
                    .iter()
                    .chain(self.pinned_list.iter())
                    .find(|t| t.desktop_info.id() == id)
                {
                    for (info, _) in &toplevel_group.toplevels {
                        if let Some(tx) = self.wayland_sender.as_ref() {
                            let _ = tx.send(WaylandRequest::Toplevel(ToplevelRequest::Quit(
                                info.foreign_toplevel.clone(),
                            )));
                        }
                    }
                }
                if let Some(Popup { id: popup_id, .. }) = self.popup.take() {
                    return destroy_popup(popup_id);
                }
            }
            Message::StartDrag(id) => {
                if let Some((_, toplevel_group, pos)) = self
                    .active_list
                    .iter()
                    .find_map(|t| {
                        if t.id == id {
                            Some((false, t.clone(), None))
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        self.pinned_list
                            .iter()
                            .position(|t| t.id == id)
                            .map(|pos| (true, self.pinned_list[pos].clone(), Some(pos)))
                    })
                {
                    let icon_id = window::Id::unique();
                    self.dnd_source =
                        Some((icon_id, toplevel_group.clone(), DndAction::empty(), pos));
                }
            }
            Message::DragFinished => {
                if let Some((_, mut toplevel_group, _, _pinned_pos)) = self.dnd_source.take() {
                    if self.dnd_offer.take().is_some() {
                        if let Some((_, toplevel_group, _, pinned_pos)) = self.dnd_source.as_ref() {
                            let mut pos = 0;
                            self.pinned_list.retain_mut(|pinned| {
                                let matched_id =
                                    pinned.desktop_info.id() == toplevel_group.desktop_info.id();
                                let pinned_match =
                                    pinned_pos.is_some_and(|pinned_pos| pinned_pos == pos);
                                let ret = !matched_id || pinned_match;

                                pos += 1;
                                ret
                            });
                        }
                    }

                    if !self
                        .pinned_list
                        .iter()
                        .chain(self.active_list.iter())
                        .any(|t| t.desktop_info.id() == toplevel_group.desktop_info.id())
                        && !toplevel_group.toplevels.is_empty()
                    {
                        self.item_ctr += 1;
                        toplevel_group.id = self.item_ctr;
                        self.active_list.push(toplevel_group);
                    }
                }
            }
            Message::DndEnter(x, y) => {
                let item_size = self.core.applet.suggested_size(false).0
                    + 2 * self.core.applet.suggested_padding(false).0;
                let pos_in_list = match self.core.applet.anchor {
                    PanelAnchor::Top | PanelAnchor::Bottom => x as f32,
                    PanelAnchor::Left | PanelAnchor::Right => y as f32,
                };
                let num_pinned = self.pinned_list.len();
                let index = index_in_list(num_pinned, item_size as f32, 4.0, None, pos_in_list);
                self.dnd_offer = Some(DndOffer {
                    preview_index: index,
                    ..DndOffer::default()
                });
                if let Some(dnd_source) = self.dnd_source.as_ref() {
                    self.dnd_offer.as_mut().unwrap().dock_item = Some(dnd_source.1.clone());
                } else {
                    // TODO dnd
                    return peek_dnd::<DndPathBuf>()
                        .map(Message::DndData)
                        .map(cosmic::Action::App);
                }
            }
            Message::DndMotion(x, y) => {
                let item_size = self.core.applet.suggested_size(false).0
                    + 2 * self.core.applet.suggested_padding(false).0;
                let pos_in_list = match self.core.applet.anchor {
                    PanelAnchor::Top | PanelAnchor::Bottom => x as f32,
                    PanelAnchor::Left | PanelAnchor::Right => y as f32,
                };
                let num_pinned = self.pinned_list.len();
                let index = index_in_list(
                    num_pinned,
                    item_size as f32,
                    4.0,
                    self.dnd_offer.as_ref().map(|o| o.preview_index),
                    pos_in_list,
                );
                if let Some(o) = self.dnd_offer.as_mut() {
                    o.preview_index = index;
                }
            }
            Message::DndLeave => {
                if let Some((_, toplevel_group, _, pinned_pos)) = self.dnd_source.as_ref() {
                    let mut pos = 0;
                    self.pinned_list.retain_mut(|pinned| {
                        let matched_id =
                            pinned.desktop_info.id() == toplevel_group.desktop_info.id();
                        let pinned_match = pinned_pos.is_some_and(|pinned_pos| pinned_pos == pos);
                        let ret = !matched_id || pinned_match;

                        pos += 1;
                        ret
                    });
                }
                self.dnd_offer = None;
            }
            Message::DndData(file_path) => {
                let Some(file_path) = file_path else {
                    tracing::error!("Couldn't peek at hovered path.");
                    return Task::none();
                };
                if let Some(DndOffer { dock_item, .. }) = self.dnd_offer.as_mut() {
                    if let Ok(de) = fde::DesktopEntry::from_path(file_path.0, Some(&self.locales)) {
                        self.item_ctr += 1;
                        *dock_item = Some(DockItem {
                            id: self.item_ctr,
                            toplevels: Vec::new(),
                            original_app_id: de.id().to_string(),
                            desktop_info: de,
                        });
                    }
                }
            }
            Message::DndDropFinished => {
                // we actually should have the data already, if not, we probably shouldn't do
                // anything anyway
                if let Some((mut dock_item, index)) = self
                    .dnd_offer
                    .take()
                    .and_then(|o| o.dock_item.map(|i| (i, o.preview_index)))
                {
                    self.item_ctr += 1;

                    if let Some((pos, is_pinned)) = self
                        .active_list
                        .iter()
                        .position(|de| de.original_app_id == dock_item.original_app_id)
                        .map(|pos| (pos, false))
                        .or_else(|| {
                            self.pinned_list
                                .iter()
                                .position(|de| de.original_app_id == dock_item.original_app_id)
                                .map(|pos| (pos, true))
                        })
                    {
                        let t = if is_pinned {
                            let t = self.pinned_list.remove(pos);
                            self.config.remove_pinned(
                                &t.original_app_id,
                                &Config::new(LIST_ID, AppListConfig::VERSION).unwrap(),
                            );
                            t
                        } else {
                            self.active_list.remove(pos)
                        };
                        dock_item.toplevels = t.toplevels;
                    }
                    dock_item.id = self.item_ctr;

                    if dock_item.desktop_info.exec().is_some() {
                        self.pinned_list
                            .insert(index.min(self.pinned_list.len()), dock_item);
                        self.config.update_pinned(
                            self.pinned_list
                                .iter()
                                .map(|dock_item| dock_item.original_app_id.clone())
                                .collect(),
                            &Config::new(LIST_ID, AppListConfig::VERSION).unwrap(),
                        );
                    }
                }
            }
            Message::Wayland(event) => {
                match event {
                    WaylandUpdate::Init(tx) => {
                        self.wayland_sender.replace(tx);
                    }
                    WaylandUpdate::Image(handle, img) => {
                        'img_update: for x in self
                            .active_list
                            .iter_mut()
                            .chain(self.pinned_list.iter_mut())
                        {
                            if let Some((_, handle_img)) = x
                                .toplevels
                                .iter_mut()
                                .find(|(info, _)| info.foreign_toplevel == handle)
                            {
                                *handle_img = Some(img);
                                break 'img_update;
                            }
                        }
                    }
                    WaylandUpdate::Finished => {
                        for t in &mut self.pinned_list {
                            t.toplevels.clear();
                        }
                        self.active_list.clear();
                        let subscription_ctr = self.subscription_ctr;
                        let rand_d = fastrand::u64(0..100);
                        return iced::Task::perform(
                            async move {
                                if let Some(millis) = 2u64
                                    .checked_pow(subscription_ctr)
                                    .and_then(|d| d.checked_add(rand_d))
                                {
                                    sleep(Duration::from_millis(millis)).await;
                                } else {
                                    pending::<()>().await;
                                }
                            },
                            |()| Message::IncrementSubscriptionCtr,
                        )
                        .map(cosmic::action::app);
                    }
                    WaylandUpdate::Toplevel(event) => match event {
                        ToplevelUpdate::Add(mut info) => {
                            let unicase_appid = fde::unicase::Ascii::new(&*info.app_id);
                            let new_desktop_info =
                                self.find_desktop_entry_for_toplevel(&info, unicase_appid);

                            if let Some(t) = self
                                .active_list
                                .iter_mut()
                                .chain(self.pinned_list.iter_mut())
                                .find(|DockItem { desktop_info, .. }| {
                                    desktop_info.id() == new_desktop_info.id()
                                })
                            {
                                t.toplevels.push((info, None));
                            } else {
                                if info.app_id.is_empty() {
                                    info.app_id = format!("Unknown Application {}", self.item_ctr);
                                }
                                self.item_ctr += 1;

                                self.active_list.push(DockItem {
                                    id: self.item_ctr,
                                    original_app_id: info.app_id.clone(),
                                    toplevels: vec![(info, None)],
                                    desktop_info: new_desktop_info,
                                });
                            }
                            return self.preview_on_start();
                        }
                        ToplevelUpdate::Remove(handle) => {
                            for t in self
                                .active_list
                                .iter_mut()
                                .chain(self.pinned_list.iter_mut())
                            {
                                t.toplevels
                                    .retain(|(info, _)| info.foreign_toplevel != handle);
                            }
                            self.active_list.retain(|t| !t.toplevels.is_empty());

                            if let Some(popup) = &mut self.popup
                                && popup.popup_type == PopupType::Preview
                            {
                                popup
                                    .dock_item
                                    .toplevels
                                    .retain(|(info, _)| info.foreign_toplevel != handle);

                                if popup.dock_item.toplevels.is_empty() {
                                    let id = popup.id;
                                    self.popup = None;
                                    return destroy_popup(id);
                                }
                            }
                        }
                        ToplevelUpdate::Update(info) => {
                            // TODO probably want to make sure it is removed
                            if info.app_id.is_empty() {
                                return Task::none();
                            }
                            // A window brought to the front elsewhere, as by a
                            // click on it, puts the previews away.
                            let was_active = self.active_list.iter().chain(&self.pinned_list).any(|item| {
                                item.toplevels.iter().any(|(t, _)| {
                                    t.foreign_toplevel == info.foreign_toplevel
                                        && t.state.contains(&State::Activated)
                                })
                            });
                            let mut closing = Task::none();
                            if info.state.contains(&State::Activated)
                                && !was_active
                                && !self.preview_hovered
                                && self.preview_of().is_some()
                            {
                                closing = self.close_preview();
                            }
                            if info.state.contains(&State::Activated) {
                                if let Some(item) = self.active_list.iter().chain(&self.pinned_list).find(
                                    |item| item.toplevels.iter().any(|(t, _)| t.foreign_toplevel == info.foreign_toplevel),
                                ) {
                                    let app_id = item.desktop_info.id().to_string();
                                    self.badges.seen(&app_id);
                                }
                            }
                            let mut updated_appid = false;

                            'toplevel_loop: for toplevel_list in self
                                .active_list
                                .iter_mut()
                                .chain(self.pinned_list.iter_mut())
                            {
                                for (t_info, _) in &mut toplevel_list.toplevels {
                                    if info.foreign_toplevel == t_info.foreign_toplevel {
                                        if info.app_id != t_info.app_id {
                                            updated_appid = true;
                                        }

                                        *t_info = info.clone();
                                        break 'toplevel_loop;
                                    }
                                }
                            }

                            if updated_appid {
                                // remove the current toplevel from its dock item
                                for t in self
                                    .active_list
                                    .iter_mut()
                                    .chain(self.pinned_list.iter_mut())
                                {
                                    t.toplevels
                                        .retain(|(t_info, _)| t_info.app_id != info.app_id);
                                }
                                self.active_list.retain(|t| !t.toplevels.is_empty());

                                // find a new one for it
                                let new_desktop_entry = self.find_desktop_entry_for_toplevel(
                                    &info,
                                    Ascii::new(&info.app_id),
                                );

                                if let Some(t) = self
                                    .active_list
                                    .iter_mut()
                                    .chain(self.pinned_list.iter_mut())
                                    .find(|DockItem { desktop_info, .. }| {
                                        desktop_info.id() == new_desktop_entry.id()
                                    })
                                {
                                    t.toplevels.push((info, None));
                                } else {
                                    self.item_ctr += 1;

                                    self.active_list.push(DockItem {
                                        id: self.item_ctr,
                                        original_app_id: info.app_id.clone(),
                                        toplevels: vec![(info, None)],
                                        desktop_info: new_desktop_entry,
                                    });
                                }
                            }
                            return closing;
                        }
                    },
                    WaylandUpdate::Workspace(workspaces) => self.active_workspaces = workspaces,
                    WaylandUpdate::Output(event) => match event {
                        OutputUpdate::Add(output, info) => {
                            self.output_list.insert(output, info);
                        }
                        OutputUpdate::Update(output, info) => {
                            self.output_list.insert(output, info);
                        }
                        OutputUpdate::Remove(output) => {
                            self.output_list.remove(&output);
                        }
                    },
                    WaylandUpdate::ActivationToken {
                        token,
                        app_id,
                        exec,
                        gpu_idx,
                        terminal,
                    } => {
                        let mut envs = Vec::new();
                        if let Some(token) = token {
                            envs.push(("XDG_ACTIVATION_TOKEN".to_string(), token.clone()));
                            envs.push(("DESKTOP_STARTUP_ID".to_string(), token));
                        }
                        if let (Some(gpus), Some(idx)) = (self.gpus.as_ref(), gpu_idx) {
                            envs.extend(
                                gpus[idx]
                                    .environment
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                        }
                        tokio::spawn(async move {
                            cosmic::desktop::spawn_desktop_exec(
                                exec,
                                envs,
                                app_id.as_deref(),
                                terminal,
                            )
                            .await;
                        });
                    }
                }
            }
            Message::NewSeat(s) => {
                self.seat.replace(s);
            }
            Message::RemovedSeat => {
                self.seat.take();
            }
            Message::Exec(exec, gpu_idx, terminal) => {
                if let Some(tx) = self.wayland_sender.as_ref() {
                    let _ = tx.send(WaylandRequest::TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec,
                        gpu_idx,
                        terminal,
                    });
                }
            }
            Message::Rectangle(u) => match u {
                RectangleUpdate::Rectangle(r) => {
                    self.rectangles.insert(r.0, r.1);
                    return self.preview_on_start();
                }
                RectangleUpdate::Init(tracker) => {
                    self.rectangle_tracker.replace(tracker);
                }
            },
            Message::ClosePopup => {
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p.id);
                }
            }
            Message::StartListeningForDnd => {
                self.is_listening_for_dnd = true;
            }
            Message::StopListeningForDnd => {
                self.is_listening_for_dnd = false;
            }
            Message::IncrementSubscriptionCtr => {
                self.subscription_ctr += 1;
            }
            Message::ConfigUpdated(config) => {
                self.config = config;
                // drain to active list
                for item in self.pinned_list.drain(..) {
                    if !item.toplevels.is_empty() {
                        self.active_list.push(item);
                    }
                }

                // pull back configured items into the favorites list
                self.pinned_list =
                    find_desktop_entries(&self.desktop_entries, &self.config.favorites)
                        .zip(&self.config.favorites)
                        .map(|(de, original_id)| {
                            if let Some(p) = self
                                .active_list
                                .iter()
                                // match using heuristic id
                                .position(|dock_item| dock_item.desktop_info.id() == de.id())
                            {
                                let mut d = self.active_list.remove(p);
                                // but use the id from the config
                                d.original_app_id.clone_from(original_id);
                                d
                            } else {
                                self.item_ctr += 1;
                                DockItem {
                                    id: self.item_ctr,
                                    toplevels: Vec::new(),
                                    desktop_info: de.clone(),
                                    original_app_id: original_id.clone(),
                                }
                            }
                        })
                        .collect();
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.popup.as_ref().map(|p| p.id) {
                    self.popup = None;
                }
                if self.overflow_active_popup.is_some_and(|p| p == id) {
                    self.overflow_active_popup = None;
                }
                if self.overflow_favorites_popup.is_some_and(|p| p == id) {
                    self.overflow_favorites_popup = None;
                }
            }
            Message::GpuRequest(gpus) => {
                self.gpus = gpus;
            }
            Message::OpenActive => {
                let create_new = self.overflow_active_popup.is_none();
                let mut cmds = vec![self.close_popups()];

                // create a popup with the active list
                if create_new {
                    let new_id = window::Id::unique();
                    self.overflow_active_popup = Some(new_id);
                    let Some(iced::Rectangle {
                        x,
                        y,
                        width,
                        height,
                    }) = self.rectangles.get(&DockItemId::ActiveOverflow).copied()
                    else {
                        return Task::none();
                    };

                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            |_| Default::default(),
                            move |app: &mut Self| {
                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    app.core.main_window_id().unwrap(),
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );

                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };

                                let applet_suggested_size = app.core.applet.suggested_size(false).0
                                    + 2 * app.core.applet.suggested_padding(false).0;
                                let (_favorite_popup_cutoff, active_popup_cutoff) =
                                    app.panel_overflow_lengths();
                                let popup_applet_count = app.active_list.len().saturating_sub(
                                    (active_popup_cutoff.unwrap_or_default()).saturating_sub(1),
                                ) as f32;
                                let popup_applet_size = applet_suggested_size as f32
                                    * popup_applet_count
                                    + 4.0 * (popup_applet_count - 1.);
                                let (max_width, max_height) = match app.core.applet.anchor {
                                    PanelAnchor::Top | PanelAnchor::Bottom => {
                                        (popup_applet_size, applet_suggested_size as f32)
                                    }
                                    PanelAnchor::Left | PanelAnchor::Right => {
                                        (applet_suggested_size as f32, popup_applet_size)
                                    }
                                };
                                popup_settings.positioner.size_limits = Limits::NONE
                                    .max_width(max_width)
                                    .min_width(1.)
                                    .max_height(max_height)
                                    .min_height(1.);
                                popup_settings
                            },
                            None,
                        ));
                    cmds.push(popup_task);
                }
                return Task::batch(cmds);
            }
            Message::OpenFavorites => {
                let create_new = self.overflow_favorites_popup.is_none();
                let mut cmds = vec![self.close_popups()];

                // create a popup with the favorites list
                if create_new {
                    let new_id = window::Id::unique();
                    self.overflow_favorites_popup = Some(new_id);
                    let Some(iced::Rectangle {
                        x,
                        y,
                        width,
                        height,
                    }) = self.rectangles.get(&DockItemId::FavoritesOverflow).copied()
                    else {
                        return Task::none();
                    };

                    let popup_task =
                        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                            |_| Default::default(),
                            move |app: &mut Self| {
                                let mut popup_settings = app.core.applet.get_popup_settings(
                                    app.core.main_window_id().unwrap(),
                                    new_id,
                                    None,
                                    None,
                                    None,
                                );

                                popup_settings.positioner.anchor_rect = iced::Rectangle::<i32> {
                                    x: x as i32,
                                    y: y as i32,
                                    width: width as i32,
                                    height: height as i32,
                                };

                                let applet_suggested_size = app.core.applet.suggested_size(false).0
                                    + 2 * app.core.applet.suggested_padding(false).0;
                                let (favorite_popup_cutoff, _active_popup_cutoff) =
                                    app.panel_overflow_lengths();
                                let popup_applet_count = app.pinned_list.len().saturating_sub(
                                    favorite_popup_cutoff.unwrap_or_default().saturating_sub(1),
                                ) as f32;
                                let popup_applet_size = applet_suggested_size as f32
                                    * popup_applet_count
                                    + 4.0 * (popup_applet_count - 1.);
                                let (max_width, max_height) = match app.core.applet.anchor {
                                    PanelAnchor::Top | PanelAnchor::Bottom => {
                                        (popup_applet_size, applet_suggested_size as f32)
                                    }
                                    PanelAnchor::Left | PanelAnchor::Right => {
                                        (applet_suggested_size as f32, popup_applet_size)
                                    }
                                };
                                popup_settings.positioner.size_limits = Limits::NONE
                                    .max_width(max_width)
                                    .min_width(1.)
                                    .max_height(max_height)
                                    .min_height(1.);
                                popup_settings
                            },
                            None,
                        ));
                    cmds.push(popup_task);
                }
                return Task::batch(cmds);
            }
            Message::Pressed(id) => {
                if self.menu_open() && self.core.main_window_id() == Some(id) {
                    return self.close_popups();
                }
            }
            Message::Surface(a) => {
                return cosmic::task::message(cosmic::Action::Surface(a));
            }
        }

        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let focused_item = self.currently_active_toplevel();
        let app_icon = AppletIconData::new(&self.core.applet);
        let is_horizontal = match self.core.applet.anchor {
            PanelAnchor::Top | PanelAnchor::Bottom => true,
            PanelAnchor::Left | PanelAnchor::Right => false,
        };
        let (favorite_popup_cutoff, active_popup_cutoff) = self.panel_overflow_lengths();
        let mut favorite_to_remove = if let Some(cutoff) = favorite_popup_cutoff {
            if cutoff < self.pinned_list.len() {
                self.pinned_list.len() - cutoff + 1
            } else {
                0
            }
        } else {
            0
        };
        let favorites: Vec<_> = self
            .pinned_list
            .iter()
            .rev()
            .filter(|f| {
                if favorite_to_remove > 0 && f.toplevels.is_empty() {
                    favorite_to_remove -= 1;
                    false
                } else {
                    true
                }
            })
            .collect();
        let main_window = self.core.main_window_id().unwrap();
        let interaction_enabled = !self.menu_open();
        let mut favorites: Vec<_> = favorites[favorite_to_remove..]
            .iter()
            .rev()
            .map(|dock_item| {
                self.item_button(
                    dock_item,
                    main_window,
                    &focused_item,
                    true,
                    interaction_enabled,
                )
            })
            .collect();

        if favorite_popup_cutoff.is_some() {
            // button to show more favorites
            let icon = match self.core.applet.anchor {
                PanelAnchor::Bottom => "go-up-symbolic",
                PanelAnchor::Left => "go-next-symbolic",
                PanelAnchor::Right => "go-previous-symbolic",
                PanelAnchor::Top => "go-down-symbolic",
            };
            let btn = self
                .core
                .applet
                .icon_button(icon)
                .on_press(Message::OpenFavorites);
            let btn: Element<_> = if let Some(rectangle_tracker) = self.rectangle_tracker.as_ref() {
                rectangle_tracker
                    .container(DockItemId::FavoritesOverflow, btn)
                    .into()
            } else {
                btn.into()
            };
            favorites.push(btn);
        }

        if let Some((item, index)) = self
            .dnd_offer
            .as_ref()
            .and_then(|o| o.dock_item.as_ref().map(|item| (item, o.preview_index)))
        {
            favorites.insert(
                index.min(favorites.len()),
                self.item_button(item, main_window, &focused_item, false, false),
            );
        } else if self.is_listening_for_dnd && self.pinned_list.is_empty() {
            // show star indicating pinned_list is drag target
            favorites.push(
                container(
                    icon::from_name("starred-symbolic.symbolic")
                        .size(self.core.applet.suggested_size(false).0),
                )
                .padding(self.core.applet.suggested_padding(false).1) // TODO
                .into(),
            );
        }

        let filtered_active_list: Vec<_> = self
            .active_list
            .iter()
            .filter(|dock_item| {
                dock_item.toplevels.iter().any(|(toplevel_info, _)| {
                    self.is_on_current_monitor_and_workspace(toplevel_info)
                })
            })
            .collect();

        let mut active: Vec<_> =
            filtered_active_list[..active_popup_cutoff.map_or(filtered_active_list.len(), |n| {
                if n < filtered_active_list.len() {
                    n.saturating_sub(1)
                } else {
                    n
                }
            })]
                .iter()
                .map(|dock_item| {
                    self.item_button(
                        dock_item,
                        main_window,
                        &focused_item,
                        true,
                        interaction_enabled,
                    )
                })
                .collect();

        if active_popup_cutoff.is_some_and(|n| n < filtered_active_list.len()) {
            // button to show more active
            let icon = match self.core.applet.anchor {
                PanelAnchor::Bottom => "go-up-symbolic",
                PanelAnchor::Left => "go-next-symbolic",
                PanelAnchor::Right => "go-previous-symbolic",
                PanelAnchor::Top => "go-down-symbolic",
            };
            let btn = self
                .core
                .applet
                .icon_button(icon)
                .on_press(Message::OpenActive);
            let btn: Element<_> = if let Some(rectangle_tracker) = self.rectangle_tracker.as_ref() {
                rectangle_tracker
                    .container(DockItemId::ActiveOverflow, btn)
                    .into()
            } else {
                btn.into()
            };
            active.push(btn);
        }

        let window_size = self.core.applet.suggested_bounds.as_ref();
        let max_num = if self.core.applet.is_horizontal() {
            let suggested_width = self.core.applet.suggested_size(false).0
                + self.core.applet.suggested_padding(false).0 * 2;
            window_size
                .map(|w| w.width)
                .map_or(u32::MAX, |b| (b / suggested_width as f32) as u32) as usize
        } else {
            let suggested_height = self.core.applet.suggested_size(false).1
                + self.core.applet.suggested_padding(false).0 * 2;
            window_size
                .map(|w| w.height)
                .map_or(u32::MAX, |b| (b / suggested_height as f32) as u32) as usize
        }
        .max(4);
        if max_num < favorites.len() + active.len() {
            let active_leftover = max_num.saturating_sub(favorites.len());
            favorites.truncate(max_num - active_leftover);
            active.truncate(active_leftover);
        }
        let (w, h, favorites, active) = if is_horizontal {
            (
                Length::Shrink,
                Length::Shrink,
                DndDestination::for_data::<DndPathBuf>(
                    row(favorites).spacing(app_icon.icon_spacing),
                    |_, _| Message::DndDropFinished,
                )
                .drag_id(DND_FAVORITES),
                row(active).spacing(app_icon.icon_spacing).into(),
            )
        } else {
            (
                Length::Shrink,
                Length::Shrink,
                DndDestination::for_data(
                    column(favorites).spacing(app_icon.icon_spacing),
                    |_data: Option<DndPathBuf>, _| Message::DndDropFinished,
                )
                .drag_id(DND_FAVORITES),
                column(active).spacing(app_icon.icon_spacing).into(),
            )
        };

        let favorites = favorites
            .on_enter(|x, y, _| Message::DndEnter(x, y))
            .on_motion(Message::DndMotion)
            .on_leave(|| Message::DndLeave);

        let show_pinned =
            !self.pinned_list.is_empty() || self.dnd_offer.is_some() || self.is_listening_for_dnd;
        // One row, as on Windows: the pinned apps, then the others open.
        let content_list: Vec<Element<_>> = if show_pinned && !self.active_list.is_empty() {
            vec![favorites.into(), active]
        } else if show_pinned {
            vec![favorites.into()]
        } else if !self.active_list.is_empty() {
            vec![active]
        } else {
            vec![
                icon::from_name("com.system76.CosmicAppList")
                    .size(self.core.applet.suggested_size(false).0)
                    .into(),
            ]
        };

        let mut content = match &self.core.applet.anchor {
            PanelAnchor::Left | PanelAnchor::Right => container(
                Column::with_children(content_list)
                    .spacing(app_icon.icon_spacing)
                    .align_x(Alignment::Center)
                    .height(h)
                    .width(w),
            ),
            PanelAnchor::Top | PanelAnchor::Bottom => container(
                Row::with_children(content_list)
                    .spacing(app_icon.icon_spacing)
                    .align_y(Alignment::Center)
                    .height(h)
                    .width(w),
            ),
        };
        if self.active_list.is_empty() && self.pinned_list.is_empty() {
            let suggested_size = self.core.applet.suggested_size(false);
            content = content.width(suggested_size.0).height(suggested_size.1);
        }

        let mut limits = Limits::NONE.min_width(1.).min_height(1.);

        if let Some(b) = self.core.applet.suggested_bounds {
            if b.width as i32 > 0 {
                limits = limits.max_width(b.width);
            }
            if b.height as i32 > 0 {
                limits = limits.max_height(b.height);
            }
        }

        self.core
            .applet
            .autosize_window(content)
            .limits(limits)
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {

        if let Some((_, item, _, _)) = self.dnd_source.as_ref().filter(|s| s.0 == id) {
            cosmic::widget::icon(
                fde::IconSource::from_unknown(item.desktop_info.icon().unwrap_or_default())
                    .as_cosmic_icon(),
            )
            .size(self.core.applet.suggested_size(false).0)
            .into()
        } else if let Some(Popup {
            dock_item: DockItem { id, .. },
            popup_type,
            ..
        }) = self.popup.as_ref().filter(|p| id == p.id)
        {
            let (dock_item, is_pinned) = match self.pinned_list.iter().find(|i| i.id == *id) {
                Some(e) => (e, true),
                None => match self.active_list.iter().find(|i| i.id == *id) {
                    Some(e) => (e, false),
                    None => return text::body("").into(),
                },
            };

            // Filter toplevels to only show windows on current monitor and workspace
            let filtered_toplevels: Vec<_> = dock_item
                .toplevels
                .iter()
                .filter(|(toplevel_info, _)| {
                    self.is_on_current_monitor_and_workspace(toplevel_info)
                })
                .collect();

            let toplevels = &filtered_toplevels;
            let desktop_info = &dock_item.desktop_info;

            match popup_type {
                PopupType::RightClickMenu => {
                    // The jump list, in Windows' order: the app's own tasks,
                    // the app (a new window of it), pin, close.
                    fn menu_button<'a, Message: Clone + 'a>(
                        content: impl Into<Element<'a, Message>>,
                    ) -> cosmic::widget::Button<'a, Message> {
                        button::custom(content)
                            .height(20 + 2 * theme::spacing().space_xxs)
                            .class(Button::MenuItem)
                            .padding(menu_control_padding())
                            .width(Length::Fill)
                    }
                    fn with_icon<'a, Message: 'a>(
                        icon: Option<icon::Handle>,
                        label: impl Into<Cow<'a, str>>,
                    ) -> Element<'a, Message> {
                        let label = text::body(label.into())
                            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
                        match icon {
                            Some(icon) => row![icon::icon(icon).size(16), label]
                                .spacing(8)
                                .align_y(Alignment::Center)
                                .into(),
                            None => row![horizontal_space().width(Length::Fixed(16.0)), label]
                                .spacing(8)
                                .align_y(Alignment::Center)
                                .into(),
                        }
                    }
                    let t = texts();
                    let app_icon = fde::IconSource::from_unknown(
                        desktop_info.icon().unwrap_or_default(),
                    )
                    .as_cosmic_icon();
                    let app_name = desktop_info
                        .full_name(&self.locales)
                        .unwrap_or_default()
                        .into_owned();

                    let mut content =
                        menu::menu_column::MenuColumn::with_capacity(6).align_x(Alignment::Center);

                    let mut tasks = 0;
                    for action in desktop_info.actions().into_iter().flatten() {
                        let Some(exec) = desktop_info.action_entry(action, "Exec") else {
                            continue;
                        };
                        let Some(name) =
                            desktop_info.action_entry_localized(action, "Name", &self.locales)
                        else {
                            continue;
                        };
                        let action_icon = desktop_info
                            .action_entry(action, "Icon")
                            .map(|i| fde::IconSource::from_unknown(i).as_cosmic_icon());
                        content = content.push(menu_button(with_icon(action_icon, name)).on_press(
                            Message::Exec(exec.into(), None, desktop_info.terminal()),
                        ));
                        tasks += 1;
                    }
                    if tasks > 0 {
                        content = content.push(divider::horizontal::light());
                    }

                    if let Some(exec) = desktop_info.exec() {
                        match self.gpus.as_ref().filter(|_| toplevels.is_empty()) {
                            Some(gpus) => {
                                let default_idx = preferred_gpu_idx(desktop_info, gpus.iter());
                                for (i, gpu) in gpus.iter().enumerate() {
                                    let mut label = t.run_on.replace("{}", &gpu.name);
                                    if i == default_idx {
                                        label = format!("{label} {}", t.default_gpu);
                                    }
                                    content = content.push(
                                        menu_button(with_icon(Some(app_icon.clone()), label))
                                            .on_press(Message::Exec(
                                                exec.to_string(),
                                                Some(i),
                                                desktop_info.terminal(),
                                            )),
                                    );
                                }
                            }
                            None => {
                                content = content.push(
                                    menu_button(with_icon(Some(app_icon.clone()), app_name))
                                        .on_press(Message::Exec(
                                            exec.to_string(),
                                            None,
                                            desktop_info.terminal(),
                                        )),
                                );
                            }
                        }
                    }

                    content = content.push(
                        menu_button(with_icon(
                            Some(from_name("view-pin-symbolic").into()),
                            if is_pinned { t.unpin } else { t.pin },
                        ))
                        .on_press(if is_pinned {
                            Message::UnpinApp(*id)
                        } else {
                            Message::PinApp(*id)
                        }),
                    );

                    if !toplevels.is_empty() {
                        content = content.push(
                            menu_button(with_icon(
                                Some(from_name("window-close-symbolic").into()),
                                if toplevels.len() == 1 {
                                    t.close_window
                                } else {
                                    t.close_all
                                },
                            ))
                            .on_press(Message::Quit(desktop_info.id().to_string())),
                        );
                    }
                    self.core
                        .applet
                        .popup_container(
                            container(content)
                                .padding(1)
                                .height(Length::Shrink)
                                .width(Length::Fill),
                        )
                        .limits(
                            Limits::NONE
                                .min_width(1.)
                                .min_height(1.)
                                .max_width(300.)
                                .max_height(1000.),
                        )
                        .into()
                }
                PopupType::Preview => {
                    let app_icon = fde::IconSource::from_unknown(
                        desktop_info.icon().unwrap_or_default(),
                    )
                    .as_cosmic_icon();
                    let cards = toplevels.iter().map(|(info, img)| {
                        toplevel_button(
                            img.as_ref(),
                            app_icon.clone(),
                            info.title.clone(),
                            info.foreign_toplevel.clone(),
                            self.is_focused(&info.foreign_toplevel),
                            self.is_hovered(&info.foreign_toplevel),
                        )
                    });
                    let content: Element<_> = if self.core.applet.is_horizontal() {
                        Row::with_children(cards)
                            .padding(8)
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .into()
                    } else {
                        Column::with_children(cards)
                            .padding(8)
                            .spacing(8)
                            .align_x(Alignment::Center)
                            .into()
                    };
                    mouse_area(
                        self.core
                            .applet
                            .popup_container(content)
                            .limits(Limits::NONE.min_width(1.).min_height(1.).max_height(1200.)),
                    )
                    .on_enter(Message::PreviewHover(true))
                    .on_exit(Message::PreviewHover(false))
                    .into()
                }
            }
        } else if self
            .overflow_active_popup
            .as_ref()
            .is_some_and(|overflow_id| overflow_id == &id)
        {
            let (_favorite_popup_cutoff, active_popup_cutoff) = self.panel_overflow_lengths();

            let focused_item = self.currently_active_toplevel();

            let filtered_active_list: Vec<_> = self
                .active_list
                .iter()
                .filter(|dock_item| {
                    dock_item.toplevels.iter().any(|(toplevel_info, _)| {
                        self.is_on_current_monitor_and_workspace(toplevel_info)
                    })
                })
                .collect();

            let active: Vec<_> = filtered_active_list
                .iter()
                .rev()
                .take(active_popup_cutoff.map_or(filtered_active_list.len(), |n| {
                    if n < filtered_active_list.len() {
                        filtered_active_list.len() - n + 1
                    } else {
                        0
                    }
                }))
                .map(|dock_item| {
                    self.item_button(dock_item, id, &focused_item, true, !self.menu_open())
                })
                .collect();
            let content = match &self.core.applet.anchor {
                PanelAnchor::Left | PanelAnchor::Right => container(
                    Column::with_children(active)
                        .spacing(4.0)
                        .align_x(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
                PanelAnchor::Top | PanelAnchor::Bottom => container(
                    Row::with_children(active)
                        .spacing(4.0)
                        .align_y(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
            };
            // send clear popup on press content if there is an active popup
            let content: Element<_> = if self.popup.is_some() {
                mouse_area(content)
                    .on_release(Message::ClosePopup)
                    .on_right_release(Message::ClosePopup)
                    .into()
            } else {
                content.into()
            };
            self.core
                .applet
                .popup_container(content)
                .limits(
                    Limits::NONE
                        .min_width(1.)
                        .min_height(1.)
                        .max_width(1920.)
                        .max_height(1000.),
                )
                .into()
        } else if self
            .overflow_favorites_popup
            .as_ref()
            .is_some_and(|popup_id| popup_id == &id)
        {
            let (favorite_popup_cutoff, _active_popup_cutoff) = self.panel_overflow_lengths();

            let focused_item = self.currently_active_toplevel();
            // show the overflow popup for favorites list

            let mut favorite_to_remove = if let Some(cutoff) = favorite_popup_cutoff {
                if cutoff < self.pinned_list.len() {
                    self.pinned_list.len() - cutoff + 1
                } else {
                    0
                }
            } else {
                0
            };
            let mut favorites_extra = Vec::with_capacity(favorite_to_remove);
            let mut favorites: Vec<_> = self
                .pinned_list
                .iter()
                .rev()
                .filter(|f| {
                    if favorite_to_remove > 0 && f.toplevels.is_empty() {
                        favorite_to_remove -= 1;
                        true
                    } else {
                        favorites_extra.push(*f);
                        false
                    }
                })
                .collect();
            favorites.extend(favorites_extra[..favorite_to_remove].iter().copied());
            let favorites: Vec<_> = favorites
                .iter()
                .rev()
                .map(|dock_item| {
                    self.item_button(dock_item, id, &focused_item, true, !self.menu_open())
                })
                .collect();
            let content = match &self.core.applet.anchor {
                PanelAnchor::Left | PanelAnchor::Right => container(
                    Column::with_children(favorites)
                        .spacing(4.0)
                        .align_x(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
                PanelAnchor::Top | PanelAnchor::Bottom => container(
                    Row::with_children(favorites)
                        .spacing(4.0)
                        .align_y(Alignment::Center)
                        .width(Length::Shrink)
                        .height(Length::Shrink),
                ),
            };
            let content: Element<_> = if self.popup.is_some() {
                mouse_area(content)
                    .on_right_release(Message::ClosePopup)
                    .on_press(Message::ClosePopup)
                    .into()
            } else {
                content.into()
            };
            self.core
                .applet
                .popup_container(content)
                .limits(
                    Limits::NONE
                        .min_width(1.)
                        .min_height(1.)
                        .max_width(1920.)
                        .max_height(1000.),
                )
                .into()
        } else {
            let suggested = self.core.applet.suggested_size(false);
            iced::widget::row!()
                .width(Length::Fixed(suggested.0 as f32))
                .height(Length::Fixed(suggested.1 as f32))
                .into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        // The previews' pictures follow the windows while they show.
        let refresh = if self.preview_of().is_some() {
            iced::time::every(PREVIEW_REFRESH).map(|_| Message::RefreshPreview)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            refresh,
            launcher_entry::subscription().map(Message::Badge),
            wayland_subscription().map(Message::Wayland),
            listen_with(|e, _, id| match e {
                cosmic::iced::core::Event::PlatformSpecific(event::PlatformSpecific::Wayland(
                    event::wayland::Event::Seat(e, seat),
                )) => match e {
                    event::wayland::SeatEvent::Enter => Some(Message::NewSeat(seat)),
                    event::wayland::SeatEvent::Leave => Some(Message::RemovedSeat),
                },
                cosmic::iced::core::Event::Mouse(
                    cosmic::iced::core::mouse::Event::ButtonPressed(_),
                ) => Some(Message::Pressed(id)),
                cosmic::iced::core::Event::Mouse(cosmic::iced::core::mouse::Event::CursorLeft) => {
                    Some(Message::PointerLeft(id))
                }
                _ => None,
            }),
            rectangle_tracker_subscription(0).map(|update| Message::Rectangle(update.1)),
            self.core.watch_config(LIST_ID).map(|u| {
                for why in u.errors {
                    tracing::error!(why = why.to_string(), "Error watching config");
                }
                Message::ConfigUpdated(u.config)
            }),
        ])
    }

    fn style(&self) -> Option<iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

fn launch_on_preferred_gpu(desktop_info: &DesktopEntry, gpus: Option<&[Gpu]>) -> Option<Message> {
    let exec = desktop_info.exec()?;

    let gpu_idx = gpus.map(|gpus| preferred_gpu_idx(desktop_info, gpus.iter()));

    Some(Message::Exec(
        exec.to_string(),
        gpu_idx,
        desktop_info.terminal(),
    ))
}

fn preferred_gpu_idx<'a, I>(desktop_info: &DesktopEntry, mut gpus: I) -> usize
where
    I: Iterator<Item = &'a Gpu>,
{
    gpus.position(|gpu| gpu.default ^ desktop_info.prefers_non_default_gpu())
        .unwrap_or(0)
}

#[derive(Debug, Default, Clone)]
pub struct DndPathBuf(PathBuf);

impl AllowedMimeTypes for DndPathBuf {
    fn allowed() -> std::borrow::Cow<'static, [String]> {
        std::borrow::Cow::Owned(vec![MIME_TYPE.to_string()])
    }
}

impl TryFrom<(Vec<u8>, String)> for DndPathBuf {
    type Error = anyhow::Error;

    fn try_from((data, mime_type): (Vec<u8>, String)) -> Result<Self, Self::Error> {
        if mime_type == MIME_TYPE {
            if let Some(p) = String::from_utf8(data)
                .ok()
                .and_then(|s| Url::from_str(&s).ok())
                .and_then(|u| u.to_file_path().ok())
            {
                Ok(DndPathBuf(p))
            } else {
                anyhow::bail!("Failed to parse.")
            }
        } else {
            anyhow::bail!("Invalid mime type.")
        }
    }
}

impl AsMimeTypes for DndPathBuf {
    fn available(&self) -> std::borrow::Cow<'static, [String]> {
        std::borrow::Cow::Owned(vec![MIME_TYPE.to_string()])
    }

    fn as_bytes(&self, _mime_type: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
        Some(Cow::Owned(self.0.to_str()?.as_bytes().to_vec()))
    }
}
