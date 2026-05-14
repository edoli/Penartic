use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PrintableArea {
    pub width_mm: f32,
    pub height_mm: f32,
}

impl PrintableArea {
    pub fn new(width_mm: f32, height_mm: f32) -> Self {
        Self { width_mm: width_mm.max(10.0), height_mm: height_mm.max(10.0) }
    }
}

impl Default for PrintableArea {
    fn default() -> Self {
        Self::new(220.0, 220.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSettings {
    pub printable_area: PrintableArea,
    pub print_speed_mm_s: f32,
    pub lift_height_mm: f32,
}

impl ToolSettings {
    pub fn sanitize(&mut self) {
        self.printable_area =
            PrintableArea::new(self.printable_area.width_mm, self.printable_area.height_mm);
        self.print_speed_mm_s = self.print_speed_mm_s.clamp(1.0, 500.0);
        self.lift_height_mm = self.lift_height_mm.clamp(0.1, 25.0);
    }

    pub fn print_feed_rate(&self) -> f32 {
        self.print_speed_mm_s.max(1.0) * 60.0
    }

    pub fn travel_feed_rate(&self) -> f32 {
        (self.print_feed_rate() * 2.0).max(1800.0)
    }
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            printable_area: PrintableArea::default(),
            print_speed_mm_s: 30.0,
            lift_height_mm: 2.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    Travel,
    Draw,
}

#[derive(Debug, Clone, Copy)]
pub struct MotionSegment {
    pub start: Vec3,
    pub end: Vec3,
    pub kind: MotionKind,
}

impl MotionSegment {
    pub fn length(&self) -> f32 {
        self.start.distance(self.end)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ToolpathStats {
    pub drawing_distance_mm: f32,
    pub travel_distance_mm: f32,
    pub stroke_count: usize,
    pub segment_count: usize,
    pub estimated_duration_s: f32,
}

#[derive(Debug, Clone)]
pub struct ToolpathPlan {
    pub source_name: String,
    pub printable_area: PrintableArea,
    pub drawing_bounds: Vec2,
    pub segments: Vec<MotionSegment>,
    pub gcode_lines: Vec<String>,
    pub warnings: Vec<String>,
    pub stats: ToolpathStats,
}

impl ToolpathPlan {
    pub fn gcode_text(&self) -> String {
        self.gcode_lines.join("\n")
    }

    pub fn progress_state(&self, progress: f32) -> (usize, Option<MotionSegment>, Vec3) {
        let clamped = progress.clamp(0.0, 1.0);

        if self.segments.is_empty() {
            return (0, None, Vec3::ZERO);
        }

        if clamped <= f32::EPSILON {
            return (0, None, self.segments[0].start);
        }

        let total = self.segments.len() as f32;
        let scaled = clamped * total;
        let finished_count = scaled.floor() as usize;

        if finished_count >= self.segments.len() {
            let pen = self.segments.last().map(|segment| segment.end).unwrap_or(Vec3::ZERO);
            return (self.segments.len(), None, pen);
        }

        let fraction = scaled.fract();
        let current = self.segments[finished_count];

        if fraction <= f32::EPSILON {
            (finished_count, None, current.start)
        } else {
            let partial = MotionSegment {
                start: current.start,
                end: current.start.lerp(current.end, fraction),
                kind: current.kind,
            };
            (finished_count, Some(partial), partial.end)
        }
    }
}
