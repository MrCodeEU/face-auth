pub struct TestCameraTab;
impl TestCameraTab {
    pub fn new() -> Self { Self }
    pub fn deactivate(&mut self) {}
    pub fn ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context, _config: &face_auth_core::config::Config) {
        ui.label("Test Camera — coming in next task");
    }
}
