use std::path::Path;

pub struct CheckConfigTab {
    output: Vec<(egui::Color32, String)>,
    ran: bool,
}

impl CheckConfigTab {
    pub fn new() -> Self {
        Self { output: Vec::new(), ran: false }
    }

    fn run_checks(&mut self) {
        self.output.clear();

        let ok = egui::Color32::GREEN;
        let warn = egui::Color32::YELLOW;
        let err = egui::Color32::RED;

        // Config file
        let cfg_path = "/etc/face-auth/config.toml";
        if Path::new(cfg_path).exists() {
            match face_auth_core::config::Config::load(Path::new(cfg_path)) {
                Ok(_) => self.output.push((ok, format!("[OK] Config: {cfg_path}"))),
                Err(e) => self.output.push((err, format!("[FAIL] Config: {e}"))),
            }
        } else {
            self.output.push((warn, format!("[WARN] Config not found: {cfg_path}")));
        }

        // Models
        let model_dirs = ["models", "/usr/share/face-auth/models", "/var/lib/face-auth/models"];
        for (file, desc) in &[("det_500m.onnx", "SCRFD detection"), ("w600k_mbf.onnx", "ArcFace recognition")] {
            let found = model_dirs.iter().any(|d| Path::new(d).join(file).exists());
            if found {
                self.output.push((ok, format!("[OK] Model: {desc}")));
            } else {
                self.output.push((err, format!("[FAIL] Model not found: {file}")));
            }
        }

        // Daemon socket
        let config = face_auth_core::config::Config::load_system().unwrap_or_default();
        if Path::new(&config.daemon.socket_path).exists() {
            self.output.push((ok, format!("[OK] Daemon socket: {}", config.daemon.socket_path)));
        } else {
            self.output.push((warn, "[WARN] Daemon socket not found (is face-authd running?)".into()));
        }

        // PAM module
        let pam_found = ["/usr/lib64/security/pam_face.so", "/var/lib/face-auth/pam_face.so"]
            .iter().any(|p| Path::new(p).exists());
        if pam_found {
            self.output.push((ok, "[OK] PAM module installed".into()));
        } else {
            self.output.push((warn, "[WARN] PAM module not found".into()));
        }

        self.ran = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Check Configuration");
        ui.separator();

        if ui.button("Run Checks").clicked() {
            self.run_checks();
        }

        if self.ran {
            ui.separator();
            for (color, line) in &self.output {
                ui.colored_label(*color, line);
            }
        }
    }
}
