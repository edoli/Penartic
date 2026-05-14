#[cfg(not(target_arch = "wasm32"))]
use super::fonts::{self, LoadedFallbackFonts};
use std::time::Duration;

use eframe::egui;

use super::viewer::{PreviewRenderer, ViewportState};
use crate::{
    platform::device::{ConnectionState, DeviceController, PrintState},
    plot::{
        gcode,
        model::{PrintableArea, ToolSettings, ToolpathPlan},
    },
    res::colors,
    svg::toolpath,
};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{self, TryRecvError};

#[cfg(target_arch = "wasm32")]
use poll_promise::Promise;

const SIDEBAR_WIDTH: f32 = 360.0;
const PREVIEW_CONTROL_BAND_HEIGHT: f32 = 104.0;
const CONTROL_BUTTON_WIDTH: f32 = 48.0;
const CONTROL_BUTTON_HEIGHT: f32 = 44.0;
const CONTROL_GRID_SPACING: f32 = 4.0;

pub struct PenarticApp {
    settings: ToolSettings,
    device: DeviceController,
    preview_renderer: PreviewRenderer,
    viewport_state: ViewportState,
    loaded_svg: Option<LoadedSvg>,
    toolpath_plan: Option<ToolpathPlan>,
    preview_progress: f32,
    preview_playing: bool,
    jog_step_mm: f32,
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

#[derive(Clone)]
pub struct StartupSvg {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

impl PenarticApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        preview_msaa_samples: u32,
        startup_svg: Option<StartupSvg>,
        startup_error: Option<String>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let mut device = DeviceController::new();
        device.refresh_ports();

        let mut app = Self {
            settings: ToolSettings::default(),
            device,
            preview_renderer: PreviewRenderer::new(cc, preview_msaa_samples),
            viewport_state: ViewportState::default(),
            loaded_svg: startup_svg
                .map(|svg| LoadedSvg { file_name: svg.file_name, bytes: svg.bytes }),
            toolpath_plan: None,
            preview_progress: 0.0,
            preview_playing: false,
            jog_step_mm: 1.0,
            error_message: startup_error,
            #[cfg(not(target_arch = "wasm32"))]
            pending_fallback_fonts: fonts::spawn_fallback_font_loader(),
            #[cfg(target_arch = "wasm32")]
            pending_svg_pick: None,
        };

        if app.loaded_svg.is_some() {
            app.rebuild_toolpath();
        }

        app
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

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped_files = ctx.input(|input| input.raw.dropped_files.clone());
        if dropped_files.is_empty() {
            return;
        }

        match self.load_first_dropped_svg(dropped_files) {
            Ok(true) => {}
            Ok(false) => {
                self.error_message =
                    Some("드래그 드롭으로는 SVG 파일만 불러올 수 있습니다.".to_owned());
            }
            Err(error) => {
                self.error_message = Some(error);
            }
        }
    }

    fn load_first_dropped_svg(
        &mut self,
        dropped_files: Vec<egui::DroppedFile>,
    ) -> Result<bool, String> {
        for file in dropped_files {
            if !is_svg_dropped_file(&file) {
                continue;
            }

            let file_name = dropped_file_name(&file);
            if let Some(bytes) = file.bytes.as_deref() {
                self.load_svg(file_name, bytes.to_vec());
                return Ok(true);
            }

            #[cfg(not(target_arch = "wasm32"))]
            if let Some(path) = file.path.as_ref() {
                let bytes = std::fs::read(path)
                    .map_err(|error| format!("드롭한 SVG 파일을 읽지 못했습니다: {error}"))?;
                self.load_svg(file_name, bytes);
                return Ok(true);
            }

            return Err(
                "드롭한 SVG 데이터를 아직 읽을 수 없습니다. 잠시 후 다시 시도하세요.".to_owned()
            );
        }

        Ok(false)
    }

    fn rebuild_toolpath(&mut self) {
        self.settings.sanitize();

        let Some(svg) = self.loaded_svg.as_ref() else {
            self.toolpath_plan = None;
            return;
        };

        match toolpath::prepare_svg(svg.file_name.clone(), &svg.bytes, self.settings.printable_area)
        {
            Ok(prepared) => {
                self.toolpath_plan = Some(gcode::build_plan(prepared, &self.settings));
                self.preview_progress = 1.0;
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
            let detected_area = PrintableArea::new(area.width_mm, area.height_mm);
            if printable_area_changed(self.settings.printable_area, detected_area) {
                self.settings.printable_area = detected_area;
                self.rebuild_toolpath();
            }
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
        let Some(plan) = self.toolpath_plan.as_ref() else {
            return;
        };
        if !self.preview_playing {
            return;
        }

        let dt = ctx.input(|input| input.stable_dt).min(0.1);
        let total_duration_s = plan.total_duration_s().max(0.1);
        self.preview_progress = (self.preview_progress + dt / total_duration_s).min(1.0);

        if self.preview_progress >= 1.0 {
            self.preview_playing = false;
        }
    }

    fn apply_device_action(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => self.error_message = None,
            Err(error) => self.error_message = Some(error),
        }
    }

    fn show_manual_controls(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("수동 제어");

        let can_control = self.device.is_connected() && !self.device.is_job_active();
        let jog_feed_rate = self.settings.travel_feed_rate();
        let xy_pad_width = CONTROL_BUTTON_WIDTH * 3.0 + CONTROL_GRID_SPACING * 2.0;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(xy_pad_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label("X/Y");
                    ui.separator();
                    egui::Grid::new("xy-jog-grid").spacing(egui::vec2(4.0, 4.0)).show(ui, |ui| {
                        spacer_button_cell(ui);
                        if control_button(ui, "↑", can_control).clicked() {
                            let result = self.device.jog_xy(0.0, self.jog_step_mm, jog_feed_rate);
                            self.apply_device_action(result);
                        }
                        spacer_button_cell(ui);
                        ui.end_row();

                        if control_button(ui, "←", can_control).clicked() {
                            let result = self.device.jog_xy(-self.jog_step_mm, 0.0, jog_feed_rate);
                            self.apply_device_action(result);
                        }
                        if control_button(ui, "🏠", can_control).clicked() {
                            let result = self.device.home_xy();
                            self.apply_device_action(result);
                        }
                        if control_button(ui, "→", can_control).clicked() {
                            let result = self.device.jog_xy(self.jog_step_mm, 0.0, jog_feed_rate);
                            self.apply_device_action(result);
                        }
                        ui.end_row();

                        spacer_button_cell(ui);
                        if control_button(ui, "↓", can_control).clicked() {
                            let result = self.device.jog_xy(0.0, -self.jog_step_mm, jog_feed_rate);
                            self.apply_device_action(result);
                        }
                        spacer_button_cell(ui);
                        ui.end_row();
                    });
                },
            );

            ui.add_space(12.0);

            ui.allocate_ui_with_layout(
                egui::vec2(CONTROL_BUTTON_WIDTH, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label("Z");
                    ui.separator();
                    egui::Grid::new("z-jog-grid").spacing(egui::vec2(4.0, 4.0)).show(ui, |ui| {
                        if control_button(ui, "↑", can_control).clicked() {
                            let result = self.device.jog_z(self.jog_step_mm, jog_feed_rate);
                            self.apply_device_action(result);
                        }
                        ui.end_row();

                        if control_button(ui, "🏠", can_control).clicked() {
                            let result = self.device.home_z();
                            self.apply_device_action(result);
                        }
                        ui.end_row();

                        if control_button(ui, "↓", can_control).clicked() {
                            let result = self.device.jog_z(-self.jog_step_mm, jog_feed_rate);
                            self.apply_device_action(result);
                        }
                        ui.end_row();
                    });
                },
            );
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("이동 간격");
            for step in [0.1_f32, 1.0, 10.0, 100.0] {
                ui.selectable_value(&mut self.jog_step_mm, step, format_jog_step(step));
            }
        });

        if !can_control {
            ui.small("장치가 연결되어 있고 프린트 중이 아닐 때만 수동 제어를 사용할 수 있습니다.");
        }
    }

    fn show_sidebar(&mut self, root_ui: &mut egui::Ui) {
        egui::Panel::left("settings-sidebar")
            .resizable(false)
            .exact_size(SIDEBAR_WIDTH)
            .show_inside(root_ui, |ui| {
                let sidebar_width = ui.available_width();
                ui.set_width(sidebar_width);
                ui.set_min_width(sidebar_width);

                ui.heading("Penartic");
                ui.label("SVG를 G-code로 변환하고, 오프라인/장치 연결 모드를 모두 지원합니다.");
                ui.separator();

                if full_width_button(ui, "SVG 불러오기").clicked() {
                    self.pick_svg();
                }
                ui.small("파일 선택이나 드래그 드롭으로 SVG를 불러올 수 있습니다.");

                if let Some(plan) = &self.toolpath_plan {
                    if full_width_button(ui, "G-code 복사").clicked() {
                        ui.ctx().copy_text(plan.gcode_text());
                    }
                }

                ui.separator();
                ui.heading("디바이스");

                let is_native = self.device.connection_state() != ConnectionState::Unsupported;
                let status_color = connection_status_color(&self.device);
                ui.horizontal(|ui| {
                    ui.label("상태");
                    ui.colored_label(status_color, format!("● {}", self.device.status_text()));
                });
                ui.horizontal(|ui| {
                    ui.label("작업");
                    ui.colored_label(
                        print_state_color(self.device.print_state()),
                        self.device.print_state_text(),
                    );
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
                    if full_width_button(ui, "포트 새로고침").clicked() {
                        self.device.refresh_ports();
                    }

                    let ports = self.device.ports().to_vec();
                    let combo_width = ui.available_width();
                    egui::ComboBox::from_id_salt("serial-port-combo")
                        .width(combo_width)
                        .selected_text(self.device.selected_port().unwrap_or("포트를 선택하세요"))
                        .show_ui(ui, |ui| {
                            for port in ports {
                                let selected = self.device.selected_port() == Some(port.as_str());
                                if ui.selectable_label(selected, &port).clicked() {
                                    self.device.set_selected_port(Some(port.clone()));
                                }
                            }
                        });

                    let can_connect =
                        matches!(self.device.connection_state(), ConnectionState::Disconnected)
                            && !self.device.ports().is_empty();
                    let can_disconnect = matches!(
                        self.device.connection_state(),
                        ConnectionState::Connecting | ConnectionState::Connected
                    );
                    ui.horizontal(|ui| {
                        let row_button_width =
                            (ui.available_width() - ui.spacing().item_spacing.x) * 0.5;
                        if ui
                            .add_enabled(
                                can_connect,
                                egui::Button::new("연결")
                                    .min_size(egui::vec2(row_button_width, 0.0)),
                            )
                            .clicked()
                        {
                            let result = self.device.connect();
                            self.apply_device_action(result);
                        }

                        if ui
                            .add_enabled(
                                can_disconnect,
                                egui::Button::new("연결 해제")
                                    .min_size(egui::vec2(row_button_width, 0.0)),
                            )
                            .clicked()
                        {
                            self.device.disconnect();
                        }
                    });

                    let can_start_print = self.toolpath_plan.is_some()
                        && self.device.is_connected()
                        && !self.device.is_job_active();
                    let can_stop_print = self.device.can_stop_print();
                    ui.horizontal(|ui| {
                        let row_button_width =
                            (ui.available_width() - ui.spacing().item_spacing.x) * 0.5;
                        if ui
                            .add_enabled(
                                can_start_print,
                                egui::Button::new("프린트 시작")
                                    .min_size(egui::vec2(row_button_width, 0.0)),
                            )
                            .clicked()
                        {
                            if let Some(plan) = self.toolpath_plan.as_ref() {
                                let gcode_lines = plan.gcode_lines.clone();
                                let result = self.device.send_job(&gcode_lines);
                                self.apply_device_action(result);
                            }
                        }

                        if ui
                            .add_enabled(
                                can_stop_print,
                                egui::Button::new("프린트 정지")
                                    .min_size(egui::vec2(row_button_width, 0.0)),
                            )
                            .clicked()
                        {
                            let result = self.device.stop_job();
                            self.apply_device_action(result);
                        }
                    });
                });

                self.show_manual_controls(ui);

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
                            ui.colored_label(colors::warning(), warning);
                        }
                    }
                } else {
                    ui.label("아직 변환된 SVG가 없습니다.");
                }

                if let Some(error) = &self.error_message {
                    ui.separator();
                    ui.colored_label(colors::error(), error);
                }

                if let Some(error) = self.device.last_error() {
                    ui.colored_label(colors::error(), error);
                }

                ui.separator();
                ui.heading("장치 로그");
                let log_width = ui.available_width();
                egui::ScrollArea::vertical().max_height(180.0).auto_shrink([false, false]).show(
                    ui,
                    |ui| {
                        ui.set_min_width(log_width);
                        for line in self.device.log_lines().rev() {
                            ui.add_sized([log_width, 0.0], egui::Label::new(line).wrap());
                        }
                    },
                );
            });
    }

    fn show_central_panel(&mut self, root_ui: &mut egui::Ui) {
        let timeline_text = self
            .toolpath_plan
            .as_ref()
            .map(|plan| {
                format!(
                    "{:.0}% · {:.1}s / {:.1}s",
                    self.preview_progress * 100.0,
                    plan.elapsed_duration_s(self.preview_progress),
                    plan.total_duration_s()
                )
            })
            .unwrap_or_else(|| "0%".to_owned());

        egui::CentralPanel::default().show_inside(root_ui, |ui| {
            egui::Panel::bottom("preview-controls")
                .resizable(false)
                .exact_size(PREVIEW_CONTROL_BAND_HEIGHT)
                .show_inside(ui, |ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let toggle_label =
                            if self.preview_playing { "일시정지" } else { "재생" };
                        if ui
                            .add_enabled(
                                self.toolpath_plan.is_some(),
                                egui::Button::new(toggle_label),
                            )
                            .clicked()
                        {
                            if self.preview_progress >= 1.0 {
                                self.preview_progress = 0.0;
                            }
                            self.preview_playing = !self.preview_playing;
                        }

                        if ui
                            .add_enabled(
                                self.toolpath_plan.is_some(),
                                egui::Button::new("처음으로"),
                            )
                            .clicked()
                        {
                            self.preview_progress = 0.0;
                            self.preview_playing = false;
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(timeline_text);
                        });
                    });

                    ui.add_space(6.0);
                    ui.label("타임라인");
                    ui.scope(|ui| {
                        ui.spacing_mut().slider_width = ui.available_width();
                        let current_interact_height = ui.spacing().interact_size.y;
                        ui.spacing_mut().interact_size.y = current_interact_height.max(28.0);
                        let slider = egui::Slider::new(&mut self.preview_progress, 0.0..=1.0)
                            .show_value(false);
                        let mut changed = false;
                        ui.add_enabled_ui(self.toolpath_plan.is_some(), |ui| {
                            changed = ui.add_sized([ui.available_width(), 28.0], slider).changed();
                        });
                        if changed {
                            self.preview_playing = false;
                        }
                    });
                });

            ui.heading("3D 미리보기");
            ui.label(
                "왼쪽 드래그로 회전, 오른쪽 드래그로 이동, 마우스 휠로 확대/축소할 수 있습니다.",
            );
            ui.add_space(8.0);
            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                let preview_size = egui::vec2(ui.available_width(), ui.available_height());
                ui.set_min_height(preview_size.y.max(1.0));
                self.preview_renderer.show(
                    ui,
                    preview_size,
                    self.toolpath_plan.as_ref(),
                    self.preview_progress,
                    &mut self.viewport_state,
                );
            });
        });
    }
}

impl eframe::App for PenarticApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_dropped_files(ctx);
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

fn is_svg_dropped_file(file: &egui::DroppedFile) -> bool {
    if file.mime.eq_ignore_ascii_case("image/svg+xml") {
        return true;
    }

    if file
        .path
        .as_ref()
        .and_then(|path| path.extension())
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        return true;
    }

    std::path::Path::new(&file.name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
}

fn dropped_file_name(file: &egui::DroppedFile) -> String {
    file.path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| (!file.name.trim().is_empty()).then(|| file.name.clone()))
        .unwrap_or_else(|| "dropped.svg".to_owned())
}

fn connection_status_color(device: &DeviceController) -> egui::Color32 {
    if device.last_error().is_some() {
        colors::error()
    } else {
        match device.connection_state() {
            ConnectionState::Connected => colors::success(),
            ConnectionState::Connecting => colors::warning(),
            ConnectionState::Unsupported | ConnectionState::Disconnected => colors::muted_text(),
        }
    }
}

fn print_state_color(print_state: PrintState) -> egui::Color32 {
    match print_state {
        PrintState::Idle => colors::muted_text(),
        PrintState::Printing => colors::success(),
        PrintState::Stopping => colors::warning(),
    }
}

fn control_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(label).min_size(egui::vec2(CONTROL_BUTTON_WIDTH, CONTROL_BUTTON_HEIGHT)),
    )
}

fn spacer_button_cell(ui: &mut egui::Ui) {
    ui.allocate_exact_size(
        egui::vec2(CONTROL_BUTTON_WIDTH, CONTROL_BUTTON_HEIGHT),
        egui::Sense::hover(),
    );
}

fn format_jog_step(step: f32) -> &'static str {
    if (step - 0.1).abs() < f32::EPSILON {
        "0.1"
    } else if (step - 1.0).abs() < f32::EPSILON {
        "1"
    } else if (step - 10.0).abs() < f32::EPSILON {
        "10"
    } else {
        "100"
    }
}

fn printable_area_changed(current: PrintableArea, next: PrintableArea) -> bool {
    (current.width_mm - next.width_mm).abs() > 0.01
        || (current.height_mm - next.height_mm).abs() > 0.01
}

fn full_width_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), 0.0], egui::Button::new(label))
}
