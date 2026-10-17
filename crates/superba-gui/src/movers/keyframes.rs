//! CSS keyframe animation, evaluated on the egui clock.
//!
//! The scene's SVG animates with CSS `@keyframes` (not SMIL): every animated
//! property is a list of keyframes (offset, value) with one timing function
//! for each interval (the element's `animation-timing-function`; no keyframe
//! overrides it), an iteration duration, a delay and infinite iterations. A
//! transform list keeps the same functions in every keyframe of these
//! animations, so it interpolates function by function: each argument is a
//! [`Track`] of its own.
//!
//! Semantics follow CSS Animations / Web Animations:
//! - before the delay has elapsed the property shows its base value (no
//!   `animation-fill-mode`);
//! - then the iteration progress is `((t - delay) mod duration) / duration`;
//! - between two keyframes the eased progress interpolates linearly; equal
//!   offsets (`0%,8%{…}` written as two keyframes) hold the value;
//! - `steps(1, end)` holds the start value of each interval until its end.

/// A CSS timing function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    /// `linear`.
    Linear,
    /// `cubic-bezier(x1, y1, x2, y2)` (`ease-in-out`, `ease-out`, ...).
    CubicBezier(f32, f32, f32, f32),
    /// `steps(1, end)`: jump at the end of the interval.
    StepEnd,
}

/// `ease-in-out`.
pub const EASE_IN_OUT: Easing = Easing::CubicBezier(0.42, 0.0, 0.58, 1.0);
/// `ease-out`.
pub const EASE_OUT: Easing = Easing::CubicBezier(0.0, 0.0, 0.58, 1.0);

impl Easing {
    /// Eased progress for an input progress `x` in `0..=1`.
    pub fn apply(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Easing::Linear => x,
            Easing::StepEnd => {
                if x >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Easing::CubicBezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, x),
        }
    }
}

/// `y` of the CSS cubic Bézier through (0,0), (x1,y1), (x2,y2), (1,1) at `x`.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    // B(s) = 3(1-s)^2 s p1 + 3(1-s) s^2 p2 + s^3, in f64 for a stable solve.
    let (x1, y1, x2, y2, x) = (
        f64::from(x1),
        f64::from(y1),
        f64::from(x2),
        f64::from(y2),
        f64::from(x),
    );
    let bez = |p1: f64, p2: f64, s: f64| {
        let r = 1.0 - s;
        3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s
    };
    let slope = |p1: f64, p2: f64, s: f64| {
        let r = 1.0 - s;
        3.0 * r * r * p1 + 6.0 * r * s * (p2 - p1) + 3.0 * s * s * (1.0 - p2)
    };
    // Newton from s = x, bisection when the slope vanishes; x(s) is monotonic
    // for control points inside 0..=1.
    let mut s = x;
    for _ in 0..8 {
        let err = bez(x1, x2, s) - x;
        if err.abs() < 1e-7 {
            return bez(y1, y2, s) as f32;
        }
        let d = slope(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    s = x;
    for _ in 0..40 {
        let v = bez(x1, x2, s);
        if (v - x).abs() < 1e-7 {
            break;
        }
        if v < x {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    bez(y1, y2, s) as f32
}

/// One animated number: keyframes, timing, base value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Track {
    /// `(offset 0..=1, value)`, offsets ascending, starting at 0 and ending at 1.
    pub keys: &'static [(f32, f32)],
    /// Timing function of every interval.
    pub easing: Easing,
    /// Iteration duration in seconds.
    pub duration: f32,
    /// `animation-delay` in seconds.
    pub delay: f32,
    /// Value before the delay has elapsed (and without animation).
    pub base: f32,
}

impl Track {
    /// The value at `t` seconds after the animation started.
    pub fn at(&self, t: f64) -> f32 {
        let t = t - f64::from(self.delay);
        if t < 0.0 || self.duration <= 0.0 || self.keys.is_empty() {
            return self.base;
        }
        let d = f64::from(self.duration);
        let p = (t.rem_euclid(d) / d) as f32;
        self.at_progress(p)
    }

    /// The value at iteration progress `p` (`0..=1`).
    pub fn at_progress(&self, p: f32) -> f32 {
        let keys = self.keys;
        let p = p.clamp(0.0, 1.0);
        let Some(&(first_offset, first)) = keys.first() else {
            return self.base;
        };
        if p <= first_offset {
            return first;
        }
        for pair in keys.windows(2) {
            let (o0, v0) = pair[0];
            let (o1, v1) = pair[1];
            if p >= o0 && p < o1 {
                let local = (p - o0) / (o1 - o0);
                return v0 + (v1 - v0) * self.easing.apply(local);
            }
        }
        keys.last().map_or(self.base, |&(_, v)| v)
    }
}

