// SPDX-License-Identifier: GPL-3.0-only

//! Pure animation math for the bar: proximity zoom, springs, the click bounce.
//! No I/O besides `motion_scale`, so it is testable without a compositor.

use std::sync::OnceLock;

pub const MAX_ZOOM: f32 = 1.7;
pub const ZOOM_RADIUS_ICONS: f32 = 2.5;

/// Longest slice a spring integrates in one go; longer frames are split.
const SPRING_SUBSTEP_S: f32 = 1.0 / 120.0;
/// A frame longer than this (a stalled bar) is treated as this long.
const SPRING_MAX_FRAME_S: f32 = 0.1;

/// Scale of an icon whose centre is `distance_px` from the pointer: the
/// maximum at the pointer, a cosine falloff, 1.0 from the radius on.
pub fn zoom_scale(distance_px: f32, icon_px: f32) -> f32 {
    let radius = ZOOM_RADIUS_ICONS * icon_px;
    let d = distance_px.abs();
    if d >= radius {
        return 1.0;
    }
    let k = 0.5 * (1.0 + (std::f32::consts::PI * d / radius).cos());
    1.0 + (MAX_ZOOM - 1.0) * k
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Spring {
    pub pos: f32,
    pub vel: f32,
}

impl Spring {
    pub fn new(pos: f32) -> Self {
        Self { pos, vel: 0.0 }
    }

    /// Semi-implicit Euler, split into fixed substeps so a long frame stays stable.
    pub fn step(&mut self, target: f32, dt_s: f32, stiffness: f32, damping: f32) {
        let mut left = dt_s.clamp(0.0, SPRING_MAX_FRAME_S);
        while left > 0.0 {
            let dt = left.min(SPRING_SUBSTEP_S);
            let accel = stiffness * (target - self.pos) - damping * self.vel;
            self.vel += accel * dt;
            self.pos += self.vel * dt;
            left -= dt;
        }
    }

    pub fn settled(&self, target: f32) -> bool {
        (self.pos - target).abs() < 0.002 && self.vel.abs() < 0.01
    }
}

const BOUNCE_FIRST_S: f32 = 0.5;
const BOUNCE_END_S: f32 = 0.8;

/// Upward offset in px of the click bounce, `t_s` seconds after the click:
/// a hop of half an icon, then a smaller one, then rest.
pub fn bounce_offset(t_s: f32, icon_px: f32) -> f32 {
    let hop = |u: f32, height: f32| 4.0 * height * u * (1.0 - u);
    if t_s <= 0.0 || t_s >= BOUNCE_END_S {
        0.0
    } else if t_s < BOUNCE_FIRST_S {
        hop(t_s / BOUNCE_FIRST_S, 0.5 * icon_px)
    } else {
        hop(
            (t_s - BOUNCE_FIRST_S) / (BOUNCE_END_S - BOUNCE_FIRST_S),
            0.25 * icon_px,
        )
    }
}

/// 0.0 when motion is off (reduced motion or `animation_duration_ms` = 0), else 1.0.
/// Read once: `COSMETICS_REDUCE_MOTION` and the compositor's `animation_duration_ms`.
pub fn motion_scale() -> f32 {
    static SCALE: OnceLock<f32> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let reduce = std::env::var("COSMETICS_REDUCE_MOTION").ok();
        let duration = std::env::var_os("HOME")
            .map(|home| {
                std::path::Path::new(&home)
                    .join(".config/cosmic/com.system76.CosmicComp/v1/animation_duration_ms")
            })
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| text.trim().trim_start_matches("Some(").trim_end_matches(')').parse().ok());
        motion_scale_from(reduce.as_deref(), duration)
    })
}

fn motion_scale_from(reduce: Option<&str>, duration_ms: Option<u32>) -> f32 {
    if reduce == Some("1") || duration_ms == Some(0) {
        0.0
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_peaks_at_the_pointer_and_ends_at_the_radius() {
        assert_eq!(zoom_scale(0.0, 32.0), MAX_ZOOM);
        assert_eq!(zoom_scale(80.0, 32.0), 1.0);
        assert_eq!(zoom_scale(500.0, 32.0), 1.0);
    }

    #[test]
    fn zoom_falls_monotonically() {
        let mut prev = zoom_scale(0.0, 32.0);
        for d in 1..=80 {
            let s = zoom_scale(d as f32, 32.0);
            assert!(s <= prev, "d={d}: {s} > {prev}");
            prev = s;
        }
    }

    #[test]
    fn zoom_ignores_the_sign_of_the_distance() {
        assert_eq!(zoom_scale(-20.0, 32.0), zoom_scale(20.0, 32.0));
    }

    #[test]
    fn spring_converges_without_big_overshoot() {
        let mut s = Spring::new(1.0);
        let mut peak = 1.0f32;
        let mut t = 0.0;
        while t < 2.0 {
            s.step(1.7, 1.0 / 120.0, 400.0, 28.0);
            peak = peak.max(s.pos);
            t += 1.0 / 120.0;
        }
        assert!(s.settled(1.7), "pos={} vel={}", s.pos, s.vel);
        assert!(peak <= 1.7 * 1.08, "peak={peak}");
    }

    #[test]
    fn spring_survives_a_long_frame() {
        let mut s = Spring::new(1.0);
        s.step(1.7, 5.0, 400.0, 28.0);
        assert!(s.pos.is_finite() && s.pos < 3.0);
    }

    #[test]
    fn bounce_starts_and_ends_at_zero_and_peaks_in_range() {
        assert_eq!(bounce_offset(0.0, 32.0), 0.0);
        assert_eq!(bounce_offset(1.0, 32.0), 0.0);
        assert_eq!(bounce_offset(-1.0, 32.0), 0.0);
        let peak = (0..=900)
            .map(|ms| bounce_offset(ms as f32 / 1000.0, 32.0))
            .fold(0.0f32, f32::max);
        assert!((12.0..=20.0).contains(&peak), "peak={peak}");
        assert!((0..=900).all(|ms| bounce_offset(ms as f32 / 1000.0, 32.0) >= 0.0));
    }

    #[test]
    fn motion_is_off_for_reduced_motion_or_zero_duration() {
        assert_eq!(motion_scale_from(Some("1"), Some(200)), 0.0);
        assert_eq!(motion_scale_from(None, Some(0)), 0.0);
        assert_eq!(motion_scale_from(None, Some(200)), 1.0);
        assert_eq!(motion_scale_from(None, None), 1.0);
        assert_eq!(motion_scale_from(Some("0"), None), 1.0);
    }
}
