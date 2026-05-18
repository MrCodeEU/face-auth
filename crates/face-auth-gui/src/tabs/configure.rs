pub struct ConfigureTab;
impl ConfigureTab {
    pub fn new() -> Self { Self }
    pub fn ui(&mut self, ui: &mut egui::Ui, _config: &face_auth_core::config::Config) {
        ui.label("Configure — coming soon");
    }
}
