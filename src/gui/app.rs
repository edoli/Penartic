use super::fonts;
#[cfg(not(target_arch = "wasm32"))]
use super::fonts::LoadedFallbackFonts;
use std::time::Duration;

use eframe::egui;

use super::layout::{Size, UiLayoutExt};
use super::viewer::{PreviewRenderer, ViewportState};
use crate::{
    paths,
    platform::device::{ConnectionState, DeviceController, PrintState},
    plot::{
        gcode,
        model::{
            CurveOutputMode, PrintStartMode, PrintableArea, SvgPlacement, ToolSettings,
            ToolpathPlan,
        },
    },
    res::colors,
};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{self, TryRecvError};

#[cfg(target_arch = "wasm32")]
use poll_promise::Promise;

const SIDEBAR_WIDTH: f32 = 360.0;
const PREVIEW_CONTROL_BAND_HEIGHT: f32 = 104.0;
const PREVIEW_CONTROL_OVERLAY_MARGIN: f32 = 12.0;
const CONTROL_BUTTON_WIDTH: f32 = 44.0;
const CONTROL_BUTTON_HEIGHT: f32 = 44.0;
const CONTROL_GRID_SPACING: f32 = 4.0;

pub struct PenarticApp {
    settings: ToolSettings,
    device: DeviceController,
    preview_renderer: PreviewRenderer,
    viewport_state: ViewportState,
    loaded_svg: Option<LoadedSvg>,
    svg_placement: Option<SvgPlacementState>,
    toolpath_plan: Option<ToolpathPlan>,
    preview_progress: f32,
    preview_playing: bool,
    show_travel_moves: bool,
    show_drawing_bounds: bool,
    jog_step_mm: f32,
    error_message: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    pending_fallback_fonts: Option<mpsc::Receiver<LoadedFallbackFonts>>,
    #[cfg(target_arch = "wasm32")]
    pending_svg_pick: Option<Promise<Option<PickedWebSvg>>>,
}

#[derive(Clone)]
struct LoadedSvg {
    document: paths::ParsedSvg,
}

#[derive(Clone, Copy)]
struct SvgPlacementState {
    placement: SvgPlacement,
    native_scale_mm_per_unit: f32,
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

impl SvgPlacementState {
    fn for_svg(svg: &LoadedSvg, printable_area: PrintableArea) -> Self {
        let placement = svg.document.centered_native_placement(printable_area);
        Self { native_scale_mm_per_unit: placement.scale_mm_per_unit, placement }
    }

    fn scale_percent(&self) -> f32 {
        (self.placement.scale_mm_per_unit / self.native_scale_mm_per_unit.max(1e-4) * 100.0)
            .max(1.0)
    }

    fn set_scale_percent(&mut self, scale_percent: f32) {
        self.placement.scale_mm_per_unit =
            (self.native_scale_mm_per_unit.max(1e-4) * (scale_percent.max(1.0) / 100.0)).max(1e-4);
    }
}

impl PenarticApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        preview_msaa_samples: u32,
        startup_svg: Option<StartupSvg>,
        startup_error: Option<String>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        #[cfg(target_arch = "wasm32")]
        fonts::apply_fallback_fonts(&cc.egui_ctx, fonts::web_fallback_fonts());

        let mut device = DeviceController::new();
        device.refresh_ports();

        let mut app = Self {
            settings: ToolSettings::default(),
            device,
            preview_renderer: PreviewRenderer::new(cc, preview_msaa_samples),
            viewport_state: ViewportState::default(),
            loaded_svg: None,
            svg_placement: None,
            toolpath_plan: None,
            preview_progress: 0.0,
            preview_playing: false,
            show_travel_moves: true,
            show_drawing_bounds: true,
            jog_step_mm: 1.0,
            error_message: startup_error,
            #[cfg(not(target_arch = "wasm32"))]
            pending_fallback_fonts: fonts::spawn_fallback_font_loader(),
            #[cfg(target_arch = "wasm32")]
            pending_svg_pick: None,
        };

        if let Some(svg) = startup_svg {
            app.load_svg(svg.file_name, svg.bytes);
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
        self.settings.sanitize();

        match paths::parse_svg(file_name, &bytes) {
            Ok(document) => {
                let loaded_svg = LoadedSvg { document };
                self.svg_placement =
                    Some(SvgPlacementState::for_svg(&loaded_svg, self.settings.printable_area));
                self.loaded_svg = Some(loaded_svg);
                self.rebuild_toolpath();
            }
            Err(error) => {
                self.loaded_svg = None;
                self.svg_placement = None;
                self.toolpath_plan = None;
                self.preview_progress = 0.0;
                self.preview_playing = false;
                self.error_message = Some(error.to_string());
            }
        }
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

        if self.svg_placement.is_none() {
            if let Some(svg) = self.loaded_svg.as_ref() {
                self.svg_placement =
                    Some(SvgPlacementState::for_svg(svg, self.settings.printable_area));
            }
        }

        let Some(svg) = self.loaded_svg.as_ref() else {
            self.toolpath_plan = None;
            return;
        };
        let Some(svg_placement) = self.svg_placement.as_mut() else {
            self.toolpath_plan = None;
            return;
        };
        svg_placement.placement.sanitize();

        let prepared = paths::prepare_svg(
            &svg.document,
            svg_placement.placement,
            self.settings.printable_area,
        );
        self.toolpath_plan = Some(gcode::build_plan(prepared, &self.settings));
        self.preview_progress = 1.0;
        self.preview_playing = false;
        self.error_message = None;
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

    fn current_timeline_position(&self) -> Option<glam::Vec2> {
        let plan = self.toolpath_plan.as_ref()?;
        let (_, _, pen_position) = plan.progress_state(self.preview_progress);
        Some(glam::vec2(pen_position.x, pen_position.y))
    }

    fn move_to_bounds_corner(&mut self, corners: Option<[glam::Vec2; 4]>, index: usize) {
        let Some(position) = corners.and_then(|corners| corners.get(index).copied()) else {
            return;
        };
        let result = self.device.move_to(position.x, position.y, self.settings.travel_feed_rate());
        self.apply_device_action(result);
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
                    egui::Grid::new("xy-jog-grid")
                        .spacing(egui::vec2(CONTROL_GRID_SPACING, CONTROL_GRID_SPACING))
                        .show(ui, |ui| {
                            spacer_button_cell(ui);
                            if control_button(ui, "↑", can_control).clicked() {
                                let result =
                                    self.device.jog_xy(0.0, self.jog_step_mm, jog_feed_rate);
                                self.apply_device_action(result);
                            }
                            spacer_button_cell(ui);
                            ui.end_row();

                            if control_button(ui, "←", can_control).clicked() {
                                let result =
                                    self.device.jog_xy(-self.jog_step_mm, 0.0, jog_feed_rate);
                                self.apply_device_action(result);
                            }
                            if control_button(ui, "🏠", can_control).clicked() {
                                let result = self.device.home_xy();
                                self.apply_device_action(result);
                            }
                            if control_button(ui, "→", can_control).clicked() {
                                let result =
                                    self.device.jog_xy(self.jog_step_mm, 0.0, jog_feed_rate);
                                self.apply_device_action(result);
                            }
                            ui.end_row();

                            spacer_button_cell(ui);
                            if control_button(ui, "↓", can_control).clicked() {
                                let result =
                                    self.device.jog_xy(0.0, -self.jog_step_mm, jog_feed_rate);
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
                    egui::Grid::new("z-jog-grid")
                        .spacing(egui::vec2(CONTROL_GRID_SPACING, CONTROL_GRID_SPACING))
                        .show(ui, |ui| {
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
                let log_reserved_height = (ui.available_height() * 0.30).clamp(180.0, 360.0);
                let controls_height = (ui.available_height() - log_reserved_height).max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt("settings-sidebar-scroll")
                    .max_height(controls_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(sidebar_width);
                        ui.set_min_width(sidebar_width);

                        if ui.button("SVG 불러오기").clicked() {
                            self.pick_svg();
                        }
                        ui.small("파일 선택이나 드래그 드롭으로 SVG를 불러올 수 있습니다.");

                        if let Some(plan) = &self.toolpath_plan {
                            if ui.button("G-code 복사").clicked() {
                                ui.ctx().copy_text(plan.gcode_text());
                            }
                        }

                        ui.separator();
                        ui.heading("디바이스");

                        let has_device_support =
                            self.device.connection_state() != ConnectionState::Unsupported;
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
                            ui.horizontal_top(|ui| {
                                ui.add_sized([56.0, 0.0], egui::Label::new("펌웨어"));
                                ui.add_sized(
                                    [ui.available_width().max(0.0), 0.0],
                                    egui::Label::new(firmware).truncate(),
                                );
                            });
                        }

                        if let Some(area) = self.device.detected_area() {
                            ui.label(format!(
                                "감지된 사이즈: {:.0} x {:.0} mm",
                                area.width_mm, area.height_mm
                            ));
                        }

                        let mut print_start_mode_changed = false;
                        ui.add_enabled_ui(has_device_support, |ui| {
                            if ui.button("포트 새로고침").clicked() {
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
                                    && self.device.can_connect();
                            let can_disconnect = matches!(
                                self.device.connection_state(),
                                ConnectionState::Connecting | ConnectionState::Connected
                            );
                            ui.columns_sized([Size::remainder(1.0), Size::remainder(1.0)], |columns| {
                                if columns[0]
                                    .add_enabled(can_connect, egui::Button::new("연결"))
                                    .clicked()
                                {
                                    let result = self.device.connect();
                                    self.apply_device_action(result);
                                }

                                if columns[1]
                                    .add_enabled(can_disconnect, egui::Button::new("연결 해제"))
                                    .clicked()
                                {
                                    self.device.disconnect();
                                }
                            });

                            let can_start_print = self.toolpath_plan.is_some()
                                && self.device.is_connected()
                                && !self.device.is_job_active();
                            let can_stop_print = self.device.can_stop_print();
                            let first_draw_point =
                                self.toolpath_plan.as_ref().and_then(|plan| plan.first_draw_point);
                            ui.columns_sized([Size::remainder(1.0), Size::remainder(1.0)], |columns| {
                                if columns[0]
                                    .add_enabled(can_start_print, egui::Button::new("프린트 시작"))
                                    .clicked()
                                {
                                    if let Some(plan) = self.toolpath_plan.as_ref() {
                                        let gcode_lines = plan.gcode_lines.clone();
                                        let result = self.device.send_job(&gcode_lines);
                                        self.apply_device_action(result);
                                    }
                                }

                                if columns[1]
                                    .add_enabled(can_stop_print, egui::Button::new("프린트 정지"))
                                    .clicked()
                                {
                                    let result = self.device.stop_job();
                                    self.apply_device_action(result);
                                }
                            });

                            let mut start_with_home =
                                self.settings.print_start_mode == PrintStartMode::HomeBeforePrint;
                            if ui.checkbox(&mut start_with_home, "프린트 시작 전에 XY Home 이동").changed() {
                                self.settings.print_start_mode = if start_with_home {
                                    PrintStartMode::HomeBeforePrint
                                } else {
                                    PrintStartMode::DirectFromCurrentPosition
                                };
                                print_start_mode_changed = true;
                            }
                            if !start_with_home {
                                ui.small(
                                    "끄면 XY Home 없이 Z 리프트 후 첫 시작점으로 이동해 그리기 시작합니다.",
                                );
                            }

                            let can_move_to_first_draw_point = first_draw_point.is_some()
                                && self.device.is_connected()
                                && !self.device.is_job_active();
                            if ui
                                .add_enabled(
                                    can_move_to_first_draw_point,
                                    egui::Button::new("첫 시작점으로 이동"),
                                )
                                .clicked()
                            {
                                if let Some(first_draw_point) = first_draw_point {
                                    let result = self.device.home_xy_and_move_to(
                                        first_draw_point.x,
                                        first_draw_point.y,
                                        self.settings.lift_height_mm,
                                        self.settings.travel_feed_rate(),
                                    );
                                    self.apply_device_action(result);
                                }
                            }
                            ui.small("Z 리프트 후 Home하고 첫 번째 그리기 시작 위치로 이동합니다.");

                            let drawing_bounds_corners =
                                self.toolpath_plan.as_ref().map(drawing_bounds_corners);
                            let can_move_to_bounds_corner = drawing_bounds_corners.is_some()
                                && self.device.is_connected()
                                && !self.device.is_job_active();
                            ui.label("바운딩 박스 모서리");
                            egui::Grid::new("bounds-corner-move-grid")
                                .num_columns(2)
                                .spacing(egui::vec2(6.0, 4.0))
                                .show(ui, |ui| {
                                    if bounds_corner_button(
                                        ui,
                                        "좌상",
                                        drawing_bounds_corners,
                                        2,
                                        can_move_to_bounds_corner,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 2);
                                    }
                                    if bounds_corner_button(
                                        ui,
                                        "우상",
                                        drawing_bounds_corners,
                                        3,
                                        can_move_to_bounds_corner,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 3);
                                    }
                                    ui.end_row();

                                    if bounds_corner_button(
                                        ui,
                                        "좌하",
                                        drawing_bounds_corners,
                                        0,
                                        can_move_to_bounds_corner,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 0);
                                    }
                                    if bounds_corner_button(
                                        ui,
                                        "우하",
                                        drawing_bounds_corners,
                                        1,
                                        can_move_to_bounds_corner,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 1);
                                    }
                                    ui.end_row();
                                });
                            ui.small("현재 SVG 바운딩 박스의 각 모서리 XY 좌표로 절대 이동합니다.");
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
                        let mut prefer_g2g3 = self.settings.curve_output_mode.prefers_g2g3();
                        let mut prefer_g5 = self.settings.curve_output_mode.prefers_g5();
                        let prefer_g2g3_changed =
                            ui.checkbox(&mut prefer_g2g3, "원호 G-code 사용 (G2/G3)").changed();
                        let prefer_g5_changed =
                            ui.checkbox(&mut prefer_g5, "Bezier G-code 사용 (G5)").changed();
                        if prefer_g2g3_changed || prefer_g5_changed {
                            self.settings.curve_output_mode =
                                CurveOutputMode::from_flags(prefer_g2g3, prefer_g5);
                            settings_changed = true;
                        }
                        if prefer_g2g3 && prefer_g5 {
                            ui.small("코너 둥글림으로 만든 원호는 G2/G3로, 베지어 곡선은 G5로 내보냅니다.");
                        } else if prefer_g2g3 {
                            ui.small("코너 둥글림으로 만든 원호만 G2/G3로 내보내고, 나머지는 선분으로 유지합니다.");
                        } else if prefer_g5 {
                            ui.small("지원 펌웨어에서는 곡선을 G5로 내보내고, 미리보기는 동일한 IR에서 계산합니다.");
                        }

                        if ui
                            .checkbox(
                                &mut self.settings.corner_smoothing_enabled,
                                "급한 코너를 미세하게 둥글게 처리",
                            )
                            .changed()
                        {
                            settings_changed = true;
                        }
                        if self.settings.corner_smoothing_enabled {
                            settings_changed |= drag_value_row(
                                ui,
                                "코너 둥글림 반경 (mm)",
                                &mut self.settings.corner_smoothing_radius_mm,
                                0.05,
                                0.1..=10.0,
                            );
                            settings_changed |= drag_value_row(
                                ui,
                                "코너 둥글림 시작 각도 (°)",
                                &mut self.settings.corner_smoothing_angle_deg,
                                1.0,
                                5.0..=170.0,
                            );
                            ui.small("선분-곡선-원호 연결부를 포함해, 끝 접선 각도가 이 값 이상일 때만 짧은 원호를 넣습니다.");
                        }

                        ui.separator();
                        ui.heading("SVG 배치");

                        let current_svg_size = self.toolpath_plan.as_ref().map(|plan| plan.drawing_bounds);
                        let svg_is_out_of_bounds =
                            self.toolpath_plan.as_ref().is_some_and(|plan| plan.is_out_of_bounds);
                        let mut placement_changed = false;

                        if let Some(svg_placement) = self.svg_placement.as_mut() {
                            let mut center_x = svg_placement.placement.center_mm.x;
                            let mut center_y = svg_placement.placement.center_mm.y;
                            let mut scale_percent = svg_placement.scale_percent();

                            let center_x_changed = drag_value_row(
                                ui,
                                "SVG 중심 X (mm)",
                                &mut center_x,
                                1.0,
                                -5_000.0..=5_000.0,
                            );
                            let center_y_changed = drag_value_row(
                                ui,
                                "SVG 중심 Y (mm)",
                                &mut center_y,
                                1.0,
                                -5_000.0..=5_000.0,
                            );
                            let scale_changed = drag_value_row(
                                ui,
                                "SVG 크기 (%)",
                                &mut scale_percent,
                                1.0,
                                1.0..=1_000.0,
                            );

                            if center_x_changed {
                                svg_placement.placement.center_mm.x = center_x;
                                placement_changed = true;
                            }
                            if center_y_changed {
                                svg_placement.placement.center_mm.y = center_y;
                                placement_changed = true;
                            }
                            if scale_changed {
                                svg_placement.set_scale_percent(scale_percent);
                                placement_changed = true;
                            }

                            if let Some(size) = current_svg_size {
                                ui.small(format!("현재 크기: {:.1} x {:.1} mm", size.x, size.y));
                            }
                            ui.small("100%는 SVG 좌표 1단위를 1mm로 본 초기 크기입니다.");
                            if svg_is_out_of_bounds {
                                ui.colored_label(
                                    colors::warning(),
                                    "SVG가 현재 프린트 가능 영역을 벗어났습니다.",
                                );
                            }
                        } else {
                            ui.small("SVG를 불러오면 위치와 크기를 조절할 수 있습니다.");
                        }

                        if settings_changed || placement_changed || print_start_mode_changed {
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
                    });
                ui.separator();
                self.show_device_log(ui);
            });
    }

    fn show_device_log(&self, ui: &mut egui::Ui) {
        ui.heading("장치 로그");
        let log_width = ui.available_width();
        egui::ScrollArea::vertical()
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(log_width);
                for line in self.device.log_lines().rev() {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        ui.add(egui::Label::new(line).wrap().halign(egui::Align::Min));
                    });
                }
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
            egui::Frame::canvas(ui.style()).show(ui, |ui| {
                let max_rect = ui.max_rect();
                let preview_size =
                    egui::vec2(max_rect.width().max(1.0), max_rect.height().max(1.0));
                ui.set_min_size(preview_size);
                let preview_rect = self.preview_renderer.show(
                    ui,
                    preview_size,
                    self.toolpath_plan.as_ref(),
                    self.preview_progress,
                    self.show_travel_moves,
                    self.show_drawing_bounds,
                    &mut self.viewport_state,
                );
                self.show_preview_controls_overlay(ui, preview_rect, &timeline_text);
            });
        });
    }

    fn show_preview_controls_overlay(
        &mut self,
        ui: &mut egui::Ui,
        preview_rect: egui::Rect,
        timeline_text: &str,
    ) {
        let available_height = preview_rect.height() - PREVIEW_CONTROL_OVERLAY_MARGIN * 2.0;
        let available_width = preview_rect.width() - PREVIEW_CONTROL_OVERLAY_MARGIN * 2.0;
        if available_height <= 0.0 || available_width <= 0.0 {
            return;
        }

        let overlay_height = available_height.min(PREVIEW_CONTROL_BAND_HEIGHT);
        let overlay_rect = egui::Rect::from_min_size(
            egui::pos2(
                preview_rect.left() + PREVIEW_CONTROL_OVERLAY_MARGIN,
                preview_rect.bottom() - PREVIEW_CONTROL_OVERLAY_MARGIN - overlay_height,
            ),
            egui::vec2(available_width, overlay_height),
        );
        egui::Area::new(egui::Id::new("preview-controls-overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(overlay_rect.min)
            .show(ui.ctx(), |ui| {
                let (area_rect, _) =
                    ui.allocate_exact_size(overlay_rect.size(), egui::Sense::hover());
                ui.painter().rect_filled(area_rect, 10.0, colors::preview_overlay_background());

                let inner_rect = area_rect.shrink2(egui::vec2(12.0, 10.0));
                let mut overlay_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                overlay_ui.set_width(inner_rect.width());

                overlay_ui.horizontal(|ui| {
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

                    let can_move_to_timeline_position = self.current_timeline_position().is_some()
                        && self.device.is_connected()
                        && !self.device.is_job_active();
                    if ui
                        .add_enabled(
                            can_move_to_timeline_position,
                            egui::Button::new("현재 위치로 이동"),
                        )
                        .clicked()
                    {
                        if let Some(position) = self.current_timeline_position() {
                            self.preview_playing = false;
                            let result = self.device.move_to(
                                position.x,
                                position.y,
                                self.settings.travel_feed_rate(),
                            );
                            self.apply_device_action(result);
                        }
                    }

                    ui.checkbox(&mut self.show_travel_moves, "펜 리프트 이동 경로 표시");
                    ui.checkbox(&mut self.show_drawing_bounds, "바운딩 박스 표시");

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(timeline_text);
                    });
                });

                overlay_ui.add_space(8.0);
                overlay_ui.scope(|ui| {
                    ui.spacing_mut().slider_width = ui.available_width();
                    let current_interact_height = ui.spacing().interact_size.y;
                    ui.spacing_mut().interact_size.y = current_interact_height.max(28.0);
                    let slider =
                        egui::Slider::new(&mut self.preview_progress, 0.0..=1.0).show_value(false);
                    let mut changed = false;
                    ui.add_enabled_ui(self.toolpath_plan.is_some(), |ui| {
                        changed = ui.add_sized([ui.available_width(), 28.0], slider).changed();
                    });
                    if changed {
                        self.preview_playing = false;
                    }
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
        } else if self.device.needs_poll() {
            ctx.request_repaint_after(Duration::from_millis(100));
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

fn drawing_bounds_corners(plan: &ToolpathPlan) -> [glam::Vec2; 4] {
    let min = plan.drawing_origin;
    let max = plan.drawing_origin + plan.drawing_bounds;
    [
        glam::vec2(min.x, min.y),
        glam::vec2(max.x, min.y),
        glam::vec2(min.x, max.y),
        glam::vec2(max.x, max.y),
    ]
}

fn bounds_corner_button(
    ui: &mut egui::Ui,
    label: &str,
    corners: Option<[glam::Vec2; 4]>,
    index: usize,
    enabled: bool,
) -> egui::Response {
    let tooltip = corners
        .and_then(|corners| corners.get(index).copied())
        .map(|corner| format!("{label}: X {:.2}, Y {:.2}", corner.x, corner.y))
        .unwrap_or_else(|| "SVG를 불러오면 사용할 수 있습니다.".to_owned());
    ui.add_enabled(enabled, egui::Button::new(label).min_size(egui::vec2(64.0, 28.0)))
        .on_hover_text(tooltip)
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
