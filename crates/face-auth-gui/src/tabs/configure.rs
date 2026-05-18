pub struct ConfigureTab;

impl ConfigureTab {
    pub fn new() -> Self { Self }

    pub fn ui(&mut self, ui: &mut egui::Ui, config: &face_auth_core::config::Config) {
        ui.heading("Configuration");
        ui.separator();
        ui.label("Current settings (read-only — edit /etc/face-auth/config.toml to change):");
        ui.separator();

        egui::Grid::new("config_grid").num_columns(2).show(ui, |ui| {
            ui.label("Recognition threshold:");
            ui.label(format!("{:.2}", config.recognition.threshold));
            ui.end_row();

            ui.label("Session timeout:");
            ui.label(format!("{}s", config.daemon.session_timeout_s));
            ui.end_row();

            ui.label("Max embeddings:");
            ui.label(config.recognition.max_enrollment.to_string());
            ui.end_row();

            ui.label("Liveness enabled:");
            ui.label(config.liveness.enabled.to_string());
            ui.end_row();

            ui.label("Daemon socket:");
            ui.label(&config.daemon.socket_path);
            ui.end_row();

            ui.label("Execution provider:");
            ui.label(&config.daemon.execution_provider);
            ui.end_row();
        });

        ui.separator();
        ui.label("Full edit support coming in a future release.");
    }
}
