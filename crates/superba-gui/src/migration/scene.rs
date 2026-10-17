//! A seven-carton move, followed by a quiet hold and a fade through empty space.

use std::time::Duration;

use egui::{Align2, Painter, Pos2, Rect, pos2, vec2};

use crate::design::fonts::{Weight, font};
use crate::design::tokens::Palette;
use crate::i18n::Tr;

use super::art::{self, CORAL, Canvas};

const BOXES: usize = 7;
const TRIP: f32 = 3.2;
const FADE_IN: f32 = 0.65;
const HOLD: f32 = 0.7;
const FADE_OUT: f32 = 0.55;
const BLANK: f32 = 0.1;
const CYCLE: f32 = FADE_IN + BOXES as f32 * TRIP + HOLD + FADE_OUT + BLANK;
const HOME_X: f32 = 90.0;
const CARRY_Y: f32 = 202.0;

#[derive(Debug)]
struct Frame {
    carton: usize,
    time: f32,
    opacity: f32,
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn between(time: f32, start: f32, end: f32) -> f32 {
    ease((time - start) / (end - start))
}

/// No accumulated frame deltas: backgrounding the window cannot drift the loop.
fn frame_at(elapsed: Duration) -> Frame {
    let cycle = elapsed.as_secs_f64().rem_euclid(f64::from(CYCLE)) as f32;
    let moving = (cycle - FADE_IN).max(0.0);
    let ending = moving - BOXES as f32 * TRIP - HOLD;
    Frame {
        carton: ((moving / TRIP) as usize).min(BOXES - 1),
        time: if moving >= BOXES as f32 * TRIP {
            TRIP
        } else {
            moving % TRIP
        },
        opacity: if cycle < FADE_IN {
            ease(cycle / FADE_IN)
        } else {
            1.0 - ease(ending / FADE_OUT)
        },
    }
}

fn slot(index: usize) -> Pos2 {
    match index {
        // Fill the far side first, so a later carton does not cross a settled one.
        0..=2 => pos2(640.0 - index as f32 * 46.0, 217.0),
        3..=5 => pos2(640.0 - (index - 3) as f32 * 46.0, 178.0),
        _ => pos2(594.0, 139.0),
    }
}

pub(super) fn paint(painter: &Painter, rect: Rect, elapsed: Duration, pal: &Palette, tr: &Tr) {
    let frame = frame_at(elapsed);
    let scale = (rect.width() / 720.0).min(rect.height() / 280.0);
    let c = Canvas {
        painter,
        origin: rect.center() - vec2(720.0, 280.0) * scale / 2.0,
        scale,
        opacity: frame.opacity,
    };
    let seconds = elapsed.as_secs_f64();
    let breeze = ((seconds * 1.7).sin() * 0.10 + (seconds * 2.9).sin() * 0.025) as f32;
    art::houses(&c, pal, breeze);
    for index in 0..frame.carton {
        art::parcel(&c, slot(index), 1.0);
    }
    trip(&c, &frame, pal);
    for (x, key) in [(123.0, "migration.old_home"), (593.0, "migration.new_home")] {
        painter.text(
            c.at(x, 267.0),
            Align2::CENTER_CENTER,
            tr.t(key),
            font(11.0 * scale, Weight::Medium),
            c.color(pal.text_subtle),
        );
    }
}

fn trip(c: &Canvas<'_>, frame: &Frame, pal: &Palette) {
    let t = frame.time;
    let target = slot(frame.carton);
    let destination = target.x - 83.0;
    let outbound = between(t, 0.32, 1.75);
    let returning = between(t, 2.45, TRIP);
    let last = frame.carton == BOXES - 1;
    if !last && t >= 1.75 {
        // Prepare the next carton while the mover settles this one in the new
        // home. It stays at the pickup position across the trip boundary.
        let peek = between(t, 1.75, 1.98) * (1.0 - between(t, 2.35, 2.65));
        let push = between(t, 1.98, 2.35);
        let waiting = pos2(98.0 + (HOME_X + 83.0 - 98.0) * push, 217.0);
        art::helper(c, peek, waiting);
        let painter = c.painter.with_clip_rect(
            Rect::from_min_max(c.at(124.0, 185.0), c.at(205.0, 237.0))
                .intersect(c.painter.clip_rect()),
        );
        let doorway = Canvas {
            painter: &painter,
            origin: c.origin,
            scale: c.scale,
            opacity: c.opacity,
        };
        art::parcel(&doorway, waiting, 0.0);
    }
    let left = t >= 2.45 && !last;
    let x = if left {
        destination + (HOME_X - destination) * returning
    } else {
        HOME_X + (destination - HOME_X) * outbound
    };
    let walking = (0.32..1.75).contains(&t) || (left && t < TRIP);
    let gait = if walking { (t * 23.0).sin() } else { 0.0 };
    let bob = if walking { gait.abs() * -2.0 } else { 0.0 };
    let bend = if t < 0.32 {
        (t / 0.32 * std::f32::consts::PI).sin() * 3.0
    } else {
        0.0
    };
    let body = pos2(x, 171.0 + bob + bend);

    // Raise upper-storey cartons before entering the room, then lower them onto
    // the stack. The 39-point spacing exactly matches the carton's full height.
    let travel_y = if frame.carton >= 3 {
        target.y - 8.0
    } else {
        CARRY_Y
    };
    let carton = if t < 0.32 {
        pos2(
            HOME_X + 83.0,
            217.0 + (CARRY_Y - 217.0) * between(t, 0.0, 0.32),
        )
    } else if t < 1.75 {
        let lift = between(x + 83.0, 420.0, 500.0);
        pos2(x + 83.0, CARRY_Y + (travel_y - CARRY_Y) * lift + bob * 0.45)
    } else {
        pos2(
            target.x,
            travel_y + (target.y - travel_y) * between(t, 1.75, 2.10),
        )
    };
    // Grounding and tiny footsteps are deliberately softer than the mascot.
    c.rect([x + 5.0, 235.0], [54.0, 3.0], 2.0, pal.border);
    // Mirror the complete leg and toe around the same axis as the mascot.
    let foot_x = |offset: f32| x + if left { 70.0 * 0.78 - offset } else { offset };
    for (foot, phase) in [(22.0, 1.0), (37.0, -1.0)] {
        c.line(
            &[
                [foot_x(foot), body.y + 54.0],
                [foot_x(foot + gait * phase * 3.0), 236.0],
                [foot_x(foot + 6.0 + gait * phase * 3.0), 236.0],
            ],
            CORAL,
            1.8,
        );
    }
    art::shrimp(c, body, 0.78, left, gait * 0.8);
    // The carton has landed before any part of the sticker becomes visible.
    let sticker = between(t, 2.12, 2.35);
    art::parcel(c, carton, sticker);
    if t < 2.45 {
        let touching_label = t >= 2.10;
        let hand = if touching_label {
            pos2(carton.x - 8.0 + 8.0 * sticker, carton.y + 2.0)
        } else {
            pos2(carton.x - 18.0, carton.y + 9.0)
        };
        c.line(
            &[
                [x + 50.0, body.y + 34.0],
                [x + 63.0, (body.y + 42.0).min(hand.y + 12.0)],
                [hand.x, hand.y],
            ],
            CORAL,
            2.6,
        );
        c.circle(hand.x, hand.y, 2.6, CORAL);
    } else if last {
        // A small satisfied wave instead of an unnecessary eighth return trip.
        c.line(
            &[
                [x + 50.0, body.y + 35.0],
                [x + 65.0, body.y + 30.0],
                [x + 69.0, body.y + 22.0],
            ],
            CORAL,
            2.2,
        );
        let sparkle = between(t, 2.45, 2.7);
        for (sx, sy) in [(573.0, 37.0), (614.0, 29.0), (634.0, 49.0)] {
            c.line(
                &[[sx - 3.0 * sparkle, sy], [sx + 3.0 * sparkle, sy]],
                CORAL,
                1.3,
            );
            c.line(
                &[[sx, sy - 3.0 * sparkle], [sx, sy + 3.0 * sparkle]],
                CORAL,
                1.3,
            );
        }
    }
}
