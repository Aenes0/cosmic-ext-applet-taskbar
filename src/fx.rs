// SPDX-License-Identifier: GPL-3.0-only

//! Per-item zoom and click-bounce state of the bar. Pure state and timing:
//! the view reads `scale_of`/`rise_of`, a tick subscription runs only while
//! `animating` says something still moves.

use crate::anim::{MAX_ZOOM, Spring, bounce_offset, motion_scale, zoom_scale};
use rustc_hash::FxHashMap;
use std::time::Instant;

const STIFFNESS: f32 = 320.0;
const DAMPING: f32 = 22.0;
/// The panel is only a little taller than its icons: the zoom keeps this
/// fraction of the full macOS zoom so the icon stays inside the surface.
const PANEL_ZOOM_GAIN: f32 = 0.35;
/// A launching app keeps hopping this long, or until its window shows.
const LAUNCH_HOP_MAX_S: f32 = 8.0;
const HOP_PERIOD_S: f32 = 0.9;
const HOP_LAST_S: f32 = 0.8;

#[derive(Default, Clone)]
pub struct Fx {
    hover: Option<u32>,
    springs: FxHashMap<u32, Spring>,
    bounces: FxHashMap<u32, Instant>,
    last_tick: Option<Instant>,
}

impl Fx {
    pub fn set_hover(&mut self, id: u32, entering: bool) {
        if entering {
            self.hover = Some(id);
        } else if self.hover == Some(id) {
            self.hover = None;
        }
    }

    pub fn launch(&mut self, id: u32) {
        if motion_scale() > 0.0 {
            self.bounces.insert(id, Instant::now());
        }
    }

    pub fn scale_of(&self, id: u32) -> f32 {
        self.springs.get(&id).map_or(1.0, |s| s.pos)
    }

    /// Upward offset in px of the item's hop right now.
    pub fn rise_of(&self, id: u32, icon_px: f32, has_windows: bool) -> f32 {
        let Some(start) = self.bounces.get(&id) else {
            return 0.0;
        };
        hop_rise(start.elapsed().as_secs_f32(), icon_px, has_windows)
    }

    pub fn animating(&self) -> bool {
        !self.bounces.is_empty()
            || self.hover.is_some()
            || self.springs.values().any(|s| !s.settled(1.0))
    }

    /// Advance the springs and drop finished hops. `items`: each app's
    /// centre along the bar; `has_windows`: whether the app is open.
    pub fn tick(&mut self, items: &[(u32, f32)], icon_px: f32, has_windows: &dyn Fn(u32) -> bool) {
        let now = Instant::now();
        let dt = self.last_tick.map_or(0.0, |t| (now - t).as_secs_f32());
        self.last_tick = Some(now);
        let motion = motion_scale() > 0.0;
        let hovered = self
            .hover
            .and_then(|h| items.iter().find(|(i, _)| *i == h).map(|(_, x)| *x));
        for (id, x) in items {
            let target = match (motion, hovered) {
                (true, Some(c)) => 1.0 + (zoom_scale(x - c, icon_px) - 1.0) * PANEL_ZOOM_GAIN,
                _ => 1.0,
            };
            let spring = self.springs.entry(*id).or_insert_with(|| Spring::new(1.0));
            spring.step(target, dt, STIFFNESS, DAMPING);
            if spring.settled(target) && target == 1.0 {
                spring.pos = 1.0;
                spring.vel = 0.0;
            }
        }
        self.bounces.retain(|id, start| {
            let t = start.elapsed().as_secs_f32();
            if has_windows(*id) {
                t < HOP_LAST_S
            } else {
                t < LAUNCH_HOP_MAX_S
            }
        });
        if self.hover.is_none() && self.bounces.is_empty() && self.springs.values().all(|s| s.pos == 1.0) {
            self.last_tick = None;
        }
    }
}

/// Rise at `t_s` since the click: one hop if the window is already up,
/// repeated hops while the app is still opening.
fn hop_rise(t_s: f32, icon_px: f32, has_windows: bool) -> f32 {
    let t = if has_windows { t_s } else { t_s % HOP_PERIOD_S };
    bounce_offset(t, icon_px)
}

#[allow(dead_code)]
const _: f32 = MAX_ZOOM;

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fx: &mut Fx, items: &[(u32, f32)], ms: u32) {
        for _ in 0..(ms / 8) {
            fx.tick(items, 32.0, &|_| false);
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }

    #[test]
    fn hovered_item_grows_more_than_its_neighbours_and_all_return() {
        let items = [(1, 20.0), (2, 64.0), (3, 108.0)];
        let mut fx = Fx::default();
        fx.set_hover(2, true);
        run(&mut fx, &items, 400);
        assert!(fx.scale_of(2) > fx.scale_of(1), "{} {}", fx.scale_of(2), fx.scale_of(1));
        assert!(fx.scale_of(1) > 1.0);
        assert!(fx.scale_of(2) <= 1.0 + (MAX_ZOOM - 1.0) * PANEL_ZOOM_GAIN * 1.1);
        fx.set_hover(2, false);
        run(&mut fx, &items, 1500);
        assert!(!fx.animating());
        assert_eq!(fx.scale_of(2), 1.0);
    }

    #[test]
    fn hop_stops_after_one_cycle_when_the_window_is_up() {
        assert_eq!(hop_rise(0.9, 32.0, true), 0.0);
        assert!(hop_rise(0.25, 32.0, true) > 10.0);
        assert!(hop_rise(0.9 + 0.25, 32.0, false) > 10.0);
    }

    #[test]
    fn a_stale_id_in_the_hover_does_not_panic() {
        let mut fx = Fx::default();
        fx.set_hover(9, true);
        run(&mut fx, &[(1, 20.0)], 80);
        assert_eq!(fx.scale_of(9), 1.0);
    }
}
