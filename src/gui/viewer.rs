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

use crate::{
    plot::model::{MotionKind, MotionSegment, PrintableArea, ToolpathPlan},
    res::colors,
};

#[derive(Debug, Clone)]
pub struct ViewportState {
    yaw: f32,
    pitch: f32,
    zoom: f32,
    pan: Vec2,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self { yaw: -std::f32::consts::FRAC_PI_2, pitch: 0.75, zoom: 1.15, pan: Vec2::ZERO }
    }
}

impl ViewportState {
    fn handle_input(&mut self, response: &egui::Response, ui: &egui::Ui, scene_extent: f32) {
        if response.dragged_by(egui::PointerButton::Primary) {
            let drag = response.drag_motion();
            self.yaw -= drag.x * 0.01;
            self.pitch = (self.pitch + drag.y * 0.01).clamp(0.2, 1.35);
        }

        if response.dragged_by(egui::PointerButton::Secondary) {
            let drag = response.drag_motion();
            let pan_scale = scene_extent.max(40.0)
                / response.rect.width().min(response.rect.height()).max(1.0)
                * self.zoom;
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
            self.pan += pan_delta.truncate();
        }

        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > f32::EPSILON {
                self.zoom = (self.zoom * (1.0 - scroll * 0.0015)).clamp(0.55, 2.4);
            }
        }
    }
}

pub struct PreviewRenderer {
    ready: bool,
    cache: RefCell<Option<PreviewGeometryCache>>,
}

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
        state: &mut ViewportState,
    ) -> egui::Rect {
        let desired = egui::vec2(desired_size.x.max(1.0), desired_size.y.max(1.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::drag());
        let scene_extent = plan
            .map(|plan| plan.printable_area.width_mm.max(plan.printable_area.height_mm))
            .unwrap_or(220.0);
        state.handle_input(&response, ui, scene_extent);

        ui.painter().rect_filled(rect, 0.0, colors::preview_background());

        if !self.ready {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "WGPU preview unavailable",
                egui::TextStyle::Heading.resolve(ui.style()),
                colors::error(),
            );
            return rect;
        }

        let Some(plan) = plan else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "SVG를 불러오면 3D 미리보기가 여기에 표시됩니다.",
                egui::TextStyle::Heading.resolve(ui.style()),
                colors::muted_text(),
            );
            return rect;
        };

        let view_projection = view_projection_for(plan, rect.size(), state);
        let geometry = self.cached_geometry(plan, progress);

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            PreviewPaintCallback {
                geometry_id: Arc::as_ptr(&geometry) as usize,
                geometry,
                view_projection: CameraUniform::new(view_projection),
            },
        ));
        rect
    }

    fn cached_geometry(&self, plan: &ToolpathPlan, progress: f32) -> Arc<PreviewGeometry> {
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
        };

        let mut cache = self.cache.borrow_mut();
        if let Some(cache) = cache.as_ref() {
            if cache.key == key {
                return cache.geometry.clone();
            }
        }

        let geometry = Arc::new(PreviewGeometry::from_plan(plan, progress));
        *cache = Some(PreviewGeometryCache { key, geometry: geometry.clone() });
        geometry
    }
}

#[derive(Clone)]
struct PreviewGeometry {
    triangle_vertices: Vec<GpuVertex>,
    line_vertices: Vec<GpuVertex>,
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
}

struct PreviewGeometryCache {
    key: PreviewGeometryKey,
    geometry: Arc<PreviewGeometry>,
}

impl PreviewGeometry {
    fn from_plan(plan: &ToolpathPlan, progress: f32) -> Self {
        let mut triangle_vertices = Vec::new();
        let mut line_vertices = Vec::new();

        append_bed(&mut triangle_vertices, &mut line_vertices, plan);
        if plan.is_out_of_bounds {
            append_out_of_bounds_outline(&mut line_vertices, plan);
        }

        let (finished, partial, pen_position) = plan.progress_state(progress);
        for segment in plan.segments.iter().take(finished) {
            append_segment(&mut line_vertices, segment, plan.printable_area);
        }
        if let Some(partial) = partial {
            append_segment(&mut line_vertices, &partial, plan.printable_area);
        }

        append_pen(&mut triangle_vertices, pen_position);

        Self { triangle_vertices, line_vertices }
    }
}

#[derive(Clone)]
struct PreviewPaintCallback {
    geometry_id: usize,
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
    geometry_id: Option<usize>,
}

impl PreviewRenderResources {
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &CameraUniform,
        geometry_id: usize,
        geometry: &PreviewGeometry,
    ) {
        self.update_camera(device, queue, camera);

        if self.geometry_id == Some(geometry_id) {
            return;
        }
        self.geometry_id = Some(geometry_id);

        let triangle_vertices = &geometry.triangle_vertices;
        let line_vertices = &geometry.line_vertices;
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

fn view_projection_for(
    plan: &ToolpathPlan,
    viewport_size: egui::Vec2,
    state: &ViewportState,
) -> Mat4 {
    let aspect = (viewport_size.x / viewport_size.y).max(0.1);
    let center = vec3(
        plan.printable_area.width_mm * 0.5 + state.pan.x,
        plan.printable_area.height_mm * 0.5 + state.pan.y,
        0.0,
    );

    let scene_radius =
        plan.printable_area.width_mm.max(plan.printable_area.height_mm).max(80.0) * 0.9;
    let eye_direction = vec3(
        state.yaw.cos() * state.pitch.cos(),
        state.yaw.sin() * state.pitch.cos(),
        state.pitch.sin(),
    );
    let eye = center + eye_direction * scene_radius * state.zoom + vec3(0.0, 0.0, 24.0);

    let view = Mat4::look_at_rh(eye, center + vec3(0.0, 0.0, 2.0), Vec3::Z);
    let projection = Mat4::perspective_rh(35_f32.to_radians(), aspect, 0.1, 5_000.0);
    projection * view
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
) {
    let color = if segment_out_of_bounds(segment, printable_area) {
        colors::preview_overflow()
    } else {
        match segment.kind {
            MotionKind::Travel => colors::preview_travel(),
            MotionKind::Draw => colors::preview_draw(),
        }
    };
    append_line(line_vertices, segment.start, segment.end, color);
}

fn append_out_of_bounds_outline(line_vertices: &mut Vec<GpuVertex>, plan: &ToolpathPlan) {
    let z = 0.15;
    let min = vec3(plan.drawing_origin.x, plan.drawing_origin.y, z);
    let max = vec3(
        plan.drawing_origin.x + plan.drawing_bounds.x,
        plan.drawing_origin.y + plan.drawing_bounds.y,
        z,
    );
    let color = colors::preview_overflow();

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
