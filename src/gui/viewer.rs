use std::{cell::RefCell, sync::Arc};

use bytemuck::{Pod, Zeroable};
use eframe::{
    egui,
    egui_wgpu::{
        self,
        wgpu::{self},
    },
};
use glam::{Mat4, Vec2, Vec3, vec3};
use serde::{Deserialize, Serialize};

use crate::{
    plot::model::{MotionKind, MotionSegment, PrintableArea, ToolpathPlan},
    res::colors,
    res::lang::Language,
};

#[derive(Debug, Clone)]
pub struct ViewportState {
    view_mode: PreviewViewMode,
    yaw: f32,
    pitch: f32,
    zoom_3d: f32,
    pan_3d: Vec2,
    zoom_2d: f32,
    pan_2d: Vec2,
    active_object_id: Option<u64>,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            view_mode: PreviewViewMode::default(),
            yaw: -std::f32::consts::FRAC_PI_2,
            pitch: 0.75,
            zoom_3d: 1.15,
            pan_3d: Vec2::ZERO,
            zoom_2d: 1.0,
            pan_2d: Vec2::ZERO,
            active_object_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PreviewViewMode {
    TwoD,
    #[default]
    ThreeD,
}

impl PreviewViewMode {
    pub const ALL: [Self; 2] = [Self::TwoD, Self::ThreeD];

    pub fn label(self, language: Language) -> &'static str {
        let text = language.strings();
        match self {
            Self::TwoD => text.preview_view_mode_2d,
            Self::ThreeD => text.preview_view_mode_3d,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManipulationMode {
    Move,
    Scale,
    Rotate,
}

#[derive(Debug, Clone, Copy)]
pub struct PreviewObjectBounds {
    pub id: u64,
    pub center_mm: Vec2,
    pub bounds_origin_mm: Vec2,
    pub bounds_size_mm: Vec2,
}

#[derive(Debug, Clone, Copy)]
pub enum PreviewManipulation {
    Select(u64),
    Move { id: u64, delta_mm: Vec2 },
    Scale { id: u64, factor: Vec2 },
    Rotate { id: u64, delta_degrees: f32 },
}

#[derive(Debug, Clone)]
pub struct PreviewOutput {
    pub rect: egui::Rect,
    pub view_projection: Mat4,
    pub manipulation: Option<PreviewManipulation>,
}

#[derive(Debug, Clone, Copy)]
struct PreviewInteraction {
    manipulation: Option<PreviewManipulation>,
    viewport_interacting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewQuality {
    Full,
    Interactive,
}

impl ViewportState {
    pub fn with_view_mode(view_mode: PreviewViewMode) -> Self {
        let mut state = Self::default();
        state.view_mode = view_mode;
        state
    }

    pub fn view_mode(&self) -> PreviewViewMode {
        self.view_mode
    }

    pub fn set_view_mode(&mut self, view_mode: PreviewViewMode) {
        if self.view_mode != view_mode {
            self.view_mode = view_mode;
            self.active_object_id = None;
        }
    }

    fn pan(&self) -> Vec2 {
        match self.view_mode {
            PreviewViewMode::TwoD => self.pan_2d,
            PreviewViewMode::ThreeD => self.pan_3d,
        }
    }

    fn pan_mut(&mut self) -> &mut Vec2 {
        match self.view_mode {
            PreviewViewMode::TwoD => &mut self.pan_2d,
            PreviewViewMode::ThreeD => &mut self.pan_3d,
        }
    }

    fn zoom(&self) -> f32 {
        match self.view_mode {
            PreviewViewMode::TwoD => self.zoom_2d,
            PreviewViewMode::ThreeD => self.zoom_3d,
        }
    }

    fn zoom_mut(&mut self) -> &mut f32 {
        match self.view_mode {
            PreviewViewMode::TwoD => &mut self.zoom_2d,
            PreviewViewMode::ThreeD => &mut self.zoom_3d,
        }
    }

    fn zoom_limits(&self) -> (f32, f32) {
        match self.view_mode {
            PreviewViewMode::TwoD => (0.05, 5.0),
            PreviewViewMode::ThreeD => (0.55, 2.4),
        }
    }

    fn pan_from_pointer_delta(
        &mut self,
        response: &egui::Response,
        ui: &egui::Ui,
        view_projection: Mat4,
    ) {
        let Some(pointer) = response.interact_pointer_pos() else {
            return;
        };
        let pointer_delta = ui.input(|input| input.pointer.delta());
        let previous = pointer - pointer_delta;
        if let (Some(current_mm), Some(previous_mm)) = (
            screen_to_bed(pointer, response.rect, view_projection),
            screen_to_bed(previous, response.rect, view_projection),
        ) {
            *self.pan_mut() += previous_mm - current_mm;
        }
    }

    fn handle_input(
        &mut self,
        response: &egui::Response,
        ui: &egui::Ui,
        scene_extent: f32,
        view_projection: Mat4,
        objects: &[PreviewObjectBounds],
        selected_object_id: Option<u64>,
        mode: ManipulationMode,
    ) -> PreviewInteraction {
        if !ui.input(|input| input.pointer.primary_down()) {
            self.active_object_id = None;
        }

        let mut manipulation = None;
        let mut viewport_interacting = false;
        if response.drag_started_by(egui::PointerButton::Primary)
            || response.clicked_by(egui::PointerButton::Primary)
        {
            if let Some(pointer) = response.interact_pointer_pos() {
                if let Some(world) = screen_to_bed(pointer, response.rect, view_projection) {
                    if let Some(id) = hit_object(
                        pointer,
                        world,
                        response.rect,
                        view_projection,
                        objects,
                        selected_object_id,
                        mode,
                    ) {
                        self.active_object_id = Some(id);
                        manipulation = Some(PreviewManipulation::Select(id));
                    }
                }
            }
        }

        if let Some(id) = self.active_object_id {
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pointer) = response.interact_pointer_pos() {
                    let pointer_delta = ui.input(|input| input.pointer.delta());
                    let previous = pointer - pointer_delta;
                    if let (Some(current_mm), Some(previous_mm)) = (
                        screen_to_bed(pointer, response.rect, view_projection),
                        screen_to_bed(previous, response.rect, view_projection),
                    ) {
                        let object_center = objects
                            .iter()
                            .find(|object| object.id == id)
                            .map(|object| object.center_mm);
                        manipulation = match mode {
                            ManipulationMode::Move => Some(PreviewManipulation::Move {
                                id,
                                delta_mm: current_mm - previous_mm,
                            }),
                            ManipulationMode::Scale => object_center.map(|center| {
                                let previous_offset = previous_mm - center;
                                let current_offset = current_mm - center;
                                PreviewManipulation::Scale {
                                    id,
                                    factor: Vec2::new(
                                        axis_scale_factor(previous_offset.x, current_offset.x),
                                        axis_scale_factor(previous_offset.y, current_offset.y),
                                    ),
                                }
                            }),
                            ManipulationMode::Rotate => object_center.map(|center| {
                                let previous_angle =
                                    (previous_mm.y - center.y).atan2(previous_mm.x - center.x);
                                let current_angle =
                                    (current_mm.y - center.y).atan2(current_mm.x - center.x);
                                PreviewManipulation::Rotate {
                                    id,
                                    delta_degrees: (current_angle - previous_angle).to_degrees(),
                                }
                            }),
                        };
                    }
                }
            }
            return PreviewInteraction { manipulation, viewport_interacting };
        }

        if response.dragged_by(egui::PointerButton::Primary) {
            match self.view_mode {
                PreviewViewMode::TwoD => {
                    self.pan_from_pointer_delta(response, ui, view_projection);
                    viewport_interacting = true;
                }
                PreviewViewMode::ThreeD => {
                    let drag = response.drag_motion();
                    self.yaw -= drag.x * 0.01;
                    self.pitch = (self.pitch + drag.y * 0.01).clamp(0.2, 1.35);
                    viewport_interacting = true;
                }
            }
        }

        if response.dragged_by(egui::PointerButton::Secondary) {
            match self.view_mode {
                PreviewViewMode::TwoD => {
                    self.pan_from_pointer_delta(response, ui, view_projection);
                    viewport_interacting = true;
                }
                PreviewViewMode::ThreeD => {
                    let drag = response.drag_motion();
                    let pan_scale = scene_extent.max(40.0)
                        / response.rect.width().min(response.rect.height()).max(1.0)
                        * self.zoom();
                    let eye_direction = vec3(
                        self.yaw.cos() * self.pitch.cos(),
                        self.yaw.sin() * self.pitch.cos(),
                        self.pitch.sin(),
                    );
                    let view_direction = -eye_direction;
                    let right = view_direction.cross(Vec3::Z).normalize_or_zero();
                    let camera_up = right.cross(view_direction);
                    let up_on_plane = vec3(camera_up.x, camera_up.y, 0.0).normalize_or_zero();
                    let pan_delta = (-right * drag.x + up_on_plane * drag.y) * pan_scale;
                    *self.pan_mut() += pan_delta.truncate();
                    viewport_interacting = true;
                }
            }
        }

        if response.hovered() {
            let (scroll, is_scrolling) =
                ui.input(|input| (input.smooth_scroll_delta.y, input.is_scrolling()));
            if scroll.abs() > f32::EPSILON {
                let (min_zoom, max_zoom) = self.zoom_limits();
                *self.zoom_mut() =
                    (self.zoom() * (1.0 - scroll * 0.0015)).clamp(min_zoom, max_zoom);
                viewport_interacting = true;
            } else if is_scrolling {
                viewport_interacting = true;
            }
        }
        PreviewInteraction { manipulation, viewport_interacting }
    }
}

pub struct PreviewRenderer {
    ready: bool,
    cache: RefCell<Option<PreviewGeometryCache>>,
}

const INTERACTIVE_TOOLPATH_SEGMENT_BUDGET: usize = 12_000;

impl PreviewRenderer {
    pub fn new(cc: &eframe::CreationContext<'_>, msaa_samples: u32) -> Self {
        let Some(render_state) = cc.wgpu_render_state.as_ref() else {
            return Self { ready: false, cache: RefCell::new(None) };
        };

        let device = &render_state.device;
        let sample_count = msaa_samples.max(1);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("penartic-preview-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("preview_shader.wgsl").into()),
        });

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("penartic-preview-camera-bind-group-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("penartic-preview-pipeline-layout"),
            bind_group_layouts: &[Some(&camera_bind_group_layout)],
            immediate_size: 0,
        });

        let target = Some(render_state.target_format.into());
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        };

        let triangle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("penartic-preview-triangle-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[target.clone()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: sample_count, ..Default::default() },
            multiview_mask: None,
            cache: None,
        });

        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("penartic-preview-line-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[target],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: sample_count, ..Default::default() },
            multiview_mask: None,
            cache: None,
        });

        render_state.renderer.write().callback_resources.insert(PreviewRenderResources {
            triangle_pipeline,
            line_pipeline,
            camera_bind_group_layout,
            camera_buffer: None,
            camera_bind_group: None,
            triangle_buffer: None,
            line_buffer: None,
            triangle_capacity: 0,
            line_capacity: 0,
            triangle_vertex_count: 0,
            line_vertex_count: 0,
            geometry_id: None,
        });

        Self { ready: true, cache: RefCell::new(None) }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        desired_size: egui::Vec2,
        plan: Option<&ToolpathPlan>,
        progress: f32,
        show_travel_moves: bool,
        show_drawing_bounds: bool,
        language: Language,
        state: &mut ViewportState,
        objects: &[PreviewObjectBounds],
        selected_object_id: Option<u64>,
        mode: ManipulationMode,
    ) -> PreviewOutput {
        let text = language.strings();
        let desired = egui::vec2(desired_size.x.max(1.0), desired_size.y.max(1.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::drag());
        let scene_extent = plan
            .map(|plan| plan.printable_area.width_mm.max(plan.printable_area.height_mm))
            .unwrap_or(220.0);
        let printable_area = plan.map(|plan| plan.printable_area).unwrap_or_default();
        let view_projection = view_projection_for_area(printable_area, rect.size(), state);
        let interaction = state.handle_input(
            &response,
            ui,
            scene_extent,
            view_projection,
            objects,
            selected_object_id,
            mode,
        );

        ui.painter().rect_filled(rect, 0.0, colors::preview_background());

        if !self.ready {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text.wgpu_preview_unavailable,
                egui::TextStyle::Heading.resolve(ui.style()),
                colors::error(),
            );
            return PreviewOutput { rect, view_projection, manipulation: interaction.manipulation };
        }

        let Some(plan) = plan else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text.load_svg_preview_placeholder,
                egui::TextStyle::Heading.resolve(ui.style()),
                colors::muted_text(),
            );
            return PreviewOutput { rect, view_projection, manipulation: interaction.manipulation };
        };

        let quality =
            if state.view_mode() == PreviewViewMode::ThreeD && interaction.viewport_interacting {
                PreviewQuality::Interactive
            } else {
                PreviewQuality::Full
            };
        let geometry = self.cached_geometry(plan, progress, show_travel_moves, show_drawing_bounds);

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            PreviewPaintCallback {
                geometry_id: PreviewRenderGeometryId {
                    geometry_ptr: Arc::as_ptr(&geometry) as usize,
                    quality,
                },
                geometry,
                view_projection: CameraUniform::new(view_projection),
            },
        ));
        PreviewOutput { rect, view_projection, manipulation: interaction.manipulation }
    }

    fn cached_geometry(
        &self,
        plan: &ToolpathPlan,
        progress: f32,
        show_travel_moves: bool,
        show_drawing_bounds: bool,
    ) -> Arc<PreviewGeometry> {
        let key = PreviewGeometryKey {
            plan_ptr: plan as *const ToolpathPlan as usize,
            segments_ptr: plan.segments.as_ptr() as usize,
            segment_count: plan.segments.len(),
            drawing_origin_x: plan.drawing_origin.x.to_bits(),
            drawing_origin_y: plan.drawing_origin.y.to_bits(),
            drawing_bounds_x: plan.drawing_bounds.x.to_bits(),
            drawing_bounds_y: plan.drawing_bounds.y.to_bits(),
            printable_width: plan.printable_area.width_mm.to_bits(),
            printable_height: plan.printable_area.height_mm.to_bits(),
            is_out_of_bounds: plan.is_out_of_bounds,
            progress_bits: progress.to_bits(),
            show_travel_moves,
            show_drawing_bounds,
        };

        let mut cache = self.cache.borrow_mut();
        if let Some(cache) = cache.as_ref() {
            if cache.key == key {
                return cache.geometry.clone();
            }
        }

        let geometry = Arc::new(PreviewGeometry::from_plan(
            plan,
            progress,
            show_travel_moves,
            show_drawing_bounds,
        ));
        *cache = Some(PreviewGeometryCache { key, geometry: geometry.clone() });
        geometry
    }
}

#[derive(Clone)]
struct PreviewGeometry {
    triangle_vertices: Vec<GpuVertex>,
    full_line_vertices: Vec<GpuVertex>,
    interactive_line_vertices: Vec<GpuVertex>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PreviewGeometryKey {
    plan_ptr: usize,
    segments_ptr: usize,
    segment_count: usize,
    drawing_origin_x: u32,
    drawing_origin_y: u32,
    drawing_bounds_x: u32,
    drawing_bounds_y: u32,
    printable_width: u32,
    printable_height: u32,
    is_out_of_bounds: bool,
    progress_bits: u32,
    show_travel_moves: bool,
    show_drawing_bounds: bool,
}

struct PreviewGeometryCache {
    key: PreviewGeometryKey,
    geometry: Arc<PreviewGeometry>,
}

impl PreviewGeometry {
    fn from_plan(
        plan: &ToolpathPlan,
        progress: f32,
        show_travel_moves: bool,
        show_drawing_bounds: bool,
    ) -> Self {
        let mut triangle_vertices = Vec::new();
        let mut static_line_vertices = Vec::new();

        append_bed(&mut triangle_vertices, &mut static_line_vertices, plan);
        if show_drawing_bounds {
            let color = if plan.is_out_of_bounds {
                colors::preview_overflow()
            } else {
                colors::preview_bounds()
            };
            append_drawing_bounds_outline(&mut static_line_vertices, plan, color);
        }

        let (finished, partial, pen_position) = plan.progress_state(progress);
        let finished_segments = &plan.segments[..finished];
        let visible_segment_count =
            visible_segment_count(finished_segments, partial.as_ref(), show_travel_moves);
        let interactive_stride = interactive_segment_stride(visible_segment_count);

        let mut full_line_vertices = static_line_vertices.clone();
        append_visible_segments(
            &mut full_line_vertices,
            finished_segments,
            partial.as_ref(),
            plan.printable_area,
            show_travel_moves,
        );

        let interactive_line_vertices = if interactive_stride == 1 {
            full_line_vertices.clone()
        } else {
            let mut interactive_line_vertices = static_line_vertices;
            append_interactive_segments_with_stride(
                &mut interactive_line_vertices,
                finished_segments,
                partial.as_ref(),
                plan.printable_area,
                show_travel_moves,
                interactive_stride,
            );
            interactive_line_vertices
        };

        append_pen(&mut triangle_vertices, pen_position);

        Self { triangle_vertices, full_line_vertices, interactive_line_vertices }
    }

    fn line_vertices(&self, quality: PreviewQuality) -> &[GpuVertex] {
        match quality {
            PreviewQuality::Full => &self.full_line_vertices,
            PreviewQuality::Interactive => &self.interactive_line_vertices,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PreviewRenderGeometryId {
    geometry_ptr: usize,
    quality: PreviewQuality,
}

#[derive(Clone)]
struct PreviewPaintCallback {
    geometry_id: PreviewRenderGeometryId,
    geometry: Arc<PreviewGeometry>,
    view_projection: CameraUniform,
}

impl egui_wgpu::CallbackTrait for PreviewPaintCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let resources: &mut PreviewRenderResources = callback_resources.get_mut().unwrap();
        resources.update(device, queue, &self.view_projection, self.geometry_id, &self.geometry);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let resources: &PreviewRenderResources = callback_resources.get().unwrap();
        resources.paint(render_pass);
    }
}

struct PreviewRenderResources {
    triangle_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    camera_bind_group_layout: wgpu::BindGroupLayout,
    camera_buffer: Option<wgpu::Buffer>,
    camera_bind_group: Option<wgpu::BindGroup>,
    triangle_buffer: Option<wgpu::Buffer>,
    line_buffer: Option<wgpu::Buffer>,
    triangle_capacity: usize,
    line_capacity: usize,
    triangle_vertex_count: u32,
    line_vertex_count: u32,
    geometry_id: Option<PreviewRenderGeometryId>,
}

impl PreviewRenderResources {
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &CameraUniform,
        geometry_id: PreviewRenderGeometryId,
        geometry: &PreviewGeometry,
    ) {
        self.update_camera(device, queue, camera);

        if self.geometry_id == Some(geometry_id) {
            return;
        }
        self.geometry_id = Some(geometry_id);

        let triangle_vertices = &geometry.triangle_vertices;
        let line_vertices = geometry.line_vertices(geometry_id.quality);
        self.triangle_vertex_count = triangle_vertices.len() as u32;
        self.line_vertex_count = line_vertices.len() as u32;

        update_buffer(
            device,
            queue,
            "penartic-preview-triangles",
            &mut self.triangle_buffer,
            &mut self.triangle_capacity,
            triangle_vertices,
        );
        update_buffer(
            device,
            queue,
            "penartic-preview-lines",
            &mut self.line_buffer,
            &mut self.line_capacity,
            line_vertices,
        );
    }

    fn update_camera(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &CameraUniform,
    ) {
        let bytes = bytemuck::bytes_of(camera);
        if self.camera_buffer.is_none() {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("penartic-preview-camera"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::UNIFORM,
                mapped_at_creation: false,
            });
            self.camera_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("penartic-preview-camera-bind-group"),
                layout: &self.camera_bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            }));
            self.camera_buffer = Some(buffer);
        }

        if let Some(buffer) = self.camera_buffer.as_ref() {
            queue.write_buffer(buffer, 0, bytes);
        }
    }

    fn paint(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        let Some(camera_bind_group) = self.camera_bind_group.as_ref() else {
            return;
        };

        if self.triangle_vertex_count > 0 {
            render_pass.set_pipeline(&self.triangle_pipeline);
            render_pass.set_bind_group(0, camera_bind_group, &[]);
            if let Some(buffer) = &self.triangle_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..self.triangle_vertex_count, 0..1);
            }
        }

        if self.line_vertex_count > 0 {
            render_pass.set_pipeline(&self.line_pipeline);
            render_pass.set_bind_group(0, camera_bind_group, &[]);
            if let Some(buffer) = &self.line_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..self.line_vertex_count, 0..1);
            }
        }
    }
}

fn update_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    buffer: &mut Option<wgpu::Buffer>,
    capacity: &mut usize,
    vertices: &[GpuVertex],
) {
    if vertices.is_empty() {
        return;
    }

    let bytes = bytemuck::cast_slice(vertices);

    if buffer.is_none() || bytes.len() > *capacity {
        *capacity = bytes.len().next_power_of_two();
        *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: *capacity as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::VERTEX,
            mapped_at_creation: false,
        }));
    }

    if let Some(existing) = buffer.as_ref() {
        queue.write_buffer(existing, 0, bytes);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_projection: [[f32; 4]; 4],
}

impl CameraUniform {
    fn new(view_projection: Mat4) -> Self {
        Self { view_projection: view_projection.to_cols_array_2d() }
    }
}

fn view_projection_for_area(
    printable_area: PrintableArea,
    viewport_size: egui::Vec2,
    state: &ViewportState,
) -> Mat4 {
    let aspect = (viewport_size.x / viewport_size.y).max(0.1);
    let pan = state.pan();
    let center =
        vec3(printable_area.width_mm * 0.5 + pan.x, printable_area.height_mm * 0.5 + pan.y, 0.0);
    match state.view_mode() {
        PreviewViewMode::ThreeD => {
            let scene_radius =
                printable_area.width_mm.max(printable_area.height_mm).max(80.0) * 0.9;
            let eye_direction = vec3(
                state.yaw.cos() * state.pitch.cos(),
                state.yaw.sin() * state.pitch.cos(),
                state.pitch.sin(),
            );
            let eye = center + eye_direction * scene_radius * state.zoom() + vec3(0.0, 0.0, 24.0);

            let view = Mat4::look_at_rh(eye, center + vec3(0.0, 0.0, 2.0), Vec3::Z);
            let projection = Mat4::perspective_rh(35_f32.to_radians(), aspect, 0.1, 5_000.0);
            projection * view
        }
        PreviewViewMode::TwoD => {
            let half_extents = orthographic_half_extents(printable_area, aspect, state.zoom());
            let eye = center + vec3(0.0, 0.0, 1_000.0);
            let view = Mat4::look_at_rh(eye, center, Vec3::Y);
            let projection = Mat4::orthographic_rh(
                -half_extents.x,
                half_extents.x,
                -half_extents.y,
                half_extents.y,
                0.1,
                2_000.0,
            );
            projection * view
        }
    }
}

fn orthographic_half_extents(printable_area: PrintableArea, aspect: f32, zoom: f32) -> Vec2 {
    let mut half_width = (printable_area.width_mm * 0.5 + 20.0).max(40.0);
    let mut half_height = (printable_area.height_mm * 0.5 + 20.0).max(40.0);
    if half_width / half_height < aspect {
        half_width = half_height * aspect;
    } else {
        half_height = half_width / aspect;
    }
    Vec2::new(half_width, half_height) * zoom
}

pub fn project_bed_point(
    point: Vec2,
    rect: egui::Rect,
    view_projection: Mat4,
) -> Option<egui::Pos2> {
    let clip = view_projection * vec3(point.x, point.y, 0.0).extend(1.0);
    if clip.w.abs() <= f32::EPSILON {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(egui::pos2(
        rect.left() + (ndc.x + 1.0) * 0.5 * rect.width(),
        rect.top() + (1.0 - (ndc.y + 1.0) * 0.5) * rect.height(),
    ))
}

fn screen_to_bed(pos: egui::Pos2, rect: egui::Rect, view_projection: Mat4) -> Option<Vec2> {
    let inverse = view_projection.inverse();
    let x = ((pos.x - rect.left()) / rect.width()) * 2.0 - 1.0;
    let y = 1.0 - ((pos.y - rect.top()) / rect.height()) * 2.0;
    let near = inverse * vec3(x, y, 0.0).extend(1.0);
    let far = inverse * vec3(x, y, 1.0).extend(1.0);
    if near.w.abs() <= f32::EPSILON || far.w.abs() <= f32::EPSILON {
        return None;
    }
    let near = near.truncate() / near.w;
    let far = far.truncate() / far.w;
    let direction = far - near;
    if direction.z.abs() <= f32::EPSILON {
        return None;
    }
    let t = -near.z / direction.z;
    if !t.is_finite() {
        return None;
    }
    let point = near + direction * t;
    Some(point.truncate())
}

fn hit_object(
    pointer: egui::Pos2,
    point: Vec2,
    rect: egui::Rect,
    view_projection: Mat4,
    objects: &[PreviewObjectBounds],
    selected_object_id: Option<u64>,
    mode: ManipulationMode,
) -> Option<u64> {
    if let Some(selected) = selected_object_id {
        if let Some(object) = objects.iter().find(|object| object.id == selected) {
            if hits_selected_gizmo(pointer, rect, view_projection, object, mode) {
                return Some(selected);
            }
        }
    }

    let contains = |object: &&PreviewObjectBounds| {
        point.x >= object.bounds_origin_mm.x
            && point.y >= object.bounds_origin_mm.y
            && point.x <= object.bounds_origin_mm.x + object.bounds_size_mm.x
            && point.y <= object.bounds_origin_mm.y + object.bounds_size_mm.y
    };

    if let Some(selected) = selected_object_id {
        if objects.iter().find(|object| object.id == selected).filter(contains).is_some() {
            return Some(selected);
        }
    }
    objects.iter().rev().find(contains).map(|object| object.id)
}

fn hits_selected_gizmo(
    pointer: egui::Pos2,
    rect: egui::Rect,
    view_projection: Mat4,
    object: &PreviewObjectBounds,
    mode: ManipulationMode,
) -> bool {
    let Some(center) = project_bed_point(object.center_mm, rect, view_projection) else {
        return false;
    };
    match mode {
        ManipulationMode::Move => {
            let x_end = center + egui::vec2(56.0, 0.0);
            let y_end = center - egui::vec2(0.0, 56.0);
            distance_to_screen_segment(pointer, center, x_end) <= 10.0
                || distance_to_screen_segment(pointer, center, y_end) <= 10.0
                || pointer.distance(center) <= 14.0
        }
        ManipulationMode::Scale => screen_object_corners(object, rect, view_projection)
            .into_iter()
            .flatten()
            .any(|corner| pointer.distance(corner) <= 14.0),
        ManipulationMode::Rotate => {
            let radius = selected_gizmo_screen_radius(object, rect, view_projection).max(34.0);
            (pointer.distance(center) - radius).abs() <= 12.0
        }
    }
}

fn selected_gizmo_screen_radius(
    object: &PreviewObjectBounds,
    rect: egui::Rect,
    view_projection: Mat4,
) -> f32 {
    screen_object_corners(object, rect, view_projection)
        .into_iter()
        .flatten()
        .filter_map(|corner| {
            project_bed_point(object.center_mm, rect, view_projection)
                .map(|center| corner.distance(center))
        })
        .fold(0.0, f32::max)
        + 18.0
}

fn screen_object_corners(
    object: &PreviewObjectBounds,
    rect: egui::Rect,
    view_projection: Mat4,
) -> [Option<egui::Pos2>; 4] {
    let min = object.bounds_origin_mm;
    let max = object.bounds_origin_mm + object.bounds_size_mm;
    [
        project_bed_point(min, rect, view_projection),
        project_bed_point(Vec2::new(max.x, min.y), rect, view_projection),
        project_bed_point(max, rect, view_projection),
        project_bed_point(Vec2::new(min.x, max.y), rect, view_projection),
    ]
}

fn distance_to_screen_segment(point: egui::Pos2, start: egui::Pos2, end: egui::Pos2) -> f32 {
    let segment = end - start;
    let length_sq = segment.length_sq();
    if length_sq <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / length_sq).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

fn axis_scale_factor(previous: f32, current: f32) -> f32 {
    if previous.abs() <= 1.0 {
        return 1.0;
    }
    (current.abs() / previous.abs()).clamp(0.25, 4.0)
}

fn append_bed(
    triangle_vertices: &mut Vec<GpuVertex>,
    line_vertices: &mut Vec<GpuVertex>,
    plan: &ToolpathPlan,
) {
    let w = plan.printable_area.width_mm;
    let h = plan.printable_area.height_mm;
    let plane_color = colors::preview_plane();
    let edge_color = colors::preview_edge();
    let grid_color = colors::preview_grid();

    append_triangle(
        triangle_vertices,
        vec3(0.0, 0.0, 0.0),
        vec3(w, 0.0, 0.0),
        vec3(w, h, 0.0),
        plane_color,
    );
    append_triangle(
        triangle_vertices,
        vec3(0.0, 0.0, 0.0),
        vec3(w, h, 0.0),
        vec3(0.0, h, 0.0),
        plane_color,
    );

    let mut grid_x = 0.0;
    while grid_x <= w + 0.1 {
        append_line(
            line_vertices,
            vec3(grid_x, 0.0, 0.05),
            vec3(grid_x, h, 0.05),
            if grid_x == 0.0 || (grid_x - w).abs() <= 0.1 { edge_color } else { grid_color },
        );
        grid_x += 20.0;
    }

    let mut grid_y = 0.0;
    while grid_y <= h + 0.1 {
        append_line(
            line_vertices,
            vec3(0.0, grid_y, 0.05),
            vec3(w, grid_y, 0.05),
            if grid_y == 0.0 || (grid_y - h).abs() <= 0.1 { edge_color } else { grid_color },
        );
        grid_y += 20.0;
    }
}

fn append_segment(
    line_vertices: &mut Vec<GpuVertex>,
    segment: &MotionSegment,
    printable_area: PrintableArea,
    show_travel_moves: bool,
) -> bool {
    if !segment_is_visible(segment, show_travel_moves) {
        return false;
    }

    let style = PreviewSegmentStyle::from_segment(segment, printable_area);
    append_line(line_vertices, segment.start, segment.end, style.color());
    true
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PreviewSegmentStyle {
    kind: MotionKind,
    out_of_bounds: bool,
}

impl PreviewSegmentStyle {
    fn from_segment(segment: &MotionSegment, printable_area: PrintableArea) -> Self {
        Self { kind: segment.kind, out_of_bounds: segment_out_of_bounds(segment, printable_area) }
    }

    fn color(self) -> [f32; 4] {
        if self.out_of_bounds {
            colors::preview_overflow()
        } else {
            match self.kind {
                MotionKind::Travel => colors::preview_travel(),
                MotionKind::Draw => colors::preview_draw(),
            }
        }
    }
}

struct InteractiveToolpathBuilder {
    stride: usize,
    bucket_style: Option<PreviewSegmentStyle>,
    bucket_start: Vec3,
    bucket_end: Vec3,
    bucket_count: usize,
}

impl InteractiveToolpathBuilder {
    fn new(stride: usize) -> Self {
        Self {
            stride: stride.max(1),
            bucket_style: None,
            bucket_start: Vec3::ZERO,
            bucket_end: Vec3::ZERO,
            bucket_count: 0,
        }
    }

    fn push(
        &mut self,
        line_vertices: &mut Vec<GpuVertex>,
        segment: &MotionSegment,
        printable_area: PrintableArea,
    ) {
        let style = PreviewSegmentStyle::from_segment(segment, printable_area);
        if let Some(bucket_style) = self.bucket_style {
            let contiguous = self.bucket_end.distance_squared(segment.start) <= 1e-6;
            if bucket_style != style || !contiguous || self.bucket_count >= self.stride {
                self.flush(line_vertices);
            }
        }

        if self.bucket_style.is_none() {
            self.bucket_style = Some(style);
            self.bucket_start = segment.start;
            self.bucket_end = segment.end;
            self.bucket_count = 1;
            return;
        }

        self.bucket_end = segment.end;
        self.bucket_count += 1;
    }

    fn finish(&mut self, line_vertices: &mut Vec<GpuVertex>) {
        self.flush(line_vertices);
    }

    fn flush(&mut self, line_vertices: &mut Vec<GpuVertex>) {
        let Some(style) = self.bucket_style.take() else {
            return;
        };
        append_line(line_vertices, self.bucket_start, self.bucket_end, style.color());
        self.bucket_count = 0;
    }
}

fn append_visible_segments(
    line_vertices: &mut Vec<GpuVertex>,
    finished_segments: &[MotionSegment],
    partial: Option<&MotionSegment>,
    printable_area: PrintableArea,
    show_travel_moves: bool,
) {
    for segment in finished_segments {
        append_segment(line_vertices, segment, printable_area, show_travel_moves);
    }
    if let Some(partial) = partial {
        append_segment(line_vertices, partial, printable_area, show_travel_moves);
    }
}

fn append_interactive_segments_with_stride(
    line_vertices: &mut Vec<GpuVertex>,
    finished_segments: &[MotionSegment],
    partial: Option<&MotionSegment>,
    printable_area: PrintableArea,
    show_travel_moves: bool,
    stride: usize,
) {
    let mut builder = InteractiveToolpathBuilder::new(stride);
    for segment in finished_segments {
        if segment_is_visible(segment, show_travel_moves) {
            builder.push(line_vertices, segment, printable_area);
        }
    }
    if let Some(partial) = partial.filter(|segment| segment_is_visible(segment, show_travel_moves))
    {
        builder.push(line_vertices, partial, printable_area);
    }
    builder.finish(line_vertices);
}

fn visible_segment_count(
    finished_segments: &[MotionSegment],
    partial: Option<&MotionSegment>,
    show_travel_moves: bool,
) -> usize {
    let finished = finished_segments
        .iter()
        .filter(|segment| segment_is_visible(segment, show_travel_moves))
        .count();
    finished
        + partial
            .filter(|segment| segment_is_visible(segment, show_travel_moves))
            .map(|_| 1)
            .unwrap_or(0)
}

fn interactive_segment_stride(visible_segment_count: usize) -> usize {
    visible_segment_count.div_ceil(INTERACTIVE_TOOLPATH_SEGMENT_BUDGET).max(1)
}

fn segment_is_visible(segment: &MotionSegment, show_travel_moves: bool) -> bool {
    show_travel_moves || segment.kind != MotionKind::Travel
}

fn append_drawing_bounds_outline(
    line_vertices: &mut Vec<GpuVertex>,
    plan: &ToolpathPlan,
    color: [f32; 4],
) {
    let z = 0.15;
    let min = vec3(plan.drawing_origin.x, plan.drawing_origin.y, z);
    let max = vec3(
        plan.drawing_origin.x + plan.drawing_bounds.x,
        plan.drawing_origin.y + plan.drawing_bounds.y,
        z,
    );

    if plan.drawing_bounds.x <= 1e-3 && plan.drawing_bounds.y <= 1e-3 {
        return;
    }
    if plan.drawing_bounds.x <= 1e-3 {
        append_line(line_vertices, min, vec3(min.x, max.y, z), color);
        return;
    }
    if plan.drawing_bounds.y <= 1e-3 {
        append_line(line_vertices, min, vec3(max.x, min.y, z), color);
        return;
    }

    let bottom_right = vec3(max.x, min.y, z);
    let top_left = vec3(min.x, max.y, z);
    append_line(line_vertices, min, bottom_right, color);
    append_line(line_vertices, bottom_right, max, color);
    append_line(line_vertices, max, top_left, color);
    append_line(line_vertices, top_left, min, color);
}

fn append_pen(triangle_vertices: &mut Vec<GpuVertex>, tip: Vec3) {
    let body_height = 18.0;
    let base_radius = 1.2;
    let top_radius = 3.6;
    let sides = 8;
    let base_color = colors::preview_pen_base();
    let top_center = tip + vec3(0.0, 0.0, body_height);

    for index in 0..sides {
        let a0 = index as f32 / sides as f32 * std::f32::consts::TAU;
        let a1 = (index + 1) as f32 / sides as f32 * std::f32::consts::TAU;

        let bottom_a = tip + vec3(a0.cos() * base_radius, a0.sin() * base_radius, 0.0);
        let bottom_b = tip + vec3(a1.cos() * base_radius, a1.sin() * base_radius, 0.0);
        let top_a = tip + vec3(a0.cos() * top_radius, a0.sin() * top_radius, body_height);
        let top_b = tip + vec3(a1.cos() * top_radius, a1.sin() * top_radius, body_height);

        append_triangle(triangle_vertices, bottom_a, top_a, top_b, base_color);
        append_triangle(triangle_vertices, bottom_a, top_b, bottom_b, base_color);
        append_triangle(triangle_vertices, top_center, top_b, top_a, colors::preview_pen_cap());
    }
}

fn append_line(vertices: &mut Vec<GpuVertex>, start: Vec3, end: Vec3, color: [f32; 4]) {
    vertices.push(GpuVertex { position: start.to_array(), color });
    vertices.push(GpuVertex { position: end.to_array(), color });
}

fn append_triangle(vertices: &mut Vec<GpuVertex>, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4]) {
    vertices.push(GpuVertex { position: a.to_array(), color });
    vertices.push(GpuVertex { position: b.to_array(), color });
    vertices.push(GpuVertex { position: c.to_array(), color });
}

fn segment_out_of_bounds(segment: &MotionSegment, printable_area: PrintableArea) -> bool {
    point_out_of_bounds(segment.start, printable_area)
        || point_out_of_bounds(segment.end, printable_area)
}

fn point_out_of_bounds(point: Vec3, printable_area: PrintableArea) -> bool {
    point.x < -0.01
        || point.y < -0.01
        || point.x > printable_area.width_mm + 0.01
        || point.y > printable_area.height_mm + 0.01
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::model::ToolpathStats;

    #[test]
    fn two_d_projection_round_trips_bed_points() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let state = ViewportState::with_view_mode(PreviewViewMode::TwoD);
        let view_projection =
            view_projection_for_area(PrintableArea::new(220.0, 180.0), rect.size(), &state);
        let point = Vec2::new(145.0, 72.5);

        let projected = project_bed_point(point, rect, view_projection)
            .expect("point should project in 2D mode");
        let restored =
            screen_to_bed(projected, rect, view_projection).expect("screen point should unproject");

        assert!((restored - point).length() < 1e-3);
    }

    #[test]
    fn interactive_quality_reduces_dense_toolpaths() {
        let segment_count = INTERACTIVE_TOOLPATH_SEGMENT_BUDGET + 24;
        let mut segments = Vec::with_capacity(segment_count);
        let mut segment_end_times_s = Vec::with_capacity(segment_count);

        for index in 0..segment_count {
            let start = vec3(index as f32, (index % 2) as f32, 0.0);
            let end = vec3((index + 1) as f32, ((index + 1) % 2) as f32, 0.0);
            segments.push(MotionSegment { start, end, kind: MotionKind::Draw, duration_s: 1.0 });
            segment_end_times_s.push((index + 1) as f32);
        }

        let last_end = segments.last().expect("expected dense segments").end;
        let plan = test_plan(PrintableArea::new(16_000.0, 200.0), segments, segment_end_times_s);
        let geometry = PreviewGeometry::from_plan(&plan, 1.0, false, false);

        assert!(geometry.interactive_line_vertices.len() < geometry.full_line_vertices.len());
        let last_vertex = geometry
            .interactive_line_vertices
            .last()
            .expect("expected an interactive toolpath line");
        assert_eq!(last_vertex.position, last_end.to_array());
    }

    #[test]
    fn interactive_quality_keeps_overflow_style_boundaries() {
        let printable_area = PrintableArea::new(20.0, 20.0);
        let segments = vec![
            MotionSegment {
                start: vec3(1.0, 1.0, 0.0),
                end: vec3(2.0, 1.0, 0.0),
                kind: MotionKind::Draw,
                duration_s: 1.0,
            },
            MotionSegment {
                start: vec3(2.0, 1.0, 0.0),
                end: vec3(25.0, 1.0, 0.0),
                kind: MotionKind::Draw,
                duration_s: 1.0,
            },
            MotionSegment {
                start: vec3(25.0, 1.0, 0.0),
                end: vec3(15.0, 1.0, 0.0),
                kind: MotionKind::Draw,
                duration_s: 1.0,
            },
            MotionSegment {
                start: vec3(15.0, 1.0, 0.0),
                end: vec3(16.0, 1.0, 0.0),
                kind: MotionKind::Draw,
                duration_s: 1.0,
            },
        ];

        let mut line_vertices = Vec::new();
        append_interactive_segments_with_stride(
            &mut line_vertices,
            &segments,
            None,
            printable_area,
            false,
            8,
        );

        assert_eq!(line_vertices.len(), 6);
        assert_eq!(line_vertices[0].color, colors::preview_draw());
        assert_eq!(line_vertices[2].color, colors::preview_overflow());
        assert_eq!(line_vertices[4].color, colors::preview_draw());
    }

    fn test_plan(
        printable_area: PrintableArea,
        segments: Vec<MotionSegment>,
        segment_end_times_s: Vec<f32>,
    ) -> ToolpathPlan {
        ToolpathPlan {
            source_name: "test".into(),
            printable_area,
            drawing_origin: Vec2::ZERO,
            drawing_bounds: Vec2::new(printable_area.width_mm, printable_area.height_mm),
            first_draw_point: segments
                .iter()
                .find(|segment| segment.kind == MotionKind::Draw)
                .map(|segment| segment.start.truncate()),
            is_out_of_bounds: false,
            stats: ToolpathStats {
                drawing_distance_mm: 0.0,
                travel_distance_mm: 0.0,
                stroke_count: 1,
                segment_count: segments.len(),
                estimated_duration_s: segment_end_times_s.last().copied().unwrap_or(0.0),
            },
            segments,
            segment_end_times_s,
            gcode_lines: Vec::new(),
            warnings: Vec::new(),
        }
    }
}
