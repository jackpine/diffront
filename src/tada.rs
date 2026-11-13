use gpui::{
    Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled, Window, canvas, fill, point, px,
    rgb, size,
};
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

const TADA_SIZE: f32 = 72.;
const CELEBRATION: Duration = Duration::from_millis(1400);
const RESTING_SCALE: f32 = 0.25;
const TURNS: f32 = 2.;
const CONFETTI_PIECES: usize = 16;

const GOLD: u32 = 0xf5b83d;
const RED: u32 = 0xe8505b;
const BLUE: u32 = 0x4c8dff;
const GREEN: u32 = 0x2ecc71;
const PINK: u32 = 0xff5fa2;
const PURPLE: u32 = 0x9b6bff;
const CONFETTI: [u32; 5] = [RED, BLUE, GREEN, PINK, PURPLE];

/// The footer's party popper, which grows in and spins the first time the diff's end comes into view.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Tada {
    #[default]
    Waiting,
    Celebrating(Instant),
}

/// How the popper is drawn at one moment of the celebration.
#[derive(Debug, PartialEq)]
struct Pose {
    scale: f32,
    degrees: f32,
    /// How far the confetti has flown, while it's still in the air.
    burst: Option<f32>,
}

impl Tada {
    pub fn celebrate(&mut self, now: Instant) {
        if *self == Self::Waiting {
            *self = Self::Celebrating(now);
        }
    }

    pub fn is_animating(self, now: Instant) -> bool {
        matches!(self, Self::Celebrating(_)) && self.progress(now) < 1.
    }

    pub fn render(self, now: Instant) -> impl IntoElement {
        let pose = self.pose(now);
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| pose.paint(bounds, window),
        )
        .w(px(TADA_SIZE))
        .h(px(TADA_SIZE))
    }

    fn progress(self, now: Instant) -> f32 {
        match self {
            Self::Waiting => 0.,
            Self::Celebrating(start) => (now.saturating_duration_since(start).as_secs_f32()
                / CELEBRATION.as_secs_f32())
            .min(1.),
        }
    }

    fn pose(self, now: Instant) -> Pose {
        let progress = self.progress(now);
        if self == Self::Waiting {
            return Pose {
                scale: RESTING_SCALE,
                degrees: 0.,
                burst: None,
            };
        }
        Pose {
            scale: RESTING_SCALE + (1. - RESTING_SCALE) * ease_out_back(progress),
            degrees: 360. * TURNS * ease_out_cubic(progress),
            burst: (progress < 1.).then_some(progress),
        }
    }
}

impl Pose {
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let center = bounds.center();
        let unit = px(TADA_SIZE / 2.) * self.scale;
        let (sin, cos) = self.degrees.to_radians().sin_cos();
        let place = |(x, y): (f32, f32)| -> Point<Pixels> {
            point(
                center.x + unit * (x * cos - y * sin),
                center.y + unit * (x * sin + y * cos),
            )
        };
        let polygon = |points: &[(f32, f32)], color: u32, window: &mut Window| {
            let mut path = PathBuilder::fill();
            path.add_polygon(&points.iter().copied().map(place).collect::<Vec<_>>(), true);
            if let Ok(path) = path.build() {
                window.paint_path(path, color_of(color));
            }
        };

        if let Some(progress) = self.burst {
            self.paint_burst(center, progress, window);
        }

        let tip = (-0.8, 0.8);
        let (left, right) = ((-0.3, -0.4), (0.4, 0.3));
        polygon(&[tip, left, right], GOLD, window);
        for (from, to, color) in [(0.45, 0.6, RED), (0.75, 0.88, BLUE)] {
            polygon(
                &[
                    lerp(tip, left, from),
                    lerp(tip, left, to),
                    lerp(tip, right, to),
                    lerp(tip, right, from),
                ],
                color,
                window,
            );
        }

        let mut streamer = PathBuilder::stroke(unit * 0.08);
        streamer.add_polygon(
            &[(0.1, -0.1), (0.25, -0.5), (0.1, -0.7), (0.3, -0.95)].map(place),
            false,
        );
        if let Ok(path) = streamer.build() {
            window.paint_path(path, color_of(PINK));
        }
        for (at, color) in [((0.75, -0.65), GREEN), ((0.85, -0.1), PURPLE)] {
            polygon(&square(at, 0.09), color, window);
        }
        for (at, radius, color) in [((0.55, -0.85), 0.09, BLUE), ((0.62, -0.35), 0.08, RED)] {
            let diameter = unit * radius * 2.;
            window.paint_quad(
                fill(
                    Bounds::centered_at(place(at), size(diameter, diameter)),
                    color_of(color),
                )
                .corner_radii(diameter / 2.),
            );
        }
    }

    fn paint_burst(&self, center: Point<Pixels>, progress: f32, window: &mut Window) {
        let flown = ease_out_cubic(progress);
        let alpha = 1. - progress * progress;
        let side = px(6.) * (1. - progress * 0.5);
        for piece in 0..CONFETTI_PIECES {
            let angle = piece as f32 / CONFETTI_PIECES as f32 * TAU + 0.2;
            let distance = px(TADA_SIZE * 0.6 + 10. * (piece % 3) as f32) * flown;
            let at = point(
                center.x + distance * angle.cos(),
                center.y + distance * angle.sin(),
            );
            let color = color_of(CONFETTI[piece % CONFETTI.len()]).opacity(alpha);
            let corner = if piece % 2 == 0 { side / 2. } else { px(1.) };
            window.paint_quad(
                fill(Bounds::centered_at(at, size(side, side)), color).corner_radii(corner),
            );
        }
    }
}

fn color_of(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn lerp(from: (f32, f32), to: (f32, f32), t: f32) -> (f32, f32) {
    (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
}

fn square((x, y): (f32, f32), half: f32) -> [(f32, f32); 4] {
    [
        (x - half, y - half),
        (x + half, y - half),
        (x + half, y + half),
        (x - half, y + half),
    ]
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// Eases in with a slight overshoot past 1 before settling there.
fn ease_out_back(t: f32) -> f32 {
    const OVERSHOOT: f32 = 1.70158;
    1. + (OVERSHOOT + 1.) * (t - 1.).powi(3) + OVERSHOOT * (t - 1.).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_waits_small_then_grows_and_spins_once() {
        let start = Instant::now();
        let mut tada = Tada::default();
        assert_eq!(tada.pose(start).scale, RESTING_SCALE);
        assert!(!tada.is_animating(start));

        tada.celebrate(start);
        assert!(tada.is_animating(start));
        let midway = tada.pose(start + CELEBRATION / 2);
        assert!(midway.scale > RESTING_SCALE && midway.degrees > 0.);
        assert!(midway.burst.is_some());

        let done = start + CELEBRATION;
        tada.celebrate(done);
        assert!(!tada.is_animating(done));
        assert_eq!(
            tada.pose(done),
            Pose {
                scale: 1.,
                degrees: 360. * TURNS,
                burst: None,
            }
        );
    }
}
