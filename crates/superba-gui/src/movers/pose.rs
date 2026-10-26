//! The scene's animations, transcribed from the `<style>` of
//! `assets/scenes/superba-session-movers.svg` (keyframes, durations, delays,
//! timing functions, transform origins), and what each animated class does at
//! a time `t`.
//!
//! The worker's carry cycle (`travel`, `facing`, `walking`, `standing`,
//! `cargo`, `delivered`, `sink`, `respawn`, `rise`, `returnArm`, `progress`) repeats every 4 s and starts
//! and ends in the same pose; the small loops (walking legs and bob 0.4 s,
//! breathing 4.8 s, foot 3.6 s, sip 7 s, steam 3.8 s with 1.3 s / 2.6 s delays,
//! status dots 2 s) repeat on their own periods, exactly like the browser plays
//! the SVG. Visible motion is continuous at every seam; portal translations
//! reset only while their parcels are hidden.
//!
//! [`Pose::still`] is the SVG's own `prefers-reduced-motion` frame: no
//! animation, the cargo shown, the portal parcels, the returning arm, the
//! steam and the walking legs hidden, the standing legs shown.

use resvg::tiny_skia::Transform;

use super::keyframes::{EASE_IN_OUT, EASE_OUT, Easing, Track};

/// Length of the carry cycle in seconds.
pub const CYCLE_SECS: f32 = 4.0;

/// Length of one step cycle, matched to the faster carry pace.
pub const STEP_SECS: f32 = 0.4;

const fn track(keys: &'static [(f32, f32)], easing: Easing, duration: f32) -> Track {
    Track {
        keys,
        easing,
        duration,
        delay: 0.0,
        base: 0.0,
    }
}

const fn with(mut t: Track, delay: f32, base: f32) -> Track {
    t.delay = delay;
    t.base = base;
    t
}

/// `travel`: `0%,8%{translateX(0)} 46%,59%{translateX(370px)} 92%,100%{translateX(0)}`, 4 s linear.
pub const TRAVEL_X: Track = track(
    &[
        (0.0, 0.0),
        (0.08, 0.0),
        (0.46, 370.0),
        (0.59, 370.0),
        (0.92, 0.0),
        (1.0, 0.0),
    ],
    Easing::Linear,
    CYCLE_SECS,
);
/// `facing`: `0%{scaleX(1)} 59%{scaleX(-1)} 96%{scaleX(1)} 100%{scaleX(1)}`, steps(1,end).
pub const FACING_SX: Track = with(
    track(
        &[(0.0, 1.0), (0.59, -1.0), (0.96, 1.0), (1.0, 1.0)],
        Easing::StepEnd,
        CYCLE_SECS,
    ),
    0.0,
    1.0,
);
/// `bob`: `0%,50%,100%{translateY(0)} 25%,75%{translateY(-3px)}`, 0.4 s ease-in-out.
pub const BOB_Y: Track = track(
    &[
        (0.0, 0.0),
        (0.25, -3.0),
        (0.5, 0.0),
        (0.75, -3.0),
        (1.0, 0.0),
    ],
    EASE_IN_OUT,
    STEP_SECS,
);
/// `walking` (opacity): `0%{0} 8%{1} 46%{0} 59%{1} 92%,100%{0}`, steps(1,end).
pub const WALKING: Track = with(
    track(
        &[
            (0.0, 0.0),
            (0.08, 1.0),
            (0.46, 0.0),
            (0.59, 1.0),
            (0.92, 0.0),
            (1.0, 0.0),
        ],
        Easing::StepEnd,
        CYCLE_SECS,
    ),
    0.0,
    1.0,
);
/// `standing` (opacity): `0%{1} 8%{0} 46%{1} 59%{0} 92%,100%{1}`, steps(1,end).
pub const STANDING: Track = with(
    track(
        &[
            (0.0, 1.0),
            (0.08, 0.0),
            (0.46, 1.0),
            (0.59, 0.0),
            (0.92, 1.0),
            (1.0, 1.0),
        ],
        Easing::StepEnd,
        CYCLE_SECS,
    ),
    0.0,
    1.0,
);
/// `legA`: `0%,100%{rotate(14deg)} 50%{rotate(-14deg)}`, 0.4 s ease-in-out, origin 0 26px.
pub const LEG_A_DEG: Track = track(
    &[(0.0, 14.0), (0.5, -14.0), (1.0, 14.0)],
    EASE_IN_OUT,
    STEP_SECS,
);
/// `legB`: `0%,100%{rotate(-14deg)} 50%{rotate(14deg)}`, 0.4 s ease-in-out, origin 0 26px.
pub const LEG_B_DEG: Track = track(
    &[(0.0, -14.0), (0.5, 14.0), (1.0, -14.0)],
    EASE_IN_OUT,
    STEP_SECS,
);
/// `cargo` (opacity): 1 until 49.999 %, 0 from 50 % to 95.999 %, 1 from 96 %; 4 s linear.
pub const CARGO_OPACITY: Track = with(
    track(
        &[
            (0.0, 1.0),
            (0.08, 1.0),
            (0.12, 1.0),
            (0.46, 1.0),
            (0.49999, 1.0),
            (0.5, 0.0),
            (0.95999, 0.0),
            (0.96, 1.0),
            (1.0, 1.0),
        ],
        Easing::Linear,
        CYCLE_SECS,
    ),
    0.0,
    1.0,
);
/// `cargo` (translateY): lifted 16 px from 8 % to 12 %, carried, set down by 49.999 %.
pub const CARGO_Y: Track = track(
    &[
        (0.0, 0.0),
        (0.08, 0.0),
        (0.12, -16.0),
        (0.46, -16.0),
        (0.49999, 0.0),
        (0.5, 0.0),
        (0.95999, 0.0),
        (0.96, 0.0),
        (1.0, 0.0),
    ],
    Easing::Linear,
    CYCLE_SECS,
);
/// `delivered` (opacity): visible from set-down (50 %) until fully underground (82 %).
pub const DELIVERED: Track = track(
    &[(0.0, 0.0), (0.5, 1.0), (0.82, 0.0), (1.0, 0.0)],
    Easing::StepEnd,
    CYCLE_SECS,
);
/// `sink` (translateY): falls through the ground while the worker returns, 59 % -> 82 %.
pub const DELIVERED_Y: Track = track(
    &[(0.0, 0.0), (0.59, 0.0), (0.82, 140.0), (1.0, 140.0)],
    Easing::CubicBezier(0.42, 0.0, 1.0, 1.0),
    CYCLE_SECS,
);
/// `respawn` (opacity): visible during the return; hands off to the carried crate at 96 %.
pub const RESPAWN: Track = track(
    &[(0.0, 0.0), (0.59, 1.0), (0.96, 0.0), (1.0, 0.0)],
    Easing::StepEnd,
    CYCLE_SECS,
);
/// `rise` (translateY): emerges from the left portal, 59 % -> 88 %, before the worker arrives.
pub const RESPAWN_Y: Track = track(
    &[(0.0, 140.0), (0.59, 140.0), (0.88, 0.0), (1.0, 0.0)],
    EASE_OUT,
    CYCLE_SECS,
);
/// `deliveryMark` (opacity): acknowledges set-down, then fades above the sinking crate.
pub const DELIVERY_MARK: Track = track(
    &[
        (0.0, 0.0),
        (0.49999, 0.0),
        (0.5, 1.0),
        (0.59, 1.0),
        (0.64, 0.0),
        (1.0, 0.0),
    ],
    Easing::Linear,
    CYCLE_SECS,
);
/// `returnArm` (opacity): empty-handed from set-down (50 %) to pickup (96 %).
pub const RETURN_ARM: Track = track(
    &[(0.0, 0.0), (0.5, 1.0), (0.96, 0.0), (1.0, 0.0)],
    Easing::StepEnd,
    CYCLE_SECS,
);
/// `breathe` (translateY): `0%,100%{0} 50%{-3px}`, 4.8 s ease-in-out, origin 0 0.
pub const BREATHE_Y: Track = track(&[(0.0, 0.0), (0.5, -3.0), (1.0, 0.0)], EASE_IN_OUT, 4.8);
/// `breathe` (rotate): `0%,100%{-3deg} 50%{-1deg}`.
pub const BREATHE_DEG: Track = track(&[(0.0, -3.0), (0.5, -1.0), (1.0, -3.0)], EASE_IN_OUT, 4.8);
/// `foot`: `0%,100%{rotate(-5deg)} 50%{rotate(9deg)}`, 3.6 s ease-in-out, origin 18px 40px.
pub const FOOT_DEG: Track = track(&[(0.0, -5.0), (0.5, 9.0), (1.0, -5.0)], EASE_IN_OUT, 3.6);
const SIP_KEYS_X: &[(f32, f32)] = &[
    (0.0, 0.0),
    (0.3, 0.0),
    (0.43, -21.0),
    (0.6, -21.0),
    (0.75, 0.0),
    (1.0, 0.0),
];
const SIP_KEYS_Y: &[(f32, f32)] = &[
    (0.0, 0.0),
    (0.3, 0.0),
    (0.43, -28.0),
    (0.6, -28.0),
    (0.75, 0.0),
    (1.0, 0.0),
];
const SIP_KEYS_DEG: &[(f32, f32)] = &[
    (0.0, 0.0),
    (0.3, 0.0),
    (0.43, -12.0),
    (0.6, -12.0),
    (0.75, 0.0),
    (1.0, 0.0),
];
/// `sip` (translate x): `0%,30%,75%,100%{0} 43%,60%{-21px}`, 7 s ease-in-out, origin 38px -15px.
pub const SIP_X: Track = track(SIP_KEYS_X, EASE_IN_OUT, 7.0);
/// `sip` (translate y): `43%,60%{-28px}`.
pub const SIP_Y: Track = track(SIP_KEYS_Y, EASE_IN_OUT, 7.0);
/// `sip` (rotate): `43%,60%{-12deg}`.
pub const SIP_DEG: Track = track(SIP_KEYS_DEG, EASE_IN_OUT, 7.0);
/// `steam` (opacity): `0%{0} 20%{.55}` and the base value 0 at 100 %; 3.8 s ease-out.
pub const STEAM_OPACITY: Track = track(&[(0.0, 0.0), (0.2, 0.55), (1.0, 0.0)], EASE_OUT, 3.8);
/// `steam` (translateY): `0%{3px} 100%{-43px}`.
pub const STEAM_Y: Track = track(&[(0.0, 3.0), (1.0, -43.0)], EASE_OUT, 3.8);
/// `animation-delay` of the three steam wisps (`.steam`, `.steam.two`, `.steam.three`).
pub const STEAM_DELAYS: [f32; 3] = [0.0, 1.3, 2.6];
/// `pulse` (the status dots' opacity): `0%,100%{1} 50%{.3}`, 2 s ease-in-out.
pub const PULSE: Track = track(&[(0.0, 1.0), (0.5, 0.3), (1.0, 1.0)], EASE_IN_OUT, 2.0);
/// `progress` (scaleX, origin 108px 780px): `0%,8%{.06} 46%,94%{1} 96%,100%{.06}`, 4 s linear.
pub const PROGRESS_SX: Track = track(
    &[
        (0.0, 0.06),
        (0.08, 0.06),
        (0.46, 1.0),
        (0.94, 1.0),
        (0.96, 0.06),
        (1.0, 0.06),
    ],
    Easing::Linear,
    CYCLE_SECS,
);

/// Every animated value of the scene at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// `.travel` translateX.
    pub travel_x: f32,
    /// `.facing` scaleX.
    pub facing_sx: f32,
    /// `.walk-legs` opacity.
    pub walking: f32,
    /// `.still-legs` opacity.
    pub standing: f32,
    /// `.leg-a` rotation (degrees).
    pub leg_a: f32,
    /// `.leg-b` rotation (degrees).
    pub leg_b: f32,
    /// `.body-bob` translateY.
    pub bob_y: f32,
    /// `.cargo` / `.working-arm` opacity.
    pub cargo_opacity: f32,
    /// `.cargo` / `.working-arm` translateY.
    pub cargo_y: f32,
    /// `.delivered` opacity.
    pub delivered: f32,
    /// `.delivered` translateY, clipped at ground level.
    pub delivered_y: f32,
    /// `.respawn` opacity.
    pub respawn: f32,
    /// `.respawn` translateY, clipped at ground level.
    pub respawn_y: f32,
    /// `.delivery-mark` opacity.
    pub delivery_mark: f32,
    /// `.return-arm` opacity.
    pub return_arm: f32,
    /// `.rest-body` translateY.
    pub breathe_y: f32,
    /// `.rest-body` rotation (degrees).
    pub breathe_deg: f32,
    /// `.idle-foot` rotation (degrees).
    pub foot_deg: f32,
    /// `.sip` translate x, y and rotation (degrees).
    pub sip: (f32, f32, f32),
    /// The three `.steam` wisps: (opacity, translateY).
    pub steam: [(f32, f32); 3],
    /// `.dot` opacity.
    pub dot: f32,
    /// `.progress` scaleX.
    pub progress_sx: f32,
}

impl Pose {
    /// The pose `t` seconds after the scene started.
    pub fn at(t: f64) -> Self {
        let steam = STEAM_DELAYS.map(|delay| {
            let opacity = Track {
                delay,
                ..STEAM_OPACITY
            };
            let y = Track { delay, ..STEAM_Y };
            (opacity.at(t), y.at(t))
        });
        Self {
            travel_x: TRAVEL_X.at(t),
            facing_sx: FACING_SX.at(t),
            walking: WALKING.at(t),
            standing: STANDING.at(t),
            leg_a: LEG_A_DEG.at(t),
            leg_b: LEG_B_DEG.at(t),
            bob_y: BOB_Y.at(t),
            cargo_opacity: CARGO_OPACITY.at(t),
            cargo_y: CARGO_Y.at(t),
            delivered: DELIVERED.at(t),
            delivered_y: DELIVERED_Y.at(t),
            respawn: RESPAWN.at(t),
            respawn_y: RESPAWN_Y.at(t),
            delivery_mark: DELIVERY_MARK.at(t),
            return_arm: RETURN_ARM.at(t),
            breathe_y: BREATHE_Y.at(t),
            breathe_deg: BREATHE_DEG.at(t),
            foot_deg: FOOT_DEG.at(t),
            sip: (SIP_X.at(t), SIP_Y.at(t), SIP_DEG.at(t)),
            steam,
            dot: PULSE.at(t),
            progress_sx: PROGRESS_SX.at(t),
        }
    }

    /// The `prefers-reduced-motion` frame of the SVG (no animation at all).
    pub fn still() -> Self {
        Self {
            travel_x: 0.0,
            facing_sx: 1.0,
            walking: 0.0,
            standing: 1.0,
            leg_a: 0.0,
            leg_b: 0.0,
            bob_y: 0.0,
            cargo_opacity: 1.0,
            cargo_y: 0.0,
            delivered: 0.0,
            delivered_y: 0.0,
            respawn: 0.0,
            respawn_y: 0.0,
            delivery_mark: 0.0,
            return_arm: 0.0,
            breathe_y: 0.0,
            breathe_deg: 0.0,
            foot_deg: 0.0,
            sip: (0.0, 0.0, 0.0),
            steam: [(0.0, 0.0); 3],
            dot: 1.0,
            progress_sx: 1.0,
        }
    }

    /// The CSS transform of an animated class, in its parent's coordinates
    /// (`transform-origin` applied); identity for classes that only fade.
    pub fn transform(&self, class: &str, steam_index: usize) -> Transform {
        let about = |ox: f32, oy: f32, t: Transform| {
            Transform::from_translate(ox, oy)
                .pre_concat(t)
                .pre_concat(Transform::from_translate(-ox, -oy))
        };
        match class {
            "travel" => Transform::from_translate(self.travel_x, 0.0),
            "facing" => Transform::from_scale(self.facing_sx, 1.0),
            "body-bob" => Transform::from_translate(0.0, self.bob_y),
            "leg-a" => about(0.0, 26.0, Transform::from_rotate(self.leg_a)),
            "leg-b" => about(0.0, 26.0, Transform::from_rotate(self.leg_b)),
            "cargo" | "working-arm" => Transform::from_translate(0.0, self.cargo_y),
            "delivered" => Transform::from_translate(0.0, self.delivered_y),
            "respawn" => Transform::from_translate(0.0, self.respawn_y),
            "rest-body" => Transform::from_translate(0.0, self.breathe_y)
                .pre_concat(Transform::from_rotate(self.breathe_deg)),
            "idle-foot" => about(18.0, 40.0, Transform::from_rotate(self.foot_deg)),
            "sip" => about(
                38.0,
                -15.0,
                Transform::from_translate(self.sip.0, self.sip.1)
                    .pre_concat(Transform::from_rotate(self.sip.2)),
            ),
            "steam" => Transform::from_translate(0.0, self.steam[steam_index.min(2)].1),
            "progress" => about(108.0, 780.0, Transform::from_scale(self.progress_sx, 1.0)),
            _ => Transform::identity(),
        }
    }

    /// The opacity an animated class gives its element (1 for classes that
    /// only move).
    pub fn opacity(&self, class: &str, steam_index: usize) -> f32 {
        match class {
            "walk-legs" => self.walking,
            "still-legs" => self.standing,
            "cargo" | "working-arm" => self.cargo_opacity,
            "delivered" => self.delivered,
            "respawn" => self.respawn,
            "delivery-mark" => self.delivery_mark,
            "return-arm" => self.return_arm,
            "steam" => self.steam[steam_index.min(2)].0,
            "dot" => self.dot,
            _ => 1.0,
        }
    }
}

/// Classes the SVG animates (the scene's moving parts and their groups).
pub const ANIMATED_CLASSES: &[&str] = &[
    "travel",
    "facing",
    "body-bob",
    "walk-legs",
    "still-legs",
    "leg-a",
    "leg-b",
    "cargo",
    "delivered",
    "respawn",
    "delivery-mark",
    "working-arm",
    "return-arm",
    "rest-body",
    "idle-foot",
    "sip",
    "steam",
    "dot",
    "progress",
];
