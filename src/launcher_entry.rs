// SPDX-License-Identifier: GPL-3.0-only

//! Badges, progress and attention, as apps tell docks over D-Bus
//! (`com.canonical.Unity.LauncherEntry`): the unread count on Telegram's or
//! Thunderbird's button, a download's progress on the browser's, a button
//! that wants to be looked at. Windows draws the same three on its taskbar.

use cosmic::iced::{self, Subscription, stream};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use zbus::{MatchRule, MessageStream, message::Type, zvariant::OwnedValue};

/// What an app's button shows over its icon.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Badge {
    pub count: Option<i64>,
    pub progress: Option<f64>,
    pub urgent: bool,
}

/// Some of an app's badge, as one signal brings it: an app sends only what
/// changed, so the rest stays as it was.
#[derive(Debug, Clone, Default)]
pub struct BadgeUpdate {
    pub app_id: String,
    count: Option<i64>,
    count_visible: Option<bool>,
    progress: Option<f64>,
    progress_visible: Option<bool>,
    urgent: Option<bool>,
}

/// Every app's badge, by desktop id, with what each last said.
#[derive(Debug, Clone, Default)]
pub struct Badges {
    raw: HashMap<String, Raw>,
}

#[derive(Debug, Clone, Default)]
struct Raw {
    count: i64,
    count_visible: bool,
    progress: f64,
    progress_visible: bool,
    urgent: bool,
}

impl Badges {
    pub fn update(&mut self, update: BadgeUpdate) {
        let raw = self.raw.entry(update.app_id).or_default();
        if let Some(count) = update.count {
            raw.count = count;
        }
        if let Some(visible) = update.count_visible {
            raw.count_visible = visible;
        }
        if let Some(progress) = update.progress {
            raw.progress = progress;
        }
        if let Some(visible) = update.progress_visible {
            raw.progress_visible = visible;
        }
        if let Some(urgent) = update.urgent {
            raw.urgent = urgent;
        }
    }

    /// An app's attention is over once one of its windows is looked at.
    pub fn seen(&mut self, app_id: &str) {
        if let Some(raw) = self.raw.get_mut(app_id) {
            raw.urgent = false;
        }
    }

    pub fn get(&self, app_id: &str) -> Badge {
        let Some(raw) = self.raw.get(app_id) else {
            return Badge::default();
        };
        Badge {
            count: (raw.count_visible && raw.count > 0).then_some(raw.count),
            progress: raw
                .progress_visible
                .then_some(raw.progress.clamp(0.0, 1.0)),
            urgent: raw.urgent,
        }
    }
}

/// `application://org.telegram.desktop.desktop` → `org.telegram.desktop`.
fn app_id(uri: &str) -> String {
    let id = uri.strip_prefix("application://").unwrap_or(uri);
    id.strip_suffix(".desktop").unwrap_or(id).to_string()
}

fn parse(uri: &str, props: &HashMap<String, OwnedValue>) -> BadgeUpdate {
    let int = |key: &str| {
        props.get(key).and_then(|v| {
            i64::try_from(v)
                .ok()
                .or_else(|| u64::try_from(v).ok().map(|n| n as i64))
                .or_else(|| i32::try_from(v).ok().map(i64::from))
                .or_else(|| u32::try_from(v).ok().map(i64::from))
        })
    };
    let boolean = |key: &str| props.get(key).and_then(|v| bool::try_from(v).ok());
    BadgeUpdate {
        app_id: app_id(uri),
        count: int("count"),
        count_visible: boolean("count-visible"),
        progress: props.get("progress").and_then(|v| f64::try_from(v).ok()),
        progress_visible: boolean("progress-visible"),
        urgent: boolean("urgent"),
    }
}

pub fn subscription() -> Subscription<BadgeUpdate> {
    Subscription::run_with(std::any::TypeId::of::<BadgeUpdate>(), |_| {
        stream::channel(20, async move |mut output| {
            if let Err(err) = listen(&mut output).await {
                tracing::warn!("no app badges: {err}");
            }
            iced::futures::future::pending::<()>().await;
        })
    })
}

async fn listen(output: &mut futures::channel::mpsc::Sender<BadgeUpdate>) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("com.canonical.Unity.LauncherEntry")?
        .member("Update")?
        .build();
    let mut signals = MessageStream::for_match_rule(rule, &conn, None).await?;
    while let Some(msg) = signals.next().await {
        let Ok(msg) = msg else { continue };
        let Ok((uri, props)) = msg
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>)>()
        else {
            continue;
        };
        if output.send(parse(&uri, &props)).await.is_err() {
            break;
        }
    }
    Ok(())
}
