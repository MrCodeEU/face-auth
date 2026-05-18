pub struct StatusTab;
impl StatusTab {
    pub fn new() -> Self { Self }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Status — coming in next task");
    }
}
