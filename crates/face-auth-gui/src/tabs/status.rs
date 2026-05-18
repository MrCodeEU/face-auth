use face_auth_core::enrollment;

pub struct StatusTab {
    info: Option<StatusInfo>,
}

struct StatusInfo {
    username: String,
    embed_count: usize,
    version: u32,
    current_version: u32,
    path: String,
}

impl StatusTab {
    pub fn new() -> Self {
        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());
        let info = Self::load(&username);
        Self { info: Some(info) }
    }

    fn load(username: &str) -> StatusInfo {
        let embed_count = enrollment::load_embeddings(username)
            .map(|e| e.len())
            .unwrap_or(0);
        let version = enrollment::enrollment_version(username).unwrap_or(0);
        let path = enrollment::enrollment_dir(username)
            .display()
            .to_string();
        StatusInfo {
            username: username.to_string(),
            embed_count,
            version,
            current_version: enrollment::ENROLLMENT_VERSION,
            path,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Enrollment Status");
        ui.separator();

        let username = std::env::var("SUDO_USER")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown".into());

        if ui.button("Refresh").clicked() {
            self.info = Some(Self::load(&username));
        }

        ui.separator();

        if let Some(ref info) = self.info {
            egui::Grid::new("status_grid").num_columns(2).show(ui, |ui| {
                ui.label("User:");
                ui.label(&info.username);
                ui.end_row();

                ui.label("Enrolled:");
                if info.embed_count > 0 {
                    ui.colored_label(egui::Color32::GREEN, "Yes");
                } else {
                    ui.colored_label(egui::Color32::RED, "No");
                }
                ui.end_row();

                ui.label("Embeddings:");
                ui.label(info.embed_count.to_string());
                ui.end_row();

                ui.label("Format version:");
                let ver_label = format!("{} (current: {})", info.version, info.current_version);
                if info.version < info.current_version && info.embed_count > 0 {
                    ui.colored_label(egui::Color32::YELLOW, ver_label);
                } else {
                    ui.label(ver_label);
                }
                ui.end_row();

                ui.label("Path:");
                ui.label(&info.path);
                ui.end_row();
            });

            if info.version < info.current_version && info.embed_count > 0 {
                ui.separator();
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Stale enrollment format — re-enroll for best accuracy.",
                );
            }
        }
    }
}
