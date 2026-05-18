pub struct EnrollTab { append: bool }
impl EnrollTab {
    pub fn new(append: bool) -> Self { Self { append } }
    pub fn deactivate(&mut self) {}
    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context, _config: &face_auth_core::config::Config) {
        ui.label(if self.append { "Re-enroll — coming soon" } else { "Enroll — coming soon" });
    }
}
