use crate::tabs::{
    check_config::CheckConfigTab, configure::ConfigureTab, enroll::EnrollTab,
    status::StatusTab, test_auth::TestAuthTab, test_camera::TestCameraTab,
};
use face_auth_core::config::Config;

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Enroll,
    ReEnroll,
    Status,
    TestAuth,
    CheckConfig,
    Configure,
    TestCamera,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Enroll => "Enroll",
            Tab::ReEnroll => "Re-enroll",
            Tab::Status => "Status",
            Tab::TestAuth => "Test Auth",
            Tab::CheckConfig => "Check Config",
            Tab::Configure => "Configure",
            Tab::TestCamera => "Test Camera",
        }
    }

    fn all() -> &'static [Tab] {
        &[
            Tab::Enroll,
            Tab::ReEnroll,
            Tab::Status,
            Tab::TestAuth,
            Tab::CheckConfig,
            Tab::Configure,
            Tab::TestCamera,
        ]
    }
}

pub struct FaceAuthApp {
    active_tab: Tab,
    is_root: bool,
    config: Config,
    enroll_tab: EnrollTab,
    re_enroll_tab: EnrollTab,
    status_tab: StatusTab,
    test_auth_tab: TestAuthTab,
    check_config_tab: CheckConfigTab,
    configure_tab: ConfigureTab,
    test_camera_tab: TestCameraTab,
}

impl FaceAuthApp {
    pub fn new() -> Self {
        let is_root = unsafe { libc::geteuid() } == 0;
        let config = Config::load_system().unwrap_or_default();
        Self {
            active_tab: Tab::Enroll,
            is_root,
            config: config.clone(),
            enroll_tab: EnrollTab::new(false),
            re_enroll_tab: EnrollTab::new(true),
            status_tab: StatusTab::new(),
            test_auth_tab: TestAuthTab::new(),
            check_config_tab: CheckConfigTab::new(),
            configure_tab: ConfigureTab::new(),
            test_camera_tab: TestCameraTab::new(),
        }
    }

    fn switch_tab(&mut self, new_tab: Tab) {
        if self.active_tab == new_tab {
            return;
        }
        match self.active_tab {
            Tab::Enroll => self.enroll_tab.deactivate(),
            Tab::ReEnroll => self.re_enroll_tab.deactivate(),
            Tab::TestCamera => self.test_camera_tab.deactivate(),
            Tab::TestAuth => self.test_auth_tab.deactivate(),
            // These tabs are stateless for now; explicit match ensures
            // the compiler forces a review here if new variants are added.
            Tab::Status | Tab::CheckConfig | Tab::Configure => {}
        }
        self.active_tab = new_tab;
    }
}

impl eframe::App for FaceAuthApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if !self.is_root {
            egui::Panel::top("root_warning").show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "⚠ Not running as root — enrollment chown may fail.",
                    );
                    ui.label("Launch with: sudo face-auth-gui");
                });
            });
        }

        egui::Panel::top("tab_bar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                for &tab in Tab::all() {
                    if ui
                        .selectable_label(self.active_tab == tab, tab.label())
                        .clicked()
                    {
                        self.switch_tab(tab);
                    }
                }
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            match self.active_tab {
                Tab::Enroll => self.enroll_tab.ui(ui, &ctx, &self.config),
                Tab::ReEnroll => self.re_enroll_tab.ui(ui, &ctx, &self.config),
                Tab::Status => self.status_tab.ui(ui),
                Tab::TestAuth => self.test_auth_tab.ui(ui, &ctx, &self.config),
                Tab::CheckConfig => self.check_config_tab.ui(ui),
                Tab::Configure => self.configure_tab.ui(ui, &self.config),
                Tab::TestCamera => self.test_camera_tab.ui(ui, &ctx, &self.config),
            }
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}
