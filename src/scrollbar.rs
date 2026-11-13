use gpui::{Bounds, Pixels, px};

const MIN_THUMB: f32 = 24.;

/// A scrollbar's thumb along its track, in whatever units its content scrolls by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrollbar {
    pub track: Bounds<Pixels>,
    pub thumb_height: Pixels,
    position: f32,
    max_position: f32,
}

impl Scrollbar {
    /// A scrollbar for content `total` long, `visible` of it at once, scrolled to `position`; none
    /// when it all fits.
    pub fn new(track: Bounds<Pixels>, visible: f32, total: f32, position: f32) -> Option<Self> {
        let max_position = total - visible;
        if track.size.height <= px(0.) || max_position <= 0. {
            return None;
        }
        let thumb_height = (track.size.height * (visible / total))
            .max(px(MIN_THUMB))
            .min(track.size.height);
        Some(Self {
            track,
            thumb_height,
            position,
            max_position,
        })
    }

    fn travel(self) -> Pixels {
        self.track.size.height - self.thumb_height
    }

    pub fn thumb_top(self) -> Pixels {
        self.travel() * (self.position / self.max_position).clamp(0., 1.)
    }

    pub fn contains_thumb(self, y: Pixels) -> bool {
        let top = self.track.top() + self.thumb_top();
        (top..top + self.thumb_height).contains(&y)
    }

    /// The scroll position that puts the thumb's top at `thumb_top`, from the top of the track.
    pub fn position_at(self, thumb_top: Pixels) -> f32 {
        if self.travel() <= px(0.) {
            return 0.;
        }
        (thumb_top / self.travel()).clamp(0., 1.) * self.max_position
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn thumb_tracks_the_position_and_maps_back() {
        let track = Bounds::new(point(px(0.), px(100.)), size(px(10.), px(200.)));
        assert_eq!(Scrollbar::new(track, 10., 10., 0.), None);

        let scrollbar = Scrollbar::new(track, 10., 40., 15.).unwrap();
        assert_eq!(scrollbar.thumb_height, px(50.));
        assert_eq!(scrollbar.thumb_top(), px(75.));
        assert!(scrollbar.contains_thumb(px(175.)));
        assert!(!scrollbar.contains_thumb(px(230.)));
        assert_eq!(scrollbar.position_at(px(75.)), 15.);
        assert_eq!(scrollbar.position_at(px(-20.)), 0.);
        assert_eq!(scrollbar.position_at(px(500.)), 30.);

        let tiny = Scrollbar::new(track, 10., 10_000., 0.).unwrap();
        assert_eq!(tiny.thumb_height, px(MIN_THUMB));
    }
}
