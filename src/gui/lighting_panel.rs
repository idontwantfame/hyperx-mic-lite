use eframe::egui;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{
    gui_widgets::{
        color_swatch, primary_button, secondary_button, section_label, segmented, value_slider,
    },
    lighting::{
        LightingProgram, StreamDuration, live_mute_lighting_color, save_lighting_to_microphone,
        stream_lighting_program_cancelable, write_solid_lighting_once,
    },
    logging::log_event,
    model::{Effect, LightTarget},
};

use super::{LightingUiEvent, MicLiteApp, NoticeSeverity, UiNotice, notice_color, theme};

impl MicLiteApp {
    pub(super) fn apply_live_mute_lighting(&mut self, is_live: bool) {
        if self.lighting_cancel.is_some() {
            log_event(
                "info",
                "lighting.live_mute.skip_active_stream",
                &[("live", is_live.to_string())],
            );
            return;
        }
        let color = live_mute_lighting_color(is_live);
        let brightness = self.lighting.brightness;
        log_event(
            "info",
            "lighting.live_mute.apply",
            &[("live", is_live.to_string())],
        );
        thread::spawn(move || {
            if let Err(error) = write_solid_lighting_once(color, brightness, false) {
                log_event(
                    "error",
                    "lighting.live_mute.error",
                    &[("message", error.to_string())],
                );
            }
        });
    }

    /// The program the current lighting settings describe, exactly as Apply streams it.
    pub(super) fn lighting_program(&self) -> LightingProgram {
        let colors = if self.lighting.effect == Effect::Solid {
            self.lighting
                .colors
                .get(self.lighting.selected_color)
                .copied()
                .map(|color| vec![[color.r(), color.g(), color.b()]])
                .unwrap_or_else(|| vec![[0, 255, 0]])
        } else {
            self.lighting
                .colors
                .iter()
                .map(|color| [color.r(), color.g(), color.b()])
                .collect()
        };

        LightingProgram {
            effect: self.lighting.effect,
            target: self.lighting.target,
            split_layers: self.lighting.split_layers,
            top_effect: self.lighting.top_effect,
            bottom_effect: self.lighting.bottom_effect,
            colors,
            speed: self.lighting.speed,
            brightness: self.lighting.brightness,
            // Share the GUI's cell rather than the monitor's own handle, so the effect keeps
            // reading levels after a replug rebuilds the monitor. When there is no monitor,
            // pass None so the stream opens its own capture instead.
            shared_peak_bits: if self.lighting.effect == Effect::VuMeter
                && self.input_monitor.is_some()
            {
                Some(self.input_peak_bits.clone())
            } else {
                None
            },
        }
    }

    pub(super) fn apply_lighting_to_microphone(&mut self) {
        if self.lighting_device.is_none() {
            log_event("warn", "lighting.apply.no_device", &[]);
            self.lighting_notice = Some(UiNotice {
                severity: NoticeSeverity::Warning,
                message:
                    "Lighting controller not detected. Connect the QuadCast S and close NGENUITY."
                        .to_string(),
            });
            return;
        }

        let program = self.lighting_program();
        log_event(
            "info",
            "lighting.apply.start",
            &[
                ("effect", program.effect.as_config().to_string()),
                ("target", program.target.as_config().to_string()),
                ("split_layers", program.split_layers.to_string()),
                ("top_effect", program.top_effect.as_config().to_string()),
                (
                    "bottom_effect",
                    program.bottom_effect.as_config().to_string(),
                ),
            ],
        );

        let previous_cancel = self.lighting_cancel.take();
        let cancel = Arc::new(AtomicBool::new(false));
        self.lighting_cancel = Some(cancel.clone());
        self.lighting_stream_wanted = true;
        self.lighting_notice = Some(UiNotice {
            severity: NoticeSeverity::Info,
            message: format!("Applying {} lighting.", program.effect.label()),
        });
        let sender = self.lighting_event_sender.clone();

        thread::spawn(move || {
            // Hand off from the previous stream inside the worker so the UI
            // thread never blocks on the cancellation grace period.
            if let Some(previous) = previous_cancel {
                previous.store(true, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(90));
            }
            match stream_lighting_program_cancelable(
                &program,
                StreamDuration::Forever,
                Some(cancel),
                false,
            ) {
                Ok(()) => {
                    let _ = sender.send(LightingUiEvent::Applied);
                    log_event(
                        "info",
                        "lighting.apply.done",
                        &[("effect", program.effect.as_config().to_string())],
                    );
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = sender.send(LightingUiEvent::ApplyFailed(message.clone()));
                    log_event("error", "lighting.apply.error", &[("message", message)]);
                }
            }
        });
    }

    /// Stop the running stream at the user's request.
    pub(super) fn stop_lighting_stream(&mut self) {
        if let Some(cancel) = &self.lighting_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.lighting_cancel = None;
        // Explicit stop, so do not bring the stream back on the next reconnect.
        self.lighting_stream_wanted = false;
        log_event("info", "lighting.apply.stop", &[]);
    }

    pub(super) fn save_lighting_to_microphone(&mut self) {
        if self.lighting_device.is_none() {
            log_event("warn", "lighting.save.no_device", &[]);
            self.lighting_notice = Some(UiNotice {
                severity: NoticeSeverity::Warning,
                message: "Lighting controller not detected. Persistent save is unavailable."
                    .to_string(),
            });
            return;
        }
        self.lighting_notice = Some(UiNotice {
            severity: NoticeSeverity::Info,
            message: "Saving lighting to microphone memory.".to_string(),
        });
        let sender = self.lighting_event_sender.clone();
        thread::spawn(move || match save_lighting_to_microphone(false) {
            Ok(()) => {
                let _ = sender.send(LightingUiEvent::Saved);
                log_event("info", "lighting.save.done", &[]);
            }
            Err(error) => {
                let message = error.to_string();
                let _ = sender.send(LightingUiEvent::SaveFailed(message.clone()));
                log_event("error", "lighting.save.error", &[("message", message)]);
            }
        });
    }

    /// Effect choice. Split mode replaces the whole-mic effect with one per half, so the
    /// two never appear together and the one that will run is always the one on screen.
    pub(super) fn ui_effect_panel(&mut self, ui: &mut egui::Ui) {
        if segmented(
            ui,
            "lighting_mode",
            &mut self.lighting.split_layers,
            &[(false, "Whole mic"), (true, "Split")],
        ) {
            self.save_config_snapshot();
        }
        ui.add_space(8.0);

        if self.lighting.split_layers {
            self.ui_layer_effect_picker(ui, "Top half", true);
            ui.add_space(4.0);
            self.ui_layer_effect_picker(ui, "Bottom half", false);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("VU Meter needs Whole mic.")
                    .size(11.0)
                    .color(theme::TEXT_DIM),
            );
        } else {
            self.ui_effect_grid(ui);
            ui.add_space(8.0);
            section_label(ui, "Target");
            if segmented(
                ui,
                "lighting_target",
                &mut self.lighting.target,
                &[LightTarget::All, LightTarget::Top, LightTarget::Bottom]
                    .map(|target| (target, target.label())),
            ) {
                self.save_config_snapshot();
            }
        }

        ui.add_space(10.0);
        if ui
            .checkbox(&mut self.lighting.live_when_muted, "Show mute state")
            .on_hover_text("Turn the lights green while live and red while muted")
            .changed()
        {
            self.save_config_snapshot();
            if self.lighting.live_when_muted
                && let Some(is_live) = self.status.as_ref().map(|status| !status.muted)
            {
                self.apply_live_mute_lighting(is_live);
            }
        }
    }

    fn ui_effect_grid(&mut self, ui: &mut egui::Ui) {
        let gap = 6.0;
        let full_width = ui.available_width();
        for row in Effect::ALL.chunks(2) {
            // A lone last effect spans the row instead of leaving a hole beside it.
            let chip_width = (full_width - gap * (row.len() - 1) as f32) / row.len() as f32;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for &effect in row {
                    let selected = self.lighting.effect == effect;
                    if ui
                        .add_sized(
                            [chip_width, 28.0],
                            egui::Button::new(effect.label()).selected(selected),
                        )
                        .clicked()
                    {
                        self.lighting.effect = effect;
                        self.save_config_snapshot();
                    }
                }
            });
        }
    }

    pub(super) fn ui_color_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for index in 0..self.lighting.colors.len() {
                let color = self.lighting.colors[index];
                let selected = self.lighting.selected_color == index;
                let response = color_swatch(ui, color, selected);
                let popup_id = response.id.with("color_picker");
                if response.double_clicked() {
                    self.lighting.selected_color = index;
                    egui::Popup::open_id(ui.ctx(), popup_id);
                    self.save_config_snapshot();
                } else if response.clicked() {
                    self.lighting.selected_color = index;
                    self.save_config_snapshot();
                }

                let mut color_changed = false;
                egui::Popup::from_response(&response)
                    .id(popup_id)
                    .open_memory(None)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.spacing_mut().slider_width = 275.0;
                        color_changed = egui::color_picker::color_picker_color32(
                            ui,
                            &mut self.lighting.colors[index],
                            egui::color_picker::Alpha::Opaque,
                        );
                    });
                if color_changed {
                    self.lighting.selected_color = index;
                    self.save_config_snapshot();
                }
            }
        });
        ui.label(
            egui::RichText::new("Double-click a color to change it.")
                .size(11.0)
                .color(theme::TEXT_DIM),
        );

        ui.add_space(4.0);
        if value_slider(ui, "Brightness", &mut self.lighting.brightness).changed() {
            self.save_config_snapshot();
        }
        ui.add_space(4.0);
        if value_slider(ui, "Speed", &mut self.lighting.speed).changed() {
            self.save_config_snapshot();
        }

        ui.add_space(8.0);
        let width = ui.available_width();
        if primary_button(ui, "Apply", width).clicked() {
            self.apply_lighting_to_microphone();
        }
        ui.horizontal(|ui| {
            let gap = 6.0;
            ui.spacing_mut().item_spacing.x = gap;
            let half = (width - gap) / 2.0;
            if secondary_button(ui, "Stop", half).clicked() {
                self.stop_lighting_stream();
            }
            if secondary_button(ui, "Save to mic", half)
                .on_hover_text("Write the effect into the mic's memory (experimental)")
                .clicked()
            {
                self.save_lighting_to_microphone();
            }
        });
        if let Some(notice) = &self.lighting_notice {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&notice.message)
                    .size(11.5)
                    .color(notice_color(notice.severity)),
            );
        } else if self.lighting_device.is_none() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Lighting needs the QuadCast S controller.")
                    .size(11.5)
                    .color(notice_color(NoticeSeverity::Warning)),
            );
        }
    }

    fn ui_layer_effect_picker(&mut self, ui: &mut egui::Ui, label: &str, top: bool) {
        let mut value = if top {
            self.lighting.top_effect
        } else {
            self.lighting.bottom_effect
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(label).color(theme::TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt(("layer_effect", label))
                    .selected_text(value.label())
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for effect in [
                            Effect::Solid,
                            Effect::Wave,
                            Effect::Cycle,
                            Effect::Pulse,
                            Effect::Blink,
                            Effect::Lightning,
                        ] {
                            ui.selectable_value(&mut value, effect, effect.label());
                        }
                    });
            });
        });
        if top && value != self.lighting.top_effect {
            self.lighting.top_effect = value;
            self.save_config_snapshot();
        } else if !top && value != self.lighting.bottom_effect {
            self.lighting.bottom_effect = value;
            self.save_config_snapshot();
        }
    }
}
