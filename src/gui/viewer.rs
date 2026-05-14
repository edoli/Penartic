use bytemuck::{Pod, Zeroable};
use eframe::{
    egui,
    egui_wgpu::{
        self,
        wgpu::{self, util::DeviceExt as _},
    },
};
use glam::{Mat4, Vec2, Vec3, vec3};

use crate::{
    plot::model::{MotionKind, MotionSegment, ToolpathPlan},
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
        Self { yaw: 0.65, pitch: 0.75, zoom: 1.15, pan: Vec2::ZERO }
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
            let right = vec3(-self.yaw.sin(), self.yaw.cos(), 0.0);
            let forward = vec3(self.yaw.cos(), self.yaw.sin(), 0.0);
            let pan_delta = (-right * drag.x + forward * drag.y) * pan_scale;
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
}

impl PreviewRenderer {
    pub fn new(cc: &eframe::CreationContext<'_>, msaa_samples: u32) -> Self {
        let Some(render_state) = cc.wgpu_render_state.as_ref() else {
            return Self { ready: false };
        };

        let device = &render_state.device;
        let sample_count = msaa_samples.max(1);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("penartic-preview-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("preview_shader.wgsl").into()),
        });

        let target = Some(render_state.target_format.into());
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
        };

        let triangle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("penartic-preview-triangle-pipeline"),
            layout: None,
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
            layout: None,
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
            triangle_buffer: None,
            line_buffer: None,
            triangle_capacity: 0,
            line_capacity: 0,
            triangle_vertex_count: 0,
            line_vertex_count: 0,
        });

        Self { ready: true }
    }

    pub fn show(
        &self,
        ui: &mut egui::Ui,
        desired_size: egui::Vec2,
        plan: Option<&ToolpathPlan>,
        progress: f32,
        state: &mut ViewportState,
    ) {
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
            return;
        }

        let Some(plan) = plan else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "SVG를 불러오면 3D 미리보기가 여기에 표시됩니다.",
                egui::TextStyle::Heading.resolve(ui.style()),
                colors::muted_text(),
            );
            return;
        };

        let geometry = PreviewGeometry::from_plan(plan, progress, rect.size(), state);

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            PreviewPaintCallback {
                triangle_vertices: geometry.triangle_vertices,
                line_vertices: geometry.line_vertices,
            },
        ));
    }
}

#[derive(Clone)]
struct PreviewGeometry {
    triangle_vertices: Vec<GpuVertex>,
    line_vertices: Vec<GpuVertex>,
}

impl PreviewGeometry {
    fn from_plan(
        plan: &ToolpathPlan,
        progress: f32,
        viewport_size: egui::Vec2,
        state: &ViewportState,
    ) -> Self {
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
        let view_projection = projection * view;

        let mut triangle_vertices = Vec::new();
        let mut line_vertices = Vec::new();

        append_bed(&mut triangle_vertices, &mut line_vertices, plan, view_projection);

        let (finished, partial, pen_position) = plan.progress_state(progress);
        for segment in plan.segments.iter().take(finished) {
            append_segment(&mut line_vertices, segment, view_projection);
        }
        if let Some(partial) = partial {
            append_segment(&mut line_vertices, &partial, view_projection);
        }

        append_pen(&mut triangle_vertices, pen_position, view_projection);

        Self { triangle_vertices, line_vertices }
    }
}

#[derive(Clone)]
struct PreviewPaintCallback {
    triangle_vertices: Vec<GpuVertex>,
    line_vertices: Vec<GpuVertex>,
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
        resources.update(device, queue, &self.triangle_vertices, &self.line_vertices);
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
    triangle_buffer: Option<wgpu::Buffer>,
    line_buffer: Option<wgpu::Buffer>,
    triangle_capacity: usize,
    line_capacity: usize,
    triangle_vertex_count: u32,
    line_vertex_count: u32,
}

impl PreviewRenderResources {
    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        triangle_vertices: &[GpuVertex],
        line_vertices: &[GpuVertex],
    ) {
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

    fn paint(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        if self.triangle_vertex_count > 0 {
            render_pass.set_pipeline(&self.triangle_pipeline);
            if let Some(buffer) = &self.triangle_buffer {
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(0..self.triangle_vertex_count, 0..1);
            }
        }

        if self.line_vertex_count > 0 {
            render_pass.set_pipeline(&self.line_pipeline);
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
        *buffer = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::VERTEX,
        }));
    } else if let Some(existing) = buffer.as_ref() {
        queue.write_buffer(existing, 0, bytes);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 2],
    color: [f32; 4],
}

fn append_bed(
    triangle_vertices: &mut Vec<GpuVertex>,
    line_vertices: &mut Vec<GpuVertex>,
    plan: &ToolpathPlan,
    view_projection: Mat4,
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
        view_projection,
    );
    append_triangle(
        triangle_vertices,
        vec3(0.0, 0.0, 0.0),
        vec3(w, h, 0.0),
        vec3(0.0, h, 0.0),
        plane_color,
        view_projection,
    );

    let mut grid_x = 0.0;
    while grid_x <= w + 0.1 {
        append_line(
            line_vertices,
            vec3(grid_x, 0.0, 0.05),
            vec3(grid_x, h, 0.05),
            if grid_x == 0.0 || (grid_x - w).abs() <= 0.1 { edge_color } else { grid_color },
            view_projection,
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
            view_projection,
        );
        grid_y += 20.0;
    }
}

fn append_segment(
    line_vertices: &mut Vec<GpuVertex>,
    segment: &MotionSegment,
    view_projection: Mat4,
) {
    let color = match segment.kind {
        MotionKind::Travel => colors::preview_travel(),
        MotionKind::Draw => colors::preview_draw(),
    };
    append_line(line_vertices, segment.start, segment.end, color, view_projection);
}

fn append_pen(triangle_vertices: &mut Vec<GpuVertex>, tip: Vec3, view_projection: Mat4) {
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

        append_triangle(triangle_vertices, bottom_a, top_a, top_b, base_color, view_projection);
        append_triangle(triangle_vertices, bottom_a, top_b, bottom_b, base_color, view_projection);
        append_triangle(
            triangle_vertices,
            top_center,
            top_b,
            top_a,
            colors::preview_pen_cap(),
            view_projection,
        );
    }
}

fn append_line(
    vertices: &mut Vec<GpuVertex>,
    start: Vec3,
    end: Vec3,
    color: [f32; 4],
    view_projection: Mat4,
) {
    let Some(start) = project_point(start, view_projection) else {
        return;
    };
    let Some(end) = project_point(end, view_projection) else {
        return;
    };

    vertices.push(GpuVertex { position: start, color });
    vertices.push(GpuVertex { position: end, color });
}

fn append_triangle(
    vertices: &mut Vec<GpuVertex>,
    a: Vec3,
    b: Vec3,
    c: Vec3,
    color: [f32; 4],
    view_projection: Mat4,
) {
    let Some(a) = project_point(a, view_projection) else {
        return;
    };
    let Some(b) = project_point(b, view_projection) else {
        return;
    };
    let Some(c) = project_point(c, view_projection) else {
        return;
    };

    vertices.push(GpuVertex { position: a, color });
    vertices.push(GpuVertex { position: b, color });
    vertices.push(GpuVertex { position: c, color });
}

fn project_point(point: Vec3, view_projection: Mat4) -> Option<[f32; 2]> {
    let clip = view_projection * point.extend(1.0);
    if clip.w.abs() <= 1e-6 {
        return None;
    }

    let ndc = clip.truncate() / clip.w;
    if ndc.z < -1.5 || ndc.z > 1.5 {
        return None;
    }

    Some([ndc.x, ndc.y])
}
