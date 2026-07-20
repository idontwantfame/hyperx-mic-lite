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
    gui_widgets::{color_swatch, percent_slider, section_label, target_button},
    lighting::{
        LightingProgram, StreamDuration, live_mute_lighting_color, save_lighting_to_microphone,
        stream_lighting_program_cancelable, write_solid_lighting_once,
    },
    logging::log_event,
    model::{Effect, LightTarget},
};

use super::{LightingUiEvent, MicLiteApp, NoticeSeverity, UiNotice, notice_color};

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
                log_event("error", "lighting.live_mute.error", &[("message", error)]);
            }
        });
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

        let program = LightingProgram {
            effect: self.lighting.effect,
            target: self.lighting.target,
            split_layers: self.lighting.split_layers,
            top_effect: self.lighting.top_effect,
            bottom_effect: self.lighting.bottom_effect,
            colors,
            speed: self.lighting.speed,
            brightness: self.lighting.brightness,
            shared_peak_bits: if self.lighting.effect == Effect::VuMeter {
                self.input_monitor
                    .as_ref()
                    .map(|monitor| monitor.peak_bits())
            } else {
                None
            },
        };
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
                    let _ = sender.send(LightingUiEvent::ApplyFailed(error.clone()));
                    log_event("error", "lighting.apply.error", &[("message", error)]);
                }
            }
        });
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
                let _ = sender.send(LightingUiEvent::SaveFailed(error.clone()));
                log_event("error", "lighting.save.error", &[("message", error)]);
            }
        });
    }

    pub(super) fn ui_lighting_panel(&mut self, ui: &mut egui::Ui, gap: f32) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui(egui::vec2(174.0, 340.0), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        section_label(ui, "LIGHTING EFFECTS");
                        ui.add_space(2.0);
                        for effect in [
                            Effect::Wave,
                            Effect::Solid,
                            Effect::Cycle,
                            Effect::Pulse,
                            Effect::Blink,
                            Effect::Lightning,
                            Effect::VuMeter,
                        ] {
                            if ui
                                .selectable_label(self.lighting.effect == effect, effect.label())
                                .clicked()
                            {
                                self.lighting.effect = effect;
                                self.save_config_snapshot();
                            }
                        }

                        ui.add_space(12.0);
                        section_label(ui, "TARGET");
                        let mut target_changed = false;
                        ui.horizontal(|ui| {
                            target_changed |=
                                target_button(ui, &mut self.lighting.target, LightTarget::All);
                            target_changed |=
                                target_button(ui, &mut self.lighting.target, LightTarget::Top);
                            target_changed |=
                                target_button(ui, &mut self.lighting.target, LightTarget::Bottom);
                        });
                        if target_changed {
                            self.save_config_snapshot();
                        }

                        ui.add_space(10.0);
                        if ui
                            .checkbox(&mut self.lighting.split_layers, "Split top/bottom")
                            .changed()
                        {
                            self.save_config_snapshot();
                        }
                        if self.lighting.split_layers {
                            self.ui_layer_effect_picker(ui, "Top", true);
                            self.ui_layer_effect_picker(ui, "Bottom", false);
                        }
                    });
                });

                ui.add_space(gap);
                ui.allocate_ui(egui::vec2(196.0, 340.0), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        section_label(ui, "COLOR");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
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

                        ui.add_space(8.0);
                        section_label(ui, "BRIGHTNESS");
                        if percent_slider(ui, &mut self.lighting.brightness, 160.0).changed() {
                            self.save_config_snapshot();
                        }
                        section_label(ui, "SPEED");
                        if percent_slider(ui, &mut self.lighting.speed, 160.0).changed() {
                            self.save_config_snapshot();
                        }
                        section_label(ui, "OPACITY");
                        if percent_slider(ui, &mut self.lighting.opacity, 160.0).changed() {
                            self.save_config_snapshot();
                        }
                        if ui
                            .checkbox(&mut self.lighting.live_when_muted, "Lights show live state")
                            .changed()
                        {
                            self.save_config_snapshot();
                            if self.lighting.live_when_muted {
                                if let Some(is_live) =
                                    self.status.as_ref().map(|status| !status.muted)
                                {
                                    self.apply_live_mute_lighting(is_live);
                                }
                            }
                        }

                        ui.add_space(8.0);
                        if ui
                            .add_sized([160.0, 26.0], egui::Button::new("Apply"))
                            .clicked()
                        {
                            self.apply_lighting_to_microphone();
                        }
                        if ui
                            .add_sized([160.0, 26.0], egui::Button::new("Stop Stream"))
                            .clicked()
                        {
                            if let Some(cancel) = &self.lighting_cancel {
                                cancel.store(true, Ordering::Relaxed);
                            }
                            self.lighting_cancel = None;
                            log_event("info", "lighting.apply.stop", &[]);
                        }
                        if ui
                            .add_sized([160.0, 26.0], egui::Button::new("Save to Mic"))
                            .on_hover_text("Experimental persistent device write")
                            .clicked()
                        {
                            self.save_lighting_to_microphone();
                        }
                        if let Some(notice) = &self.lighting_notice {
                            ui.add_space(6.0);
                            ui.colored_label(notice_color(notice.severity), &notice.message);
                        } else if self.lighting_device.is_none() {
                            ui.add_space(6.0);
                            ui.colored_label(
                                notice_color(NoticeSeverity::Warning),
                                "Lighting controls need the QuadCast S HID interface.",
                            );
                        }
                    });
                });
            });
        });
    }

    fn ui_layer_effect_picker(&mut self, ui: &mut egui::Ui, label: &str, top: bool) {
        let mut value = if top {
            self.lighting.top_effect
        } else {
            self.lighting.bottom_effect
        };
        ui.horizontal(|ui| {
            ui.add_sized([46.0, 20.0], egui::Label::new(label));
            egui::ComboBox::from_id_salt(("layer_effect", label))
                .selected_text(value.label())
                .width(92.0)
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
        if top && value != self.lighting.top_effect {
            self.lighting.top_effect = value;
            self.save_config_snapshot();
        } else if !top && value != self.lighting.bottom_effect {
            self.lighting.bottom_effect = value;
            self.save_config_snapshot();
        }
    }
}
