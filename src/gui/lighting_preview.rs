//! Animates the drawn microphone with the selected lighting program.

use std::time::{Duration, Instant};

use crate::{
    lighting::{
        FrameSequence, LightingFrame, LightingProgram, apply_light_target, build_vu_frame,
        effect_frame_delay, smooth_vu_level, vu_target_level, zone_colors,
    },
    model::{Effect, LightTarget},
};

/// After a longer pause (window hidden, app busy) the preview jumps to the present instead
/// of replaying every frame it missed.
const MAX_CATCH_UP: Duration = Duration::from_millis(500);

/// The settings that decide which frames a program produces.
#[derive(Clone, PartialEq)]
struct ProgramKey {
    effect: Effect,
    target: LightTarget,
    split_layers: bool,
    top_effect: Effect,
    bottom_effect: Effect,
    colors: Vec<[u8; 3]>,
    speed: u8,
    brightness: u8,
}

impl ProgramKey {
    fn of(program: &LightingProgram) -> Self {
        Self {
            effect: program.effect,
            target: program.target,
            split_layers: program.split_layers,
            top_effect: program.top_effect,
            bottom_effect: program.bottom_effect,
            colors: program.colors.clone(),
            speed: program.speed,
            brightness: program.brightness,
        }
    }
}

/// Plays a lighting program at the speed the mic would, one zone colour pair at a time.
pub(super) struct LightingPreview {
    key: Option<ProgramKey>,
    sequence: Option<FrameSequence>,
    frame: LightingFrame,
    frame_delay: Duration,
    next_frame_at: Instant,
    vu_level: f32,
    vu_tick: u32,
}

impl LightingPreview {
    pub(super) fn new() -> Self {
        Self {
            key: None,
            sequence: None,
            frame: [[0, 0, 0]; 16],
            frame_delay: Duration::ZERO,
            next_frame_at: Instant::now(),
            vu_level: 0.0,
            vu_tick: 0,
        }
    }

    /// Advance to `now` and return the colours of the top and bottom zones.
    ///
    /// `input_peak` feeds the VU Meter, which follows live input rather than a sequence.
    pub(super) fn zones(
        &mut self,
        program: &LightingProgram,
        input_peak: f32,
        now: Instant,
    ) -> ([u8; 3], [u8; 3]) {
        let key = ProgramKey::of(program);
        if self.key.as_ref() != Some(&key) {
            self.sequence = FrameSequence::new(program).ok();
            self.frame_delay = effect_frame_delay(program.speed);
            self.next_frame_at = now;
            self.key = Some(key);
        }
        if now.saturating_duration_since(self.next_frame_at) > MAX_CATCH_UP {
            self.next_frame_at = now;
        }

        let vu_meter = !program.split_layers && program.effect == Effect::VuMeter;
        while self.next_frame_at <= now {
            self.frame = if vu_meter {
                self.vu_level = smooth_vu_level(self.vu_level, vu_target_level(input_peak));
                self.vu_tick = self.vu_tick.wrapping_add(1);
                apply_light_target(
                    build_vu_frame(self.vu_level, program.brightness, self.vu_tick),
                    program.target,
                )
            } else if let Some(sequence) = self.sequence.as_mut() {
                sequence.next_frame()
            } else {
                self.frame
            };
            self.next_frame_at += self.frame_delay;
        }
        zone_colors(&self.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(effect: Effect) -> LightingProgram {
        LightingProgram {
            effect,
            target: LightTarget::All,
            split_layers: false,
            top_effect: Effect::Solid,
            bottom_effect: Effect::Solid,
            colors: vec![[255, 0, 0], [0, 0, 255]],
            speed: 50,
            brightness: 100,
            shared_peak_bits: None,
        }
    }

    #[test]
    fn solid_preview_shows_the_streamed_color() {
        let mut preview = LightingPreview::new();

        let zones = preview.zones(&program(Effect::Solid), 0.0, Instant::now());

        assert_eq!(zones, ([255, 0, 0], [255, 0, 0]));
    }

    #[test]
    fn blink_preview_steps_at_the_effect_speed() {
        let blink = program(Effect::Blink);
        let delay = effect_frame_delay(blink.speed);
        let mut preview = LightingPreview::new();
        let start = Instant::now();

        let first = preview.zones(&blink, 0.0, start);
        let same_frame = preview.zones(&blink, 0.0, start + delay / 2);

        assert_eq!(first, same_frame, "no new frame before the delay elapses");
    }

    #[test]
    fn changing_the_effect_restarts_the_preview() {
        let mut preview = LightingPreview::new();
        let now = Instant::now();
        preview.zones(&program(Effect::Blink), 0.0, now);

        let mut green = program(Effect::Solid);
        green.colors = vec![[0, 255, 0]];

        assert_eq!(preview.zones(&green, 0.0, now), ([0, 255, 0], [0, 255, 0]));
    }

    #[test]
    fn vu_preview_is_dark_at_the_top_while_silent() {
        let mut preview = LightingPreview::new();

        let (top, _) = preview.zones(&program(Effect::VuMeter), 0.0, Instant::now());

        // Silence only lights the lowest cell, so the top zone stays at the dim base.
        assert!(top.iter().all(|&channel| channel < 80), "top zone {top:?}");
    }
}
