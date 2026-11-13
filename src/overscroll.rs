use gpui::{Pixels, px};
use std::f32::consts::E;
use std::time::{Duration, Instant};

/// How readily the content follows a push past the end, as in AppKit's rubber band.
const ELASTICITY: f32 = 0.55;
/// How quickly the spring pulls back, per second.
const SPRING_RATE: f32 = 10.;
const SETTLE: Duration = Duration::from_millis(800);
/// How far a flick that coasts into the end bounces past it.
const BOUNCE_MIN: f32 = 8.;
const BOUNCE_MAX: f32 = 48.;
/// A bounce's peak, relative to the size of the coasting scroll that hits the end.
const BOUNCE_GAIN: f32 = 2.;

/// Rubber-band stretch past either end of the diff.
///
/// It follows the fingers while they're on the trackpad, springs back the moment they lift, and
/// turns a flick that coasts into the end into one short bounce.
#[derive(Clone, Copy, Debug, Default)]
pub struct Overscroll {
    motion: Motion,
    fingers_down: bool,
    /// Whether this gesture has already stretched or bounced, so its coast shouldn't bounce.
    spent: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Motion {
    #[default]
    Rest,
    /// Held past the end by the fingers: positive past the bottom.
    Held { pushed: Pixels, extent: Pixels },
    /// Springing back to rest from `from`, moving at `velocity` per second when it started.
    Returning {
        from: Pixels,
        velocity: f32,
        since: Instant,
    },
}

impl Overscroll {
    pub fn fingers_down(&mut self) {
        self.fingers_down = true;
        self.spent = false;
    }

    pub fn fingers_up(&mut self, now: Instant) {
        self.fingers_down = false;
        if let Motion::Held { .. } = self.motion {
            self.motion = Motion::Returning {
                from: self.offset(now),
                velocity: 0.,
                since: now,
            };
        }
    }

    /// Takes a scroll of `delta` toward the bottom, given whether it pushes past an end.
    ///
    /// Returns whether the stretch took the scroll, so the content shouldn't scroll by it too.
    pub fn scrolled(
        &mut self,
        delta: Pixels,
        past_end: bool,
        extent: Pixels,
        now: Instant,
    ) -> bool {
        let current = self.offset(now);
        if self.fingers_down {
            if !past_end && current == px(0.) {
                return false;
            }
            self.drag(current, delta, past_end, extent);
            return true;
        }
        if !past_end {
            return false;
        }
        // A flick that coasts into the end bounces once; one that stretched already had its bounce.
        if !self.spent && self.is_settled(now) {
            self.bounce(delta, now);
        }
        true
    }

    fn drag(&mut self, current: Pixels, delta: Pixels, past_end: bool, extent: Pixels) {
        let toward_stretch = current == px(0.) || (current > px(0.)) == (delta > px(0.));
        let from = if past_end && !toward_stretch {
            px(0.)
        } else {
            unstretch(current, extent)
        };
        let pushed = from + delta;
        self.spent = true;
        // Scrolling back toward the content unwinds the stretch, but never through to the other end.
        let unwound = !past_end && current != px(0.) && pushed.signum() != current.signum();
        self.motion = if unwound {
            Motion::Rest
        } else {
            Motion::Held { pushed, extent }
        };
    }

    fn bounce(&mut self, delta: Pixels, now: Instant) {
        self.spent = true;
        let peak = (delta.abs() * BOUNCE_GAIN).clamp(px(BOUNCE_MIN), px(BOUNCE_MAX));
        // A critically damped spring launched from rest peaks at velocity / (rate × e).
        let velocity = (peak * SPRING_RATE * E / px(1.)) * delta.signum();
        self.motion = Motion::Returning {
            from: px(0.),
            velocity,
            since: now,
        };
    }

    /// How far the content is shifted up from where it rests.
    pub fn offset(&self, now: Instant) -> Pixels {
        match self.motion {
            Motion::Rest => px(0.),
            Motion::Held { pushed, extent } => stretch(pushed, extent),
            Motion::Returning {
                from,
                velocity,
                since,
            } => {
                let elapsed = now.saturating_duration_since(since);
                if elapsed >= SETTLE {
                    return px(0.);
                }
                // A critically damped spring: it returns to rest without overshooting.
                let t = elapsed.as_secs_f32();
                (from + px(velocity * t) + from * SPRING_RATE * t) * (-SPRING_RATE * t).exp()
            }
        }
    }

    pub fn is_settled(&self, now: Instant) -> bool {
        match self.motion {
            Motion::Rest => true,
            Motion::Held { .. } => false,
            Motion::Returning { since, .. } => now.saturating_duration_since(since) >= SETTLE,
        }
    }
}

/// How far a push past the end moves the content: closely at first, then ever more stiffly.
fn stretch(pushed: Pixels, extent: Pixels) -> Pixels {
    if extent <= px(0.) {
        return px(0.);
    }
    let reach = pushed.abs() / extent * ELASTICITY;
    extent * (1. - 1. / (reach + 1.)) * pushed.signum()
}

/// The push that moves the content by `stretched`.
fn unstretch(stretched: Pixels, extent: Pixels) -> Pixels {
    if extent <= px(0.) {
        return px(0.);
    }
    let fraction = (stretched.abs() / extent).min(0.99);
    extent / ELASTICITY * (fraction / (1. - fraction)) * stretched.signum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTENT: Pixels = px(600.);

    fn close(a: Pixels, b: Pixels) -> bool {
        (a - b).abs() < px(0.01)
    }

    fn dragged(pushes: &[f32], now: Instant) -> Overscroll {
        let mut overscroll = Overscroll::default();
        overscroll.fingers_down();
        for push in pushes {
            overscroll.scrolled(px(*push), true, EXTENT, now);
        }
        overscroll
    }

    #[test]
    fn dragging_follows_closely_at_first_then_resists() {
        let now = Instant::now();
        let small = dragged(&[20.], now).offset(now);
        assert!(small > px(10.) && small < px(20.));
        let far = dragged(&[100.; 20], now).offset(now);
        assert!(far > px(200.) && far < EXTENT);
        assert!(close(
            dragged(&[50., 50.], now).offset(now),
            dragged(&[100.], now).offset(now)
        ));
    }

    #[test]
    fn it_holds_while_the_fingers_stay_down_and_springs_back_when_they_lift() {
        let start = Instant::now();
        let mut overscroll = dragged(&[-100.], start);
        let held = overscroll.offset(start);
        let later = start + Duration::from_secs(2);
        assert_eq!(overscroll.offset(later), held);
        assert!(!overscroll.is_settled(later));

        overscroll.fingers_up(later);
        let at = |ms| overscroll.offset(later + Duration::from_millis(ms));
        let mut previous = at(0);
        assert!(close(previous, held));
        for ms in (50..800).step_by(50) {
            assert!(at(ms) >= previous && at(ms) <= px(0.));
            previous = at(ms);
        }
        assert!(at(250) > held / 2.);
        assert_eq!(at(800), px(0.));
        assert!(overscroll.is_settled(later + SETTLE));
    }

    #[test]
    fn scrolling_back_unwinds_the_stretch_without_crossing_to_the_other_end() {
        let now = Instant::now();
        let mut overscroll = dragged(&[100.], now);
        overscroll.scrolled(px(-30.), false, EXTENT, now);
        assert!(close(
            overscroll.offset(now),
            dragged(&[70.], now).offset(now)
        ));
        overscroll.scrolled(px(-500.), false, EXTENT, now);
        assert_eq!(overscroll.offset(now), px(0.));
        overscroll.scrolled(px(-500.), false, EXTENT, now);
        assert_eq!(overscroll.offset(now), px(0.));
    }

    #[test]
    fn pushing_the_other_end_while_springing_back_starts_a_fresh_stretch() {
        let start = Instant::now();
        let mut overscroll = dragged(&[-100.], start);
        overscroll.fingers_up(start);
        overscroll.fingers_down();
        overscroll.scrolled(px(30.), true, EXTENT, start);
        assert!(close(
            overscroll.offset(start),
            dragged(&[30.], start).offset(start)
        ));
    }

    #[test]
    fn coasting_into_the_end_bounces_once_and_returns() {
        let start = Instant::now();
        let mut overscroll = dragged(&[], start);
        overscroll.fingers_up(start);
        overscroll.scrolled(px(30.), true, EXTENT, start);
        let peak_time = Duration::from_secs_f32(1. / SPRING_RATE);
        let peak = overscroll.offset(start + peak_time);
        assert!(close(peak, px(30. * BOUNCE_GAIN).min(px(BOUNCE_MAX))));

        overscroll.scrolled(px(30.), true, EXTENT, start + peak_time);
        assert_eq!(overscroll.offset(start + peak_time), peak);
        assert_eq!(overscroll.offset(start + SETTLE), px(0.));
    }

    #[test]
    fn coasting_after_letting_go_of_a_stretch_does_not_bounce_again() {
        let start = Instant::now();
        let mut overscroll = dragged(&[100.], start);
        overscroll.fingers_up(start);
        let returning = overscroll.offset(start + Duration::from_millis(50));
        assert!(overscroll.scrolled(px(30.), true, EXTENT, start + Duration::from_millis(50)));
        assert_eq!(
            overscroll.offset(start + Duration::from_millis(50)),
            returning
        );
    }

    #[test]
    fn coasting_after_a_stretch_has_settled_does_not_bounce() {
        let start = Instant::now();
        let mut overscroll = dragged(&[100.], start);
        overscroll.fingers_up(start);
        assert!(overscroll.scrolled(px(30.), true, EXTENT, start + SETTLE));
        assert!(overscroll.is_settled(start + SETTLE));
    }

    #[test]
    fn coasting_short_of_the_end_does_nothing() {
        let now = Instant::now();
        let mut overscroll = Overscroll::default();
        assert!(!overscroll.scrolled(px(30.), false, EXTENT, now));
        assert!(overscroll.is_settled(now));
    }
}
