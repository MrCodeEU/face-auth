use crate::camera_texture;
use face_auth_core::config::Config;
use std::sync::Arc;

pub struct TestCameraTab {
    camera: Option<face_auth_camera::CameraHandle>,
    latest_frame: Option<Arc<face_auth_camera::Frame>>,
    error: Option<String>,
}

impl TestCameraTab {
    pub fn new() -> Self {
        Self { camera: None, latest_frame: None, error: None }
    }

    pub fn deactivate(&mut self) {
        self.camera = None;
        self.latest_frame = None;
    }

    fn activate(&mut self, config: &Config) {
        if self.camera.is_none() {
            match face_auth_camera::open_camera(&config.camera) {
                Ok(cam) => {
                    self.camera = Some(cam);
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("Camera error: {e}")),
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, config: &Config) {
        self.activate(config);

        // Drain latest frame from camera
        if let Some(cam) = &self.camera {
            while let Some(f) = cam.try_recv_frame() {
                self.latest_frame = Some(f);
            }
        }

        ui.heading("Test Camera");
        ui.separator();

        if let Some(ref err) = self.error {
            ui.colored_label(egui::Color32::RED, err);
            return;
        }

        if let Some(ref frame) = self.latest_frame {
            let texture = camera_texture::upload_frame(frame, ctx, "test_camera");
            camera_texture::show_texture(ui, &texture, frame.width, frame.height);
            ui.separator();
            ui.label(format!(
                "Resolution: {}×{}  Format: GREY (IR grayscale)",
                frame.width, frame.height
            ));
        } else {
            ui.label("Waiting for camera...");
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
