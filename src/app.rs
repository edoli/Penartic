use std::time::Duration;

use eframe::egui;

use crate::{
    device::{ConnectionState, DeviceController},
    fonts, gcode,
    model::{PrintableArea, ToolSettings, ToolpathPlan},
    svg_toolpath,
    viewer::{PreviewRenderer, ViewportState},
};

#[cfg(not(target_arch = "wasm32"))]
use crate::fonts::LoadedFallbackFonts;

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{self, TryRecvError};

#[cfg(target_arch = "wasm32")]
use poll_promise::Promise;

const PREVIEW_PLAYBACK_SECONDS: f32 = 8.0;

pub struct PenarticApp {
    settings: ToolSettings,
    device: DeviceController,
    preview_renderer: PreviewRenderer,
    viewport_state: ViewportState,
    loaded_svg: Option<LoadedSvg>,
    toolpath_plan: Option<ToolpathPlan>,
    preview_progress: f32,
    preview_playing: bool,
    error_message: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    pending_fallback_fonts: Option<mpsc::Receiver<LoadedFallbackFonts>>,
    #[cfg(target_arch = "wasm32")]
    pending_svg_pick: Option<Promise<Option<PickedWebSvg>>>,
}

#[derive(Clone)]
struct LoadedSvg {
    file_name: String,
    bytes: Vec<u8>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct PickedWebSvg {
    file_name: String,
    bytes: Vec<u8>,
}

impl PenarticApp {
    pub fn new(cc: &eframe::CreationContext<'_>, preview_msaa_samples: u32) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let mut device = DeviceController::new();
        device.refresh_ports();

        Self {
            settings: ToolSettings::default(),
            device,
            preview_renderer: PreviewRenderer::new(cc, preview_msaa_samples),
            viewport_state: ViewportState::default(),
            loaded_svg: None,
            toolpath_plan: None,
            preview_progress: 0.0,
            preview_playing: false,
            error_message: None,
            #[cfg(not(target_arch = "wasm32"))]
            pending_fallback_fonts: fonts::spawn_fallback_font_loader(),
            #[cfg(target_arch = "wasm32")]
            pending_svg_pick: None,
        }
    }

    fn pick_svg(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(path) = rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file() {
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        let file_name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        self.load_svg(file_name, bytes);
                    }
                    Err(error) => {
                        self.error_message = Some(format!("SVG 파일을 읽지 못했습니다: {error}"));
                    }
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            if self.pending_svg_pick.is_none() {
                self.pending_svg_pick = Some(Promise::spawn_local(async {
                    let file =
                        rfd::AsyncFileDialog::new().add_filter("SVG", &["svg"]).pick_file().await?;
                    let bytes = file.read().await;
                    Some(PickedWebSvg { file_name: file.file_name(), bytes })
                }));
            }
        }
    }

    fn load_svg(&mut self, file_name: String, bytes: Vec<u8>) {
        self.loaded_svg = Some(LoadedSvg { file_name, bytes });
        self.rebuild_toolpath();
    }

    fn rebuild_toolpath(&mut self) {
        self.settings.sanitize();

        let Some(svg) = self.loaded_svg.as_ref() else {
            self.toolpath_plan = None;
            return;
        };

        match svg_toolpath::prepare_svg(
            svg.file_name.clone(),
            &svg.bytes,
            self.settings.printable_area,
        ) {
            Ok(prepared) => {
                self.toolpath_plan = Some(gcode::build_plan(prepared, &self.settings));
                self.preview_progress = 0.0;
                self.preview_playing = false;
                self.error_message = None;
            }
            Err(error) => {
                self.toolpath_plan = None;
                self.error_message = Some(error.to_string());
            }
        }
    }

    fn handle_device_updates(&mut self) {
        if let Some(area) = self.device.tick() {
            self.settings.printable_area = PrintableArea::new(area.width_mm, area.height_mm);
            self.rebuild_toolpath();
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn handle_web_file_pick(&mut self) {
        if let Some(promise) = self.pending_svg_pick.as_ref() {
            if let Some(result) = promise.ready().cloned() {
                self.pending_svg_pick = None;
                if let Some(file) = result {
                    self.load_svg(file.file_name, file.bytes);
                }
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn handle_web_file_pick(&mut self) {}

    #[cfg(not(target_arch = "wasm32"))]
    fn handle_fallback_fonts(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self.pending_fallback_fonts.as_ref() else {
            return;
        };

        match receiver.try_recv() {
            Ok(loaded_fonts) => {
                fonts::apply_fallback_fonts(ctx, loaded_fonts);
                self.pending_fallback_fonts = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.pending_fallback_fonts = None;
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn handle_fallback_fonts(&mut self, _ctx: &egui::Context) {}

    fn update_preview_playback(&mut self, ctx: &egui::Context) {
        if !self.preview_playing || self.toolpath_plan.is_none() {
            return;
        }

        let dt = ctx.input(|input| input.stable_dt).min(0.1);
        self.preview_progress = (self.preview_progress + dt / PREVIEW_PLAYBACK_SECONDS).min(1.0);

        if self.preview_progress >= 1.0 {
            self.preview_playing = false;
        }
    }

    fn show_sidebar(&mut self, root_ui: &mut egui::Ui) {
        egui::Panel::left("settings-sidebar").resizable(false).exact_size(320.0).show_inside(
            root_ui,
            |ui| {
                ui.heading("Penartic");
                ui.label("SVG를 G-code로 변환하고, 오프라인/장치 연결 모드를 모두 지원합니다.");
                ui.separator();

                if ui.button("SVG 불러오기").clicked() {
                    self.pick_svg();
                }

                if let Some(plan) = &self.toolpath_plan {
                    if ui.button("G-code 복사").clicked() {
                        ui.ctx().copy_text(plan.gcode_text());
                    }
                }

                ui.separator();
                ui.heading("디바이스");

                let is_native = self.device.connection_state() != ConnectionState::Unsupported;
                ui.horizontal(|ui| {
                    ui.label("상태");
                    ui.label(self.device.status_text());
                });

                if let Some(firmware) = self.device.firmware_summary() {
                    ui.label(format!("펌웨어: {firmware}"));
                }

                if let Some(area) = self.device.detected_area() {
                    ui.label(format!(
                        "감지된 사이즈: {:.0} x {:.0} mm",
                        area.width_mm, area.height_mm
                    ));
                }

                ui.add_enabled_ui(is_native, |ui| {
                    if ui.button("포트 새로고침").clicked() {
                        self.device.refresh_ports();
                    }

                    let ports = self.device.ports().to_vec();
                    egui::ComboBox::from_id_salt("serial-port-combo")
                        .width(240.0)
                        .selected_text(self.device.selected_port().unwrap_or("포트를 선택하세요"))
                        .show_ui(ui, |ui| {
                            for port in ports {
                                let selected = self.device.selected_port() == Some(port.as_str());
                                if ui.selectable_label(selected, &port).clicked() {
                                    self.device.set_selected_port(Some(port.clone()));
                                }
                            }
                        });

                    ui.horizontal(|ui| {
                        if ui.button("연결").clicked() {
                            if let Err(error) = self.device.connect() {
                                self.error_message = Some(error);
                            }
                        }

                        if ui.button("연결 해제").clicked() {
                            self.device.disconnect();
                        }
                    });

                    let can_print = self.toolpath_plan.is_some() && self.device.is_connected();
                    if ui.add_enabled(can_print, egui::Button::new("프린트 시작")).clicked() {
                        if let Some(plan) = &self.toolpath_plan {
                            if let Err(error) = self.device.send_job(&plan.gcode_lines) {
                                self.error_message = Some(error);
                            }
                        }
                    }
                });

                ui.separator();
                ui.heading("설정");

                let mut settings_changed = false;
                settings_changed |= drag_value_row(
                    ui,
                    "프린트 가능 너비 (mm)",
                    &mut self.settings.printable_area.width_mm,
                    1.0,
                    10.0..=1_000.0,
                );
                settings_changed |= drag_value_row(
                    ui,
                    "프린트 가능 높이 (mm)",
                    &mut self.settings.printable_area.height_mm,
                    1.0,
                    10.0..=1_000.0,
                );
                settings_changed |= drag_value_row(
                    ui,
                    "프린트 속도 (mm/s)",
                    &mut self.settings.print_speed_mm_s,
                    1.0,
                    1.0..=500.0,
                );
                settings_changed |= drag_value_row(
                    ui,
                    "Z 리프트 (mm)",
                    &mut self.settings.lift_height_mm,
                    0.1,
                    0.1..=25.0,
                );

                if settings_changed {
                    self.rebuild_toolpath();
                }

                ui.separator();
                ui.heading("잡 정보");

                if let Some(plan) = &self.toolpath_plan {
                    ui.label(format!("SVG: {}", plan.source_name));
                    ui.label(format!(
                        "그려지는 범위: {:.1} x {:.1} mm",
                        plan.drawing_bounds.x, plan.drawing_bounds.y
                    ));
                    ui.label(format!("스트로크 수: {}", plan.stats.stroke_count));
                    ui.label(format!("세그먼트 수: {}", plan.stats.segment_count));
                    ui.label(format!("드로잉 거리: {:.1} mm", plan.stats.drawing_distance_mm));
                    ui.label(format!("이동 거리: {:.1} mm", plan.stats.travel_distance_mm));
                    ui.label(format!("예상 소요 시간: {:.1} s", plan.stats.estimated_duration_s));

                    if !plan.warnings.is_empty() {
                        ui.separator();
                        for warning in &plan.warnings {
                            ui.colored_label(egui::Color32::YELLOW, warning);
                        }
                    }
                } else {
                    ui.label("아직 변환된 SVG가 없습니다.");
                }

                if let Some(error) = &self.error_message {
                    ui.separator();
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }

                if let Some(error) = self.device.last_error() {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }

                ui.separator();
                ui.heading("장치 로그");
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    for line in self.device.log_lines().rev() {
                        ui.label(line);
                    }
                });
            },
        );
    }

    fn show_central_panel(&mut self, root_ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(root_ui, |ui| {
            ui.heading("3D 미리보기");
            ui.label("드래그로 회전하고 마우스 휠로 확대/축소할 수 있습니다.");
            ui.add_space(8.0);

            let available_height = (ui.available_height() - 96.0).max(260.0);
            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                ui.set_min_height(available_height);
                self.preview_renderer.show(
                    ui,
                    self.toolpath_plan.as_ref(),
                    self.preview_progress,
                    &mut self.viewport_state,
                );
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let toggle_label = if self.preview_playing { "일시정지" } else { "재생" };
                if ui
                    .add_enabled(self.toolpath_plan.is_some(), egui::Button::new(toggle_label))
                    .clicked()
                {
                    if self.preview_progress >= 1.0 {
                        self.preview_progress = 0.0;
                    }
                    self.preview_playing = !self.preview_playing;
                }

                if ui
                    .add_enabled(self.toolpath_plan.is_some(), egui::Button::new("처음으로"))
                    .clicked()
                {
                    self.preview_progress = 0.0;
                    self.preview_playing = false;
                }

                ui.label(format!("{:.0}%", self.preview_progress * 100.0));
            });

            let slider = egui::Slider::new(&mut self.preview_progress, 0.0..=1.0)
                .show_value(false)
                .text("타임라인");
            if ui.add_enabled(self.toolpath_plan.is_some(), slider).changed() {
                self.preview_playing = false;
            }
        });
    }
}

impl eframe::App for PenarticApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_web_file_pick();
        self.handle_fallback_fonts(ctx);
        self.handle_device_updates();
        self.update_preview_playback(ctx);

        if self.preview_playing {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.device.is_connected() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        #[cfg(target_arch = "wasm32")]
        if self.pending_svg_pick.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        #[cfg(not(target_arch = "wasm32"))]
        if self.pending_fallback_fonts.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_sidebar(ui);
        self.show_central_panel(ui);
    }
}

fn drag_value_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    speed: f64,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed = ui
            .add(egui::DragValue::new(value).speed(speed).range(range).fixed_decimals(2))
            .changed();
    });
    changed
}
