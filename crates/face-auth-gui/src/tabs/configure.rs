use face_auth_core::config::Config;

const CONFIG_PATH: &str = "/etc/face-auth/config.toml";

pub struct ConfigureTab {
    dirty: bool,
    save_status: Option<Result<String, String>>,
}

impl ConfigureTab {
    pub fn new() -> Self {
        Self {
            dirty: false,
            save_status: None,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, config: &mut Config) {
        ui.heading("Configuration");
        ui.label(CONFIG_PATH);
        ui.separator();

        let prev = config.clone();
        self.draw_fields(ui, config);
        if config.recognition.threshold != prev.recognition.threshold
            || config.recognition.frames_required != prev.recognition.frames_required
            || config.recognition.max_enrollment != prev.recognition.max_enrollment
            || config.daemon.session_timeout_s != prev.daemon.session_timeout_s
            || config.daemon.execution_provider != prev.daemon.execution_provider
            || config.liveness.enabled != prev.liveness.enabled
            || config.liveness.lbp_entropy_min != prev.liveness.lbp_entropy_min
            || config.liveness.local_contrast_cv_min != prev.liveness.local_contrast_cv_min
            || config.liveness.local_contrast_cv_max != prev.liveness.local_contrast_cv_max
            || config.camera.crop_right_fraction != prev.camera.crop_right_fraction
            || config.camera.device_path != prev.camera.device_path
            || config.notify.enabled != prev.notify.enabled
            || config.notify.timeout_ms != prev.notify.timeout_ms
        {
            self.dirty = true;
            self.save_status = None;
        }

        ui.separator();

        ui.horizontal(|ui| {
            let save_label = if self.dirty { "Save *" } else { "Save" };
            if ui.button(save_label).clicked() {
                self.save_status = Some(save_config(config));
                if self.save_status.as_ref().is_some_and(|r| r.is_ok()) {
                    self.dirty = false;
                }
            }

            if ui.button("Restart Daemon").clicked() {
                restart_daemon();
            }
        });

        if let Some(ref status) = self.save_status {
            match status {
                Ok(msg) => {
                    ui.colored_label(egui::Color32::GREEN, msg);
                }
                Err(err) => {
                    ui.colored_label(egui::Color32::RED, err);
                }
            }
        }
    }

    fn draw_fields(&mut self, ui: &mut egui::Ui, config: &mut Config) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.collapsing("Recognition", |ui| {
                egui::Grid::new("recognition_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Threshold");
                        ui.add(
                            egui::DragValue::new(&mut config.recognition.threshold)
                                .speed(0.005)
                                .range(0.30..=0.99)
                                .fixed_decimals(3),
                        );
                        ui.end_row();

                        ui.label("Frames required");
                        ui.add(
                            egui::DragValue::new(&mut config.recognition.frames_required)
                                .range(1..=10),
                        );
                        ui.end_row();

                        ui.label("Max enrollment");
                        ui.add(
                            egui::DragValue::new(&mut config.recognition.max_enrollment)
                                .range(1..=100),
                        );
                        ui.end_row();
                    });
            });

            ui.collapsing("Daemon", |ui| {
                egui::Grid::new("daemon_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Session timeout (s)");
                        ui.add(
                            egui::DragValue::new(&mut config.daemon.session_timeout_s)
                                .range(3..=60),
                        );
                        ui.end_row();

                        ui.label("Execution provider");
                        egui::ComboBox::from_id_salt("exec_provider")
                            .selected_text(&config.daemon.execution_provider)
                            .show_ui(ui, |ui| {
                                for opt in &["cpu", "rocm", "cuda", "xdna"] {
                                    ui.selectable_value(
                                        &mut config.daemon.execution_provider,
                                        opt.to_string(),
                                        *opt,
                                    );
                                }
                            });
                        ui.end_row();
                    });
            });

            ui.collapsing("Liveness", |ui| {
                egui::Grid::new("liveness_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Enabled");
                        ui.checkbox(&mut config.liveness.enabled, "");
                        ui.end_row();

                        ui.label("LBP entropy min");
                        ui.add(
                            egui::DragValue::new(&mut config.liveness.lbp_entropy_min)
                                .speed(0.05)
                                .range(0.0..=8.0)
                                .fixed_decimals(2),
                        );
                        ui.end_row();

                        ui.label("Contrast CV min");
                        ui.add(
                            egui::DragValue::new(&mut config.liveness.local_contrast_cv_min)
                                .speed(0.005)
                                .range(0.0..=1.0)
                                .fixed_decimals(3),
                        );
                        ui.end_row();

                        ui.label("Contrast CV max");
                        ui.add(
                            egui::DragValue::new(&mut config.liveness.local_contrast_cv_max)
                                .speed(0.005)
                                .range(0.0..=1.5)
                                .fixed_decimals(3),
                        );
                        ui.end_row();
                    });
            });

            ui.collapsing("Camera", |ui| {
                egui::Grid::new("camera_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Device path");
                        ui.text_edit_singleline(&mut config.camera.device_path);
                        ui.end_row();

                        ui.label("Crop right fraction");
                        ui.add(
                            egui::Slider::new(&mut config.camera.crop_right_fraction, 0.1..=1.0)
                                .fixed_decimals(2),
                        );
                        ui.end_row();
                    });
            });

            ui.collapsing("Notifications", |ui| {
                egui::Grid::new("notify_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Enabled");
                        ui.checkbox(&mut config.notify.enabled, "");
                        ui.end_row();

                        ui.label("Timeout (ms)");
                        ui.add(
                            egui::DragValue::new(&mut config.notify.timeout_ms).range(0..=30000),
                        );
                        ui.end_row();
                    });
            });

            ui.collapsing("Geometry", |ui| {
                egui::Grid::new("geometry_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Distance min");
                        ui.add(
                            egui::DragValue::new(&mut config.geometry.distance_min)
                                .speed(0.005)
                                .range(0.01..=0.5)
                                .fixed_decimals(3),
                        );
                        ui.end_row();

                        ui.label("Distance max");
                        ui.add(
                            egui::DragValue::new(&mut config.geometry.distance_max)
                                .speed(0.005)
                                .range(0.1..=2.0)
                                .fixed_decimals(3),
                        );
                        ui.end_row();

                        ui.label("Yaw max (°)");
                        ui.add(
                            egui::DragValue::new(&mut config.geometry.yaw_max_deg)
                                .speed(1.0)
                                .range(10.0..=90.0)
                                .fixed_decimals(0),
                        );
                        ui.end_row();

                        ui.label("Pitch max (°)");
                        ui.add(
                            egui::DragValue::new(&mut config.geometry.pitch_max_deg)
                                .speed(1.0)
                                .range(10.0..=90.0)
                                .fixed_decimals(0),
                        );
                        ui.end_row();

                        ui.label("Roll max (°)");
                        ui.add(
                            egui::DragValue::new(&mut config.geometry.roll_max_deg)
                                .speed(1.0)
                                .range(5.0..=60.0)
                                .fixed_decimals(0),
                        );
                        ui.end_row();
                    });
            });
        });
    }
}

fn save_config(config: &Config) -> Result<String, String> {
    let toml_str = toml::to_string_pretty(config).map_err(|e| format!("Serialize failed: {e}"))?;
    std::fs::write(CONFIG_PATH, toml_str)
        .map_err(|e| format!("Write failed: {e}\n(Need root — run with sudo)"))?;
    Ok(format!("Saved to {CONFIG_PATH}"))
}

fn restart_daemon() {
    std::thread::spawn(|| {
        let _ = std::process::Command::new("systemctl")
            .args(["restart", "face-authd.service"])
            .status();
    });
}
