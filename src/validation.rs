#![cfg(not(target_arch = "wasm32"))]

use std::{
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui;

use crate::gui::app::{PenarticApp, StartupSvg};

const VALIDATION_SCREENSHOT_TAG: &str = "penartic-ui-validation";

#[derive(Clone, Debug)]
pub struct NativeScreenshotValidationConfig {
    pub startup_svg_path: PathBuf,
    pub output_path: PathBuf,
    pub delay: Duration,
    pub timeout: Duration,
}

impl Default for NativeScreenshotValidationConfig {
    fn default() -> Self {
        Self {
            startup_svg_path: PathBuf::from("sample/sample_curve.svg"),
            output_path: PathBuf::from("target/validation/ui-validation.png"),
            delay: Duration::from_secs(2),
            timeout: Duration::from_secs(20),
        }
    }
}

pub fn run_native_screenshot_validation(
    config: NativeScreenshotValidationConfig,
) -> Result<(), Box<dyn Error>> {
    let startup_svg = read_startup_svg(&config.startup_svg_path)?;
    let result = Arc::new(Mutex::new(None));
    let app_result = Arc::clone(&result);
    let output_path = config.output_path.clone();
    let delay = config.delay;
    let timeout = config.timeout.max(delay + Duration::from_secs(1));

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        multisampling: crate::NATIVE_PREVIEW_MSAA_SAMPLES as u16,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([1100.0, 720.0])
            .with_title("Penartic UI Validation"),
        ..Default::default()
    };

    eframe::run_native(
        "Penartic UI Validation",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(ScreenshotValidationApp::new(
                cc,
                startup_svg,
                output_path,
                delay,
                timeout,
                app_result,
            )))
        }),
    )?;

    match result.lock().expect("validation status lock poisoned").take() {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(error.into()),
        None => Err("UI screenshot validation exited before capturing a screenshot".into()),
    }
}

fn read_startup_svg(path: &std::path::Path) -> Result<StartupSvg, Box<dyn Error>> {
    let bytes = std::fs::read(path)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    Ok(StartupSvg { file_name, bytes })
}

struct ScreenshotValidationApp {
    app: PenarticApp,
    started_at: Instant,
    delay: Duration,
    timeout: Duration,
    output_path: PathBuf,
    result: Arc<Mutex<Option<Result<(), String>>>>,
    requested_screenshot: bool,
}

impl ScreenshotValidationApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        startup_svg: StartupSvg,
        output_path: PathBuf,
        delay: Duration,
        timeout: Duration,
        result: Arc<Mutex<Option<Result<(), String>>>>,
    ) -> Self {
        Self {
            app: PenarticApp::new(cc, crate::NATIVE_PREVIEW_MSAA_SAMPLES, Some(startup_svg), None),
            started_at: Instant::now(),
            delay,
            timeout,
            output_path,
            result,
            requested_screenshot: false,
        }
    }

    fn set_result_and_close(&self, ctx: &egui::Context, result: Result<(), impl Into<String>>) {
        let result = result.map_err(Into::into);
        *self.result.lock().expect("validation status lock poisoned") = Some(result);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn handle_screenshot_events(&self, ctx: &egui::Context) -> bool {
        let screenshot = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { user_data, image, .. } = event {
                    let is_validation_screenshot = user_data
                        .data
                        .as_ref()
                        .and_then(|data| data.downcast_ref::<String>())
                        .is_some_and(|tag| tag == VALIDATION_SCREENSHOT_TAG);

                    if is_validation_screenshot {
                        return Some(Arc::clone(image));
                    }
                }

                None
            })
        });

        let Some(image) = screenshot else {
            return false;
        };

        self.set_result_and_close(ctx, save_color_image(&self.output_path, &image));
        true
    }
}

impl eframe::App for ScreenshotValidationApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);

        if self.handle_screenshot_events(ctx) {
            return;
        }

        let elapsed = self.started_at.elapsed();
        if !self.requested_screenshot && elapsed >= self.delay {
            self.requested_screenshot = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                VALIDATION_SCREENSHOT_TAG.to_owned(),
            )));
            ctx.request_repaint();
        } else if elapsed >= self.timeout {
            self.set_result_and_close(
                ctx,
                Err(format!(
                    "timed out after {:.1}s waiting for the validation screenshot",
                    self.timeout.as_secs_f32()
                )),
            );
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }
}

fn save_color_image(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), String> {
    let [width, height] = image.size;
    if width == 0 || height == 0 {
        return Err(format!("refusing to save empty screenshot: {width}x{height}"));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }

    image::save_buffer(path, &rgba, width as u32, height as u32, image::ColorType::Rgba8)
        .map_err(|error| format!("failed to save {}: {error}", path.display()))
}
