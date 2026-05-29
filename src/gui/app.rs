use super::fonts;
#[cfg(not(target_arch = "wasm32"))]
use super::fonts::LoadedFallbackFonts;
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::layout::{Size, UiLayoutExt};
use super::viewer::{
    ManipulationMode, PreviewManipulation, PreviewObjectBounds, PreviewRenderer, ViewportState,
    project_bed_point,
};
use crate::{
    paths,
    platform::device::{
        ConnectionMethod, ConnectionState, DeviceController, DevicePreferences, PrintState,
    },
    plot::{
        gcode,
        model::{
            CurveOutputMode, FillPattern, PrintStartMode, PrintableArea, SvgPlacement,
            ToolSettings, ToolpathPlan,
        },
    },
    res::{
        colors,
        lang::{Language, Strings},
    },
};

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{self, TryRecvError};

#[cfg(target_arch = "wasm32")]
use poll_promise::Promise;

const SIDEBAR_WIDTH: f32 = 360.0;
const PREVIEW_CONTROL_BAND_HEIGHT: f32 = 72.0;
const PREVIEW_CONTROL_OVERLAY_MARGIN: f32 = 12.0;
const CONTROL_BUTTON_WIDTH: f32 = 44.0;
const CONTROL_BUTTON_HEIGHT: f32 = 44.0;
const CONTROL_GRID_SPACING: f32 = 4.0;
const APP_STATE_STORAGE_KEY: &str = eframe::APP_KEY;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PersistedAppState {
    language: Language,
    device: DevicePreferences,
    curve_output_mode: CurveOutputMode,
}

impl Default for PersistedAppState {
    fn default() -> Self {
        Self {
            language: Language::default(),
            device: DevicePreferences::default(),
            curve_output_mode: CurveOutputMode::default(),
        }
    }
}

pub struct PenarticApp {
    language: Language,
    settings: ToolSettings,
    device: DeviceController,
    preview_renderer: PreviewRenderer,
    viewport_state: ViewportState,
    svg_objects: Vec<SvgObject>,
    selected_svg_id: Option<u64>,
    next_svg_id: u64,
    manipulation_mode: ManipulationMode,
    scale_aspect_locked: bool,
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
    file_name: String,
    bytes: Vec<u8>,
    document: paths::ParsedSvg,
}

#[derive(Clone, Copy)]
struct SvgPlacementState {
    placement: SvgPlacement,
    native_scale_mm_per_unit: glam::Vec2,
}

#[derive(Clone)]
struct SvgObject {
    id: u64,
    loaded_svg: LoadedSvg,
    placement: SvgPlacementState,
    prepared_bounds: Option<PreparedObjectBounds>,
}

#[derive(Clone, Copy)]
struct PreparedObjectBounds {
    origin: glam::Vec2,
    size: glam::Vec2,
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

fn load_persisted_app_state(storage: &dyn eframe::Storage) -> Option<PersistedAppState> {
    eframe::get_value::<PersistedAppState>(storage, APP_STATE_STORAGE_KEY).or_else(|| {
        storage.get_string(APP_STATE_STORAGE_KEY).and_then(|value| {
            Language::from_storage_key(&value)
                .map(|language| PersistedAppState { language, ..PersistedAppState::default() })
        })
    })
}

impl SvgPlacementState {
    fn for_svg(svg: &LoadedSvg, printable_area: PrintableArea) -> Self {
        let placement = svg.document.centered_native_placement(printable_area);
        Self { native_scale_mm_per_unit: placement.scale_mm_per_unit, placement }
    }

    fn scale_percent(&self) -> glam::Vec2 {
        (self.placement.scale_mm_per_unit
            / self.native_scale_mm_per_unit.max(glam::Vec2::splat(1e-4))
            * 100.0)
            .max(glam::Vec2::ONE)
    }

    fn set_scale_percent(&mut self, scale_percent: glam::Vec2) {
        self.placement.scale_mm_per_unit =
            (self.native_scale_mm_per_unit.max(glam::Vec2::splat(1e-4))
                * (scale_percent.max(glam::Vec2::ONE) / 100.0))
                .max(glam::Vec2::splat(1e-4));
    }

    fn local_size_mm(&self, svg: &LoadedSvg) -> glam::Vec2 {
        svg.document.source_size() * self.placement.scale_mm_per_unit
    }

    fn set_local_size_mm(&mut self, svg: &LoadedSvg, size_mm: glam::Vec2) {
        let source_size = svg.document.source_size().max(glam::Vec2::splat(1e-4));
        self.placement.scale_mm_per_unit =
            (size_mm.max(glam::Vec2::splat(0.01)) / source_size).max(glam::Vec2::splat(1e-4));
    }
}

impl PenarticApp {
    fn text(&self) -> &'static Strings {
        self.language.strings()
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        preview_msaa_samples: u32,
        startup_svg: Option<StartupSvg>,
        startup_error: Option<String>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        #[cfg(target_arch = "wasm32")]
        fonts::apply_fallback_fonts(&cc.egui_ctx, fonts::web_fallback_fonts());

        let persisted = cc.storage.and_then(load_persisted_app_state).unwrap_or_default();
        let language = persisted.language;
        let mut device = DeviceController::new(language, persisted.device);
        if device.connection_method() == ConnectionMethod::Serial {
            device.refresh_ports();
        }

        let mut app = Self {
            language,
            settings: ToolSettings {
                curve_output_mode: persisted.curve_output_mode,
                ..ToolSettings::default()
            },
            device,
            preview_renderer: PreviewRenderer::new(cc, preview_msaa_samples),
            viewport_state: ViewportState::default(),
            svg_objects: Vec::new(),
            selected_svg_id: None,
            next_svg_id: 1,
            manipulation_mode: ManipulationMode::Move,
            scale_aspect_locked: true,
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

    fn set_language(&mut self, language: Language) {
        if self.language == language {
            return;
        }

        self.language = language;
        self.device.set_language(language);

        let mut had_error = false;
        for object in &mut self.svg_objects {
            match paths::parse_svg_with_language(
                object.loaded_svg.file_name.clone(),
                &object.loaded_svg.bytes,
                language,
            ) {
                Ok(document) => {
                    object.loaded_svg.document = document;
                }
                Err(error) => {
                    self.error_message = Some(error.localized_message(language));
                    had_error = true;
                }
            }
        }
        if !had_error {
            self.rebuild_toolpath();
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
                        self.error_message = Some(self.text().read_svg_file_failed(error));
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

        match paths::parse_svg_with_language(file_name.clone(), &bytes, self.language) {
            Ok(document) => {
                let loaded_svg = LoadedSvg { file_name, bytes, document };
                let id = self.next_svg_id;
                self.next_svg_id += 1;
                let placement =
                    SvgPlacementState::for_svg(&loaded_svg, self.settings.printable_area);
                self.svg_objects.push(SvgObject {
                    id,
                    loaded_svg,
                    placement,
                    prepared_bounds: None,
                });
                self.selected_svg_id = Some(id);
                self.rebuild_toolpath();
            }
            Err(error) => {
                self.preview_progress = 0.0;
                self.preview_playing = false;
                self.error_message = Some(error.localized_message(self.language));
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
                self.error_message = Some(self.text().only_svg_drag_drop.to_owned());
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
                    .map_err(|error| self.text().read_dropped_svg_file_failed(error))?;
                self.load_svg(file_name, bytes);
                return Ok(true);
            }

            return Err(self.text().dropped_svg_not_ready.to_owned());
        }

        Ok(false)
    }

    fn rebuild_toolpath(&mut self) {
        self.settings.sanitize();

        if self.svg_objects.is_empty() {
            self.toolpath_plan = None;
            return;
        }

        let mut prepared_svgs = Vec::with_capacity(self.svg_objects.len());
        for object in &mut self.svg_objects {
            object.placement.placement.sanitize();
            let prepared = paths::prepare_svg(
                &object.loaded_svg.document,
                object.placement.placement,
                self.settings.printable_area,
            );
            object.prepared_bounds = Some(PreparedObjectBounds {
                origin: prepared.drawing_origin,
                size: prepared.drawing_bounds,
            });
            prepared_svgs.push(prepared);
        }

        let prepared = combine_prepared_svgs(prepared_svgs, self.settings.printable_area);
        self.toolpath_plan =
            Some(gcode::build_plan_with_language(prepared, &self.settings, self.language));
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

    fn selected_svg_mut(&mut self) -> Option<&mut SvgObject> {
        let selected_id = self.selected_svg_id?;
        self.svg_objects.iter_mut().find(|object| object.id == selected_id)
    }

    fn delete_selected_svg(&mut self) {
        let Some(selected_id) = self.selected_svg_id else {
            return;
        };
        let before_len = self.svg_objects.len();
        self.svg_objects.retain(|object| object.id != selected_id);
        if self.svg_objects.len() == before_len {
            return;
        }
        self.selected_svg_id = self.svg_objects.last().map(|object| object.id);
        self.rebuild_toolpath();
    }

    fn handle_object_shortcuts(&mut self, ctx: &egui::Context) {
        if !ctx.egui_wants_keyboard_input()
            && ctx.input(|input| input.key_pressed(egui::Key::Delete))
        {
            self.delete_selected_svg();
        }
    }

    fn preview_object_bounds(&self) -> Vec<PreviewObjectBounds> {
        self.svg_objects
            .iter()
            .filter_map(|object| {
                let bounds = object.prepared_bounds?;
                Some(PreviewObjectBounds {
                    id: object.id,
                    center_mm: object.placement.placement.center_mm,
                    bounds_origin_mm: bounds.origin,
                    bounds_size_mm: bounds.size,
                })
            })
            .collect()
    }

    fn apply_preview_manipulation(&mut self, manipulation: PreviewManipulation) {
        match manipulation {
            PreviewManipulation::Select(id) => {
                self.selected_svg_id = Some(id);
            }
            PreviewManipulation::Move { id, delta_mm } => {
                if let Some(object) = self.svg_objects.iter_mut().find(|object| object.id == id) {
                    object.placement.placement.center_mm += delta_mm;
                    self.selected_svg_id = Some(id);
                    self.rebuild_toolpath();
                }
            }
            PreviewManipulation::Scale { id, factor } => {
                if let Some(object) = self.svg_objects.iter_mut().find(|object| object.id == id) {
                    object.placement.placement.scale_mm_per_unit =
                        (object.placement.placement.scale_mm_per_unit * factor)
                            .clamp(glam::Vec2::splat(1e-4), glam::Vec2::splat(1000.0));
                    self.selected_svg_id = Some(id);
                    self.rebuild_toolpath();
                }
            }
            PreviewManipulation::Rotate { id, delta_degrees } => {
                if let Some(object) = self.svg_objects.iter_mut().find(|object| object.id == id) {
                    object.placement.placement.rotation_degrees += delta_degrees;
                    self.selected_svg_id = Some(id);
                    self.rebuild_toolpath();
                }
            }
        }
    }

    fn move_to_bounds_corner(&mut self, corners: Option<[glam::Vec2; 4]>, index: usize) {
        let Some(position) = corners.and_then(|corners| corners.get(index).copied()) else {
            return;
        };
        let result = self.device.move_to(position.x, position.y, self.settings.travel_feed_rate());
        self.apply_device_action(result);
    }

    fn show_manual_controls(&mut self, ui: &mut egui::Ui) {
        let text = self.text();
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
            ui.label(text.jog_step);
            for step in [0.1_f32, 1.0, 10.0, 100.0] {
                ui.selectable_value(&mut self.jog_step_mm, step, format_jog_step(step));
            }
        });

        if ui.add_enabled(can_control, egui::Button::new(text.motors_off)).clicked() {
            let result = self.device.motors_off();
            self.apply_device_action(result);
        }

        if !can_control {
            ui.small(text.manual_control_unavailable);
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

                        let mut selected_language = self.language;
                        ui.horizontal(|ui| {
                            ui.label(self.text().language_label);
                            egui::ComboBox::from_id_salt("language-setting")
                                .width(ui.available_width().max(140.0))
                                .selected_text(selected_language.native_name())
                                .show_ui(ui, |ui| {
                                    for language in Language::ALL {
                                        ui.selectable_value(
                                            &mut selected_language,
                                            language,
                                            language.native_name(),
                                        );
                                    }
                                });
                        });
                        if selected_language != self.language {
                            self.set_language(selected_language);
                        }
                        let language = self.language;
                        let text = self.text();

                        if ui.button(text.load_svg).clicked() {
                            self.pick_svg();
                        }
                        ui.small(text.load_svg_hint);

                        if let Some(plan) = &self.toolpath_plan {
                            if ui.button(text.copy_gcode).clicked() {
                                ui.ctx().copy_text(plan.gcode_text());
                            }
                        }

                        ui.separator();
                        ui.heading(text.device_heading);

                        let has_device_support =
                            self.device.connection_state() != ConnectionState::Unsupported;
                        let status_color = connection_status_color(&self.device);
                        ui.horizontal(|ui| {
                            ui.label(text.status_label);
                            ui.colored_label(
                                status_color,
                                format!("● {}", self.device.status_text()),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(text.job_label);
                            ui.colored_label(
                                print_state_color(self.device.print_state()),
                                self.device.print_state_text(),
                            );
                        });

                        if let Some(firmware) = self.device.firmware_summary() {
                            ui.horizontal_top(|ui| {
                                ui.add_sized([56.0, 0.0], egui::Label::new(text.firmware_label));
                                ui.add_sized(
                                    [ui.available_width().max(0.0), 0.0],
                                    egui::Label::new(firmware).truncate(),
                                );
                            });
                        }

                        if let Some(area) = self.device.detected_area() {
                            ui.label(text.detected_size(area.width_mm, area.height_mm));
                        }

                        let mut print_start_mode_changed = false;
                        ui.add_enabled_ui(has_device_support, |ui| {
                            let can_change_connection_target = matches!(
                                self.device.connection_state(),
                                ConnectionState::Disconnected
                            );
                            ui.label(text.connection_method);
                            let previous_method = self.device.connection_method();
                            let mut connection_method = previous_method;
                            ui.add_enabled_ui(can_change_connection_target, |ui| {
                                egui::ComboBox::from_id_salt("connection-method-combo")
                                    .width(ui.available_width())
                                    .selected_text(connection_method.label(self.language))
                                    .show_ui(ui, |ui| {
                                        for method in ConnectionMethod::available() {
                                            ui.selectable_value(
                                                &mut connection_method,
                                                *method,
                                                method.label(self.language),
                                            );
                                        }
                                    });
                            });
                            if can_change_connection_target && connection_method != previous_method
                            {
                                self.device.set_connection_method(connection_method);
                                if connection_method == ConnectionMethod::Serial {
                                    self.device.refresh_ports();
                                }
                            }

                            match self.device.connection_method() {
                                ConnectionMethod::Serial => {
                                    if ui
                                        .add_enabled(
                                            can_change_connection_target,
                                            egui::Button::new(text.refresh_ports),
                                        )
                                        .clicked()
                                    {
                                        self.device.refresh_ports();
                                    }

                                    let ports = self.device.serial_ports().to_vec();
                                    let combo_width = ui.available_width();
                                    egui::ComboBox::from_id_salt("serial-port-combo")
                                        .width(combo_width)
                                        .selected_text(
                                            self.device
                                                .selected_serial_port()
                                                .unwrap_or(text.select_port),
                                        )
                                        .show_ui(ui, |ui| {
                                            for port in ports {
                                                let selected = self.device.selected_serial_port()
                                                    == Some(port.as_str());
                                                if ui.selectable_label(selected, &port).clicked() {
                                                    self.device.set_selected_serial_port(Some(
                                                        port.clone(),
                                                    ));
                                                }
                                            }
                                        });
                                }
                                ConnectionMethod::Esp3d => {
                                    ui.label(text.esp3d_address);
                                    let mut endpoint = self.device.esp3d_endpoint().to_owned();
                                    if ui
                                        .add_enabled(
                                            can_change_connection_target,
                                            egui::TextEdit::singleline(&mut endpoint)
                                                .desired_width(ui.available_width()),
                                        )
                                        .changed()
                                    {
                                        self.device.set_esp3d_endpoint(endpoint);
                                    }
                                }
                                ConnectionMethod::OctoPrint => {
                                    ui.label(text.octoprint_address);
                                    let mut base_url = self.device.octoprint_base_url().to_owned();
                                    if ui
                                        .add_enabled(
                                            can_change_connection_target,
                                            egui::TextEdit::singleline(&mut base_url)
                                                .desired_width(ui.available_width()),
                                        )
                                        .changed()
                                    {
                                        self.device.set_octoprint_base_url(base_url);
                                    }

                                    ui.label(text.octoprint_api_key);
                                    let mut api_key = self.device.octoprint_api_key().to_owned();
                                    if ui
                                        .add_enabled(
                                            can_change_connection_target,
                                            egui::TextEdit::singleline(&mut api_key)
                                                .password(true)
                                                .desired_width(ui.available_width()),
                                        )
                                        .changed()
                                    {
                                        self.device.set_octoprint_api_key(api_key);
                                    }
                                }
                            }

                            let can_connect = matches!(
                                self.device.connection_state(),
                                ConnectionState::Disconnected
                            ) && self.device.can_connect();
                            let can_disconnect = matches!(
                                self.device.connection_state(),
                                ConnectionState::Connecting | ConnectionState::Connected
                            );
                            ui.columns_sized(
                                [Size::remainder(1.0), Size::remainder(1.0)],
                                |columns| {
                                    if columns[0]
                                        .add_enabled(can_connect, egui::Button::new(text.connect))
                                        .clicked()
                                    {
                                        let result = self.device.connect();
                                        self.apply_device_action(result);
                                    }

                                    if columns[1]
                                        .add_enabled(
                                            can_disconnect,
                                            egui::Button::new(text.disconnect),
                                        )
                                        .clicked()
                                    {
                                        self.device.disconnect();
                                    }
                                },
                            );

                            let can_start_print = self.toolpath_plan.is_some()
                                && self.device.is_connected()
                                && !self.device.is_job_active();
                            let can_stop_print = self.device.can_stop_print();
                            let first_draw_point =
                                self.toolpath_plan.as_ref().and_then(|plan| plan.first_draw_point);
                            ui.columns_sized(
                                [Size::remainder(1.0), Size::remainder(1.0)],
                                |columns| {
                                    if columns[0]
                                        .add_enabled(
                                            can_start_print,
                                            egui::Button::new(text.start_print),
                                        )
                                        .clicked()
                                    {
                                        if let Some(plan) = self.toolpath_plan.as_ref() {
                                            let gcode_lines = plan.gcode_lines.clone();
                                            let result = self.device.send_job(&gcode_lines);
                                            self.apply_device_action(result);
                                        }
                                    }

                                    if columns[1]
                                        .add_enabled(
                                            can_stop_print,
                                            egui::Button::new(text.stop_print),
                                        )
                                        .clicked()
                                    {
                                        let result = self.device.stop_job();
                                        self.apply_device_action(result);
                                    }
                                },
                            );

                            let mut start_with_home =
                                self.settings.print_start_mode == PrintStartMode::HomeBeforePrint;
                            if ui
                                .checkbox(&mut start_with_home, text.home_xy_before_print)
                                .changed()
                            {
                                self.settings.print_start_mode = if start_with_home {
                                    PrintStartMode::HomeBeforePrint
                                } else {
                                    PrintStartMode::DirectFromCurrentPosition
                                };
                                print_start_mode_changed = true;
                            }
                            if !start_with_home {
                                ui.small(text.direct_start_without_home_hint);
                            }

                            let can_move_to_first_draw_point = first_draw_point.is_some()
                                && self.device.is_connected()
                                && !self.device.is_job_active();
                            if ui
                                .add_enabled(
                                    can_move_to_first_draw_point,
                                    egui::Button::new(text.move_to_first_start_point),
                                )
                                .clicked()
                            {
                                if let Some(first_draw_point) = first_draw_point {
                                    let result = self.device.move_to_first_start(
                                        first_draw_point.x,
                                        first_draw_point.y,
                                        self.settings.travel_feed_rate(),
                                    );
                                    self.apply_device_action(result);
                                }
                            }

                            let can_move_to_timeline_position =
                                self.current_timeline_position().is_some()
                                    && self.device.is_connected()
                                    && !self.device.is_job_active();
                            if ui
                                .add_enabled(
                                    can_move_to_timeline_position,
                                    egui::Button::new(text.move_to_current_position),
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

                            let drawing_bounds_corners =
                                self.toolpath_plan.as_ref().map(drawing_bounds_corners);
                            let can_move_to_bounds_corner = drawing_bounds_corners.is_some()
                                && self.device.is_connected()
                                && !self.device.is_job_active();
                            ui.label(text.bounding_box_corners);
                            egui::Grid::new("bounds-corner-move-grid")
                                .num_columns(2)
                                .spacing(egui::vec2(6.0, 4.0))
                                .show(ui, |ui| {
                                    if bounds_corner_button(
                                        ui,
                                        text.top_left,
                                        drawing_bounds_corners,
                                        2,
                                        can_move_to_bounds_corner,
                                        language,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 2);
                                    }
                                    if bounds_corner_button(
                                        ui,
                                        text.top_right,
                                        drawing_bounds_corners,
                                        3,
                                        can_move_to_bounds_corner,
                                        language,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 3);
                                    }
                                    ui.end_row();

                                    if bounds_corner_button(
                                        ui,
                                        text.bottom_left,
                                        drawing_bounds_corners,
                                        0,
                                        can_move_to_bounds_corner,
                                        language,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 0);
                                    }
                                    if bounds_corner_button(
                                        ui,
                                        text.bottom_right,
                                        drawing_bounds_corners,
                                        1,
                                        can_move_to_bounds_corner,
                                        language,
                                    )
                                    .clicked()
                                    {
                                        self.move_to_bounds_corner(drawing_bounds_corners, 1);
                                    }
                                    ui.end_row();
                                });
                        });

                        self.show_manual_controls(ui);

                        ui.separator();
                        ui.heading(text.settings_heading);

                        let mut settings_changed = false;
                        settings_changed |= drag_value_row(
                            ui,
                            text.printable_width,
                            &mut self.settings.printable_area.width_mm,
                            1.0,
                            10.0..=1_000.0,
                        );
                        settings_changed |= drag_value_row(
                            ui,
                            text.printable_height,
                            &mut self.settings.printable_area.height_mm,
                            1.0,
                            10.0..=1_000.0,
                        );
                        settings_changed |= drag_value_row(
                            ui,
                            text.print_speed,
                            &mut self.settings.print_speed_mm_s,
                            1.0,
                            1.0..=500.0,
                        );
                        settings_changed |= drag_value_row(
                            ui,
                            text.z_lift,
                            &mut self.settings.lift_height_mm,
                            0.1,
                            0.1..=25.0,
                        );
                        let mut prefer_g2g3 = self.settings.curve_output_mode.prefers_g2g3();
                        let mut prefer_g5 = self.settings.curve_output_mode.prefers_g5();
                        let prefer_g2g3_changed =
                            ui.checkbox(&mut prefer_g2g3, text.use_arc_gcode).changed();
                        let prefer_g5_changed =
                            ui.checkbox(&mut prefer_g5, text.use_bezier_gcode).changed();
                        if prefer_g2g3_changed || prefer_g5_changed {
                            self.settings.curve_output_mode =
                                CurveOutputMode::from_flags(prefer_g2g3, prefer_g5);
                            settings_changed = true;
                        }
                        if prefer_g2g3 && prefer_g5 {
                            ui.small(text.arc_and_bezier_export_hint);
                        } else if prefer_g2g3 {
                            ui.small(text.arc_export_hint);
                        } else if prefer_g5 {
                            ui.small(text.bezier_export_hint);
                        }

                        if ui
                            .checkbox(
                                &mut self.settings.corner_smoothing_enabled,
                                text.round_sharp_corners,
                            )
                            .changed()
                        {
                            settings_changed = true;
                        }
                        if self.settings.corner_smoothing_enabled {
                            settings_changed |= drag_value_row(
                                ui,
                                text.corner_rounding_radius,
                                &mut self.settings.corner_smoothing_radius_mm,
                                0.05,
                                0.1..=10.0,
                            );
                            settings_changed |= drag_value_row(
                                ui,
                                text.corner_rounding_start_angle,
                                &mut self.settings.corner_smoothing_angle_deg,
                                1.0,
                                5.0..=170.0,
                            );
                            ui.small(text.corner_rounding_hint);
                        }

                        if ui
                            .checkbox(&mut self.settings.fill_enabled, text.fill_closed_shapes)
                            .changed()
                        {
                            settings_changed = true;
                        }
                        if self.settings.fill_enabled {
                            let previous_pattern = self.settings.fill_pattern;
                            egui::ComboBox::from_id_salt("fill-pattern-combo")
                                .selected_text(fill_pattern_label(self.settings.fill_pattern, text))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.settings.fill_pattern,
                                        FillPattern::Lines,
                                        text.fill_pattern_lines,
                                    );
                                    ui.selectable_value(
                                        &mut self.settings.fill_pattern,
                                        FillPattern::Crosshatch,
                                        text.fill_pattern_crosshatch,
                                    );
                                    ui.selectable_value(
                                        &mut self.settings.fill_pattern,
                                        FillPattern::Zigzag,
                                        text.fill_pattern_zigzag,
                                    );
                                    ui.selectable_value(
                                        &mut self.settings.fill_pattern,
                                        FillPattern::ContinuousZigzag,
                                        text.fill_pattern_continuous_zigzag,
                                    );
                                });
                            settings_changed |= previous_pattern != self.settings.fill_pattern;
                            settings_changed |= drag_value_row(
                                ui,
                                text.fill_density,
                                &mut self.settings.fill_density_percent,
                                1.0,
                                1.0..=100.0,
                            );
                            settings_changed |= drag_value_row(
                                ui,
                                text.fill_angle,
                                &mut self.settings.fill_angle_degrees,
                                1.0,
                                0.0..=179.0,
                            );
                            ui.small(text.fill_density_hint);
                        }

                        if settings_changed || print_start_mode_changed {
                            self.rebuild_toolpath();
                        }

                        ui.separator();
                        ui.heading(text.job_info_heading);

                        if let Some(plan) = &self.toolpath_plan {
                            ui.label(format!("SVG: {}", plan.source_name));
                            ui.label(
                                text.drawing_bounds(plan.drawing_bounds.x, plan.drawing_bounds.y),
                            );
                            ui.label(text.stroke_count(plan.stats.stroke_count));
                            ui.label(text.segment_count(plan.stats.segment_count));
                            ui.label(text.drawing_distance(plan.stats.drawing_distance_mm));
                            ui.label(text.travel_distance(plan.stats.travel_distance_mm));
                            ui.label(text.estimated_duration(plan.stats.estimated_duration_s));

                            if !plan.warnings.is_empty() {
                                ui.separator();
                                for warning in &plan.warnings {
                                    ui.colored_label(colors::warning(), warning);
                                }
                            }
                        } else {
                            ui.label(text.no_converted_svg);
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
        ui.heading(self.text().device_log_heading);
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
        let language = self.language;
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
                let object_bounds = self.preview_object_bounds();
                let preview_output = self.preview_renderer.show(
                    ui,
                    preview_size,
                    self.toolpath_plan.as_ref(),
                    self.preview_progress,
                    self.show_travel_moves,
                    self.show_drawing_bounds,
                    language,
                    &mut self.viewport_state,
                    &object_bounds,
                    self.selected_svg_id,
                    self.manipulation_mode,
                );
                if let Some(manipulation) = preview_output.manipulation {
                    self.apply_preview_manipulation(manipulation);
                }
                self.show_object_toolbar_overlay(
                    ui,
                    preview_output.rect,
                    preview_output.view_projection,
                    &object_bounds,
                );
                self.show_preview_controls_overlay(ui, preview_output.rect, &timeline_text);
            });
        });
    }

    fn show_object_toolbar_overlay(
        &mut self,
        ui: &mut egui::Ui,
        preview_rect: egui::Rect,
        view_projection: glam::Mat4,
        object_bounds: &[PreviewObjectBounds],
    ) {
        if preview_rect.width() <= 0.0 || preview_rect.height() <= 0.0 {
            return;
        }
        let text = self.text();

        let toolbar_rect = egui::Rect::from_min_size(
            preview_rect.min + egui::vec2(8.0, 6.0),
            egui::vec2((preview_rect.width() - 16.0).max(1.0), 64.0),
        );
        egui::Area::new(egui::Id::new("object-toolbar-overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(toolbar_rect.min)
            .show(ui.ctx(), |ui| {
                let (area_rect, _) =
                    ui.allocate_exact_size(toolbar_rect.size(), egui::Sense::hover());
                ui.painter().rect_filled(area_rect, 6.0, colors::preview_overlay_background());
                let inner_rect = area_rect.shrink2(egui::vec2(10.0, 8.0));
                let mut toolbar_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(inner_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                toolbar_ui.set_height(inner_rect.height());
                toolbar_ui
                    .selectable_value(&mut self.manipulation_mode, ManipulationMode::Move, "↔")
                    .on_hover_text(text.object_move_tool);
                toolbar_ui
                    .selectable_value(&mut self.manipulation_mode, ManipulationMode::Scale, "□")
                    .on_hover_text(text.object_scale_tool);
                toolbar_ui
                    .selectable_value(&mut self.manipulation_mode, ManipulationMode::Rotate, "⟳")
                    .on_hover_text(text.object_rotate_tool);
                toolbar_ui.separator();

                let mut placement_changed = false;
                let mut scale_aspect_locked = self.scale_aspect_locked;
                if let Some(object) = self.selected_svg_mut() {
                    let mut position = object.placement.placement.center_mm;
                    let mut scale_percent = object.placement.scale_percent();
                    let mut local_size_mm = object.placement.local_size_mm(&object.loaded_svg);
                    let original_scale_percent = scale_percent;
                    let original_local_size_mm = local_size_mm;
                    let mut rotation = object.placement.placement.rotation_degrees;
                    let position_change = toolbar_group(
                        &mut toolbar_ui,
                        &mut ToolbarGroup::new(
                            text.object_position_label,
                            ToolbarItem::new(&mut position, Some("mm"), -5_000.0..=5_000.0),
                        ),
                    );
                    toolbar_ui.add_space(18.0);
                    let scale_change = toolbar_group(
                        &mut toolbar_ui,
                        &mut ToolbarGroup::new(
                            text.object_scale_label,
                            ToolbarItem::new(&mut scale_percent, Some("%"), 1.0..=1_000.0),
                        )
                        .with_secondary(ToolbarItem::new(
                            &mut local_size_mm,
                            Some("mm"),
                            0.01..=50_000.0,
                        )),
                    );
                    toolbar_ui
                        .checkbox(&mut scale_aspect_locked, "")
                        .on_hover_text(text.object_lock_aspect_ratio);
                    toolbar_ui.add_space(18.0);
                    let rotation_change = toolbar_group(
                        &mut toolbar_ui,
                        &mut ToolbarGroup::new(
                            text.object_rotation_label,
                            ToolbarItem::new(&mut rotation, Some("°"), -3600.0..=3600.0),
                        ),
                    );
                    placement_changed = position_change.changed()
                        || scale_change.changed()
                        || rotation_change.changed();
                    if placement_changed {
                        object.placement.placement.center_mm = position;
                        if scale_change.secondary_changed {
                            if scale_aspect_locked {
                                local_size_mm =
                                    locked_aspect_vec2(original_local_size_mm, local_size_mm);
                            }
                            object.placement.set_local_size_mm(&object.loaded_svg, local_size_mm);
                        } else if scale_change.primary_changed {
                            if scale_aspect_locked {
                                scale_percent =
                                    locked_aspect_vec2(original_scale_percent, scale_percent);
                            }
                            object.placement.set_scale_percent(scale_percent);
                        }
                        object.placement.placement.rotation_degrees = rotation;
                    }
                } else {
                    toolbar_ui.label(text.no_svg_selected);
                }
                self.scale_aspect_locked = scale_aspect_locked;

                if placement_changed {
                    self.rebuild_toolpath();
                }

                if toolbar_ui
                    .add_enabled(
                        self.selected_svg_id.is_some(),
                        egui::Button::new(text.object_delete_short),
                    )
                    .on_hover_text(text.delete_selected_svg)
                    .clicked()
                {
                    self.delete_selected_svg();
                }
            });

        self.paint_selected_object_gizmo(ui, preview_rect, view_projection, object_bounds);
    }

    fn paint_selected_object_gizmo(
        &self,
        ui: &mut egui::Ui,
        preview_rect: egui::Rect,
        view_projection: glam::Mat4,
        object_bounds: &[PreviewObjectBounds],
    ) {
        let Some(selected_id) = self.selected_svg_id else {
            return;
        };
        let Some(object) = object_bounds.iter().find(|object| object.id == selected_id) else {
            return;
        };
        let Some(center) = project_bed_point(object.center_mm, preview_rect, view_projection)
        else {
            return;
        };
        let painter = ui.painter().with_clip_rect(preview_rect);
        let bounds_points = projected_bounds_points(object, preview_rect, view_projection);
        let color_x = egui::Color32::from_rgb(240, 72, 72);
        let color_y = egui::Color32::from_rgb(72, 210, 112);
        let color_ring = egui::Color32::from_rgb(90, 140, 255);
        if let Some(points) = bounds_points {
            for window in points.windows(2) {
                painter.line_segment(
                    [window[0], window[1]],
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(140, 170, 220)),
                );
            }
            painter.line_segment(
                [points[3], points[0]],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(140, 170, 220)),
            );
        }

        painter.circle_filled(center, 5.0, egui::Color32::WHITE);
        match self.manipulation_mode {
            ManipulationMode::Move => {
                let x_end = center + egui::vec2(56.0, 0.0);
                let y_end = center - egui::vec2(0.0, 56.0);
                painter.line_segment([center, x_end], egui::Stroke::new(3.0, color_x));
                painter.line_segment([center, y_end], egui::Stroke::new(3.0, color_y));
                draw_arrow_head(&painter, x_end, egui::vec2(1.0, 0.0), color_x);
                draw_arrow_head(&painter, y_end, egui::vec2(0.0, -1.0), color_y);
            }
            ManipulationMode::Scale => {
                if let Some(points) = bounds_points {
                    for point in points {
                        let handle_rect =
                            egui::Rect::from_center_size(point, egui::vec2(12.0, 12.0));
                        painter.rect_filled(
                            handle_rect,
                            2.0,
                            egui::Color32::from_rgb(38, 160, 220),
                        );
                        painter.rect_stroke(
                            handle_rect,
                            2.0,
                            egui::Stroke::new(1.5, egui::Color32::WHITE),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }
            ManipulationMode::Rotate => {
                let radius = bounds_points
                    .map(|points| {
                        points.iter().map(|point| point.distance(center)).fold(0.0, f32::max) + 18.0
                    })
                    .unwrap_or(36.0)
                    .max(36.0);
                painter.circle_stroke(center, radius, egui::Stroke::new(2.5, color_ring));
                let handle_angle = -0.75_f32;
                let handle =
                    center + egui::vec2(handle_angle.cos() * radius, handle_angle.sin() * radius);
                painter.circle_filled(handle, 7.0, color_ring);
                painter.circle_stroke(handle, 7.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
            }
        }
    }

    fn show_preview_controls_overlay(
        &mut self,
        ui: &mut egui::Ui,
        preview_rect: egui::Rect,
        timeline_text: &str,
    ) {
        let text = self.text();
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
                    let toggle_label = if self.preview_playing { text.pause } else { text.play };
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
                        .add_enabled(self.toolpath_plan.is_some(), egui::Button::new(text.reset))
                        .clicked()
                    {
                        self.preview_progress = 0.0;
                        self.preview_playing = false;
                    }

                    ui.checkbox(&mut self.show_travel_moves, text.show_pen_lift_travel_paths);
                    ui.checkbox(&mut self.show_drawing_bounds, text.show_bounding_box);

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
        self.handle_object_shortcuts(ctx);
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

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            APP_STATE_STORAGE_KEY,
            &PersistedAppState {
                language: self.language,
                device: self.device.preferences(),
                curve_output_mode: self.settings.curve_output_mode,
            },
        );
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

struct ToolbarItem<'a, T> {
    value: &'a mut T,
    suffix: Option<&'a str>,
    range: std::ops::RangeInclusive<f32>,
}

impl<'a, T> ToolbarItem<'a, T> {
    fn new(
        value: &'a mut T,
        suffix: Option<&'a str>,
        range: std::ops::RangeInclusive<f32>,
    ) -> Self {
        Self { value, suffix, range }
    }
}

struct ToolbarGroup<'a, T> {
    label: &'a str,
    primary: ToolbarItem<'a, T>,
    secondary: Option<ToolbarItem<'a, T>>,
}

impl<'a, T> ToolbarGroup<'a, T> {
    fn new(label: &'a str, primary: ToolbarItem<'a, T>) -> Self {
        Self { label, primary, secondary: None }
    }

    fn with_secondary(mut self, secondary: ToolbarItem<'a, T>) -> Self {
        self.secondary = Some(secondary);
        self
    }
}

#[derive(Default)]
struct ToolbarGroupChange {
    primary_changed: bool,
    secondary_changed: bool,
}

impl ToolbarGroupChange {
    fn changed(&self) -> bool {
        self.primary_changed || self.secondary_changed
    }
}

trait ToolbarValue {
    fn field_count() -> usize;
    fn show_fields(
        &mut self,
        ui: &mut egui::Ui,
        suffix: Option<&str>,
        range: std::ops::RangeInclusive<f32>,
    ) -> bool;
}

impl ToolbarValue for f32 {
    fn field_count() -> usize {
        1
    }

    fn show_fields(
        &mut self,
        ui: &mut egui::Ui,
        suffix: Option<&str>,
        range: std::ops::RangeInclusive<f32>,
    ) -> bool {
        show_toolbar_number(ui, self, suffix, range)
    }
}

impl ToolbarValue for i32 {
    fn field_count() -> usize {
        1
    }

    fn show_fields(
        &mut self,
        ui: &mut egui::Ui,
        suffix: Option<&str>,
        range: std::ops::RangeInclusive<f32>,
    ) -> bool {
        let mut value = *self as f32;
        let changed = show_toolbar_number(ui, &mut value, suffix, range);
        if changed {
            *self = value.round() as i32;
        }
        changed
    }
}

impl ToolbarValue for glam::Vec2 {
    fn field_count() -> usize {
        2
    }

    fn show_fields(
        &mut self,
        ui: &mut egui::Ui,
        suffix: Option<&str>,
        range: std::ops::RangeInclusive<f32>,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= show_toolbar_number(ui, &mut self.x, None, range.clone());
            changed |= show_toolbar_number(ui, &mut self.y, suffix, range);
        });
        changed
    }
}

impl ToolbarValue for glam::Vec3 {
    fn field_count() -> usize {
        3
    }

    fn show_fields(
        &mut self,
        ui: &mut egui::Ui,
        suffix: Option<&str>,
        range: std::ops::RangeInclusive<f32>,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= show_toolbar_number(ui, &mut self.x, None, range.clone());
            changed |= show_toolbar_number(ui, &mut self.y, None, range.clone());
            changed |= show_toolbar_number(ui, &mut self.z, suffix, range);
        });
        changed
    }
}

fn toolbar_group<T: ToolbarValue>(
    ui: &mut egui::Ui,
    group: &mut ToolbarGroup<'_, T>,
) -> ToolbarGroupChange {
    let mut change = ToolbarGroupChange::default();
    ui.label(group.label);
    let row_width = toolbar_value_width::<T>();
    let row_height = ui.spacing().interact_size.y;
    let total_height = if group.secondary.is_some() {
        row_height * 2.0 + ui.spacing().item_spacing.y
    } else {
        row_height
    };
    ui.allocate_ui_with_layout(
        egui::vec2(row_width, total_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            change.primary_changed = group.primary.value.show_fields(
                ui,
                group.primary.suffix,
                group.primary.range.clone(),
            );
            if let Some(secondary) = group.secondary.as_mut() {
                change.secondary_changed =
                    secondary.value.show_fields(ui, secondary.suffix, secondary.range.clone());
            }
        },
    );
    change
}

fn toolbar_value_width<T: ToolbarValue>() -> f32 {
    T::field_count() as f32 * 78.0 + 32.0
}

fn show_toolbar_number(
    ui: &mut egui::Ui,
    value: &mut f32,
    suffix: Option<&str>,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let old_spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.x = 2.0;
        changed = ui
            .add_sized(
                [72.0, ui.spacing().interact_size.y],
                egui::DragValue::new(value).speed(1.0).range(range).fixed_decimals(2),
            )
            .changed();
        if let Some(suffix) = suffix {
            ui.label(suffix);
        }
        ui.spacing_mut().item_spacing = old_spacing;
    });
    changed
}

fn locked_aspect_vec2(original: glam::Vec2, edited: glam::Vec2) -> glam::Vec2 {
    let x_changed = (edited.x - original.x).abs();
    let y_changed = (edited.y - original.y).abs();
    if x_changed <= f32::EPSILON && y_changed <= f32::EPSILON {
        return edited;
    }
    if x_changed >= y_changed {
        let ratio = if original.x.abs() <= f32::EPSILON { 1.0 } else { edited.x / original.x };
        glam::vec2(edited.x, (original.y * ratio).max(0.01))
    } else {
        let ratio = if original.y.abs() <= f32::EPSILON { 1.0 } else { edited.y / original.y };
        glam::vec2((original.x * ratio).max(0.01), edited.y)
    }
}

fn projected_bounds_points(
    object: &PreviewObjectBounds,
    preview_rect: egui::Rect,
    view_projection: glam::Mat4,
) -> Option<[egui::Pos2; 4]> {
    let min = object.bounds_origin_mm;
    let max = object.bounds_origin_mm + object.bounds_size_mm;
    Some([
        project_bed_point(min, preview_rect, view_projection)?,
        project_bed_point(glam::vec2(max.x, min.y), preview_rect, view_projection)?,
        project_bed_point(max, preview_rect, view_projection)?,
        project_bed_point(glam::vec2(min.x, max.y), preview_rect, view_projection)?,
    ])
}

fn draw_arrow_head(
    painter: &egui::Painter,
    tip: egui::Pos2,
    direction: egui::Vec2,
    color: egui::Color32,
) {
    let direction = direction.normalized();
    let normal = egui::vec2(-direction.y, direction.x);
    let back = tip - direction * 12.0;
    painter.line_segment([tip, back + normal * 5.0], egui::Stroke::new(3.0, color));
    painter.line_segment([tip, back - normal * 5.0], egui::Stroke::new(3.0, color));
}

fn combine_prepared_svgs(
    prepared_svgs: Vec<paths::PreparedSvg>,
    printable_area: PrintableArea,
) -> paths::PreparedSvg {
    let mut source_names = Vec::new();
    let mut strokes = Vec::new();
    let mut fill_regions = Vec::new();
    let mut warnings = Vec::new();
    let mut min = glam::Vec2::splat(f32::INFINITY);
    let mut max = glam::Vec2::splat(f32::NEG_INFINITY);
    let mut is_out_of_bounds = false;

    for prepared in prepared_svgs {
        source_names.push(prepared.source_name);
        warnings.extend(prepared.warnings);
        min = min.min(prepared.drawing_origin);
        max = max.max(prepared.drawing_origin + prepared.drawing_bounds);
        is_out_of_bounds |= prepared.is_out_of_bounds;
        strokes.extend(prepared.strokes);
        fill_regions.extend(prepared.fill_regions);
    }

    if !min.is_finite() || !max.is_finite() {
        min = glam::Vec2::ZERO;
        max = glam::Vec2::ZERO;
    }

    is_out_of_bounds |= min.x < -0.01
        || min.y < -0.01
        || max.x > printable_area.width_mm + 0.01
        || max.y > printable_area.height_mm + 0.01;

    paths::PreparedSvg {
        source_name: source_names.join(", "),
        strokes,
        fill_regions,
        warnings,
        drawing_origin: min,
        drawing_bounds: max - min,
        is_out_of_bounds,
    }
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
    language: Language,
) -> egui::Response {
    let tooltip = corners
        .and_then(|corners| corners.get(index).copied())
        .map(|corner| format!("{label}: X {:.2}, Y {:.2}", corner.x, corner.y))
        .unwrap_or_else(|| language.strings().load_svg_to_use_control.to_owned());
    ui.add_enabled(enabled, egui::Button::new(label).min_size(egui::vec2(48.0, 24.0)))
        .on_hover_text(tooltip)
}

fn fill_pattern_label(pattern: FillPattern, text: &Strings) -> &'static str {
    match pattern {
        FillPattern::Lines => text.fill_pattern_lines,
        FillPattern::Crosshatch => text.fill_pattern_crosshatch,
        FillPattern::Zigzag => text.fill_pattern_zigzag,
        FillPattern::ContinuousZigzag => text.fill_pattern_continuous_zigzag,
    }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct TestStorage {
        values: HashMap<String, String>,
    }

    impl eframe::Storage for TestStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.values.insert(key.to_owned(), value);
        }

        fn flush(&mut self) {}
    }

    #[test]
    fn persisted_app_state_round_trips_curve_output_mode() {
        let mut storage = TestStorage::default();
        let state = PersistedAppState {
            language: Language::Korean,
            device: DevicePreferences::default(),
            curve_output_mode: CurveOutputMode::PreferG2G3AndG5,
        };

        eframe::set_value(&mut storage, APP_STATE_STORAGE_KEY, &state);
        let loaded = load_persisted_app_state(&storage).expect("state should round-trip");

        assert_eq!(loaded.language, Language::Korean);
        assert_eq!(loaded.curve_output_mode, CurveOutputMode::PreferG2G3AndG5);
    }

    #[test]
    fn legacy_language_only_storage_keeps_default_curve_output_mode() {
        let mut storage = TestStorage::default();
        eframe::Storage::set_string(
            &mut storage,
            APP_STATE_STORAGE_KEY,
            Language::Korean.storage_key().to_owned(),
        );

        let loaded =
            load_persisted_app_state(&storage).expect("legacy language storage should load");

        assert_eq!(loaded.language, Language::Korean);
        assert_eq!(loaded.curve_output_mode, CurveOutputMode::default());
    }
}
