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

#[derive(Debug, Clone, Copy)]
pub struct SvgPlacement {
    pub center_mm: Vec2,
    pub scale_mm_per_unit: f32,
}

impl SvgPlacement {
    pub fn new(center_mm: Vec2, scale_mm_per_unit: f32) -> Self {
        let mut placement = Self { center_mm, scale_mm_per_unit };
        placement.sanitize();
        placement
    }

    pub fn sanitize(&mut self) {
        if !self.center_mm.is_finite() {
            self.center_mm = Vec2::ZERO;
        }
        if !self.scale_mm_per_unit.is_finite() {
            self.scale_mm_per_unit = 1.0;
        }
        self.scale_mm_per_unit = self.scale_mm_per_unit.max(1e-4);
    }

    pub fn drawing_origin(&self, drawing_bounds: Vec2) -> Vec2 {
        self.center_mm - drawing_bounds * 0.5
    }
}

fn default_corner_smoothing_enabled() -> bool {
    true
}

fn default_corner_smoothing_radius_mm() -> f32 {
    0.3
}

fn default_corner_smoothing_angle_deg() -> f32 {
    45.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSettings {
    pub printable_area: PrintableArea,
    pub print_speed_mm_s: f32,
    pub lift_height_mm: f32,
    #[serde(default)]
    pub print_start_mode: PrintStartMode,
    #[serde(default)]
    pub curve_output_mode: CurveOutputMode,
    #[serde(default = "default_corner_smoothing_enabled")]
    pub corner_smoothing_enabled: bool,
    #[serde(default = "default_corner_smoothing_radius_mm")]
    pub corner_smoothing_radius_mm: f32,
    #[serde(default = "default_corner_smoothing_angle_deg")]
    pub corner_smoothing_angle_deg: f32,
}

impl ToolSettings {
    pub fn sanitize(&mut self) {
        self.printable_area =
            PrintableArea::new(self.printable_area.width_mm, self.printable_area.height_mm);
        self.print_speed_mm_s = self.print_speed_mm_s.clamp(1.0, 500.0);
        self.lift_height_mm = self.lift_height_mm.clamp(0.1, 25.0);
        self.corner_smoothing_radius_mm = self.corner_smoothing_radius_mm.clamp(0.1, 10.0);
        self.corner_smoothing_angle_deg = self.corner_smoothing_angle_deg.clamp(5.0, 170.0);
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
            lift_height_mm: 1.0,
            print_start_mode: PrintStartMode::default(),
            curve_output_mode: CurveOutputMode::default(),
            corner_smoothing_enabled: default_corner_smoothing_enabled(),
            corner_smoothing_radius_mm: default_corner_smoothing_radius_mm(),
            corner_smoothing_angle_deg: default_corner_smoothing_angle_deg(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrintStartMode {
    HomeBeforePrint,
    #[default]
    DirectFromCurrentPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CurveOutputMode {
    #[default]
    LinearSegments,
    PreferG2G3,
    PreferG5,
    PreferG2G3AndG5,
}

impl CurveOutputMode {
    pub fn from_flags(prefer_g2g3: bool, prefer_g5: bool) -> Self {
        match (prefer_g2g3, prefer_g5) {
            (false, false) => Self::LinearSegments,
            (true, false) => Self::PreferG2G3,
            (false, true) => Self::PreferG5,
            (true, true) => Self::PreferG2G3AndG5,
        }
    }

    pub fn prefers_g2g3(self) -> bool {
        matches!(self, Self::PreferG2G3 | Self::PreferG2G3AndG5)
    }

    pub fn prefers_g5(self) -> bool {
        matches!(self, Self::PreferG5 | Self::PreferG2G3AndG5)
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
    pub duration_s: f32,
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
    pub drawing_origin: Vec2,
    pub drawing_bounds: Vec2,
    pub first_draw_point: Option<Vec2>,
    pub is_out_of_bounds: bool,
    pub segments: Vec<MotionSegment>,
    pub segment_end_times_s: Vec<f32>,
    pub gcode_lines: Vec<String>,
    pub warnings: Vec<String>,
    pub stats: ToolpathStats,
}

impl ToolpathPlan {
    pub fn gcode_text(&self) -> String {
        self.gcode_lines.join("\n")
    }

    pub fn total_duration_s(&self) -> f32 {
        self.segment_end_times_s.last().copied().unwrap_or(self.stats.estimated_duration_s).max(0.0)
    }

    pub fn elapsed_duration_s(&self, progress: f32) -> f32 {
        progress.clamp(0.0, 1.0) * self.total_duration_s()
    }

    pub fn progress_state(&self, progress: f32) -> (usize, Option<MotionSegment>, Vec3) {
        let clamped = progress.clamp(0.0, 1.0);

        if self.segments.is_empty() {
            return (0, None, Vec3::ZERO);
        }

        if clamped <= f32::EPSILON {
            return (0, None, self.segments[0].start);
        }

        let total_duration_s = self.total_duration_s();
        if total_duration_s <= f32::EPSILON {
            let pen = self.segments.last().map(|segment| segment.end).unwrap_or(Vec3::ZERO);
            return (self.segments.len(), None, pen);
        }

        let elapsed_s = clamped * total_duration_s;
        let finished_count =
            self.segment_end_times_s.partition_point(|end_time| *end_time < elapsed_s);

        if finished_count >= self.segments.len() {
            let pen = self.segments.last().map(|segment| segment.end).unwrap_or(Vec3::ZERO);
            return (self.segments.len(), None, pen);
        }

        let current = self.segments[finished_count];
        if current.duration_s <= f32::EPSILON {
            return (finished_count + 1, None, current.end);
        }

        let segment_start_s =
            if finished_count == 0 { 0.0 } else { self.segment_end_times_s[finished_count - 1] };
        if elapsed_s <= segment_start_s + f32::EPSILON {
            return (finished_count, None, current.start);
        }

        let fraction = ((elapsed_s - segment_start_s) / current.duration_s).clamp(0.0, 1.0);
        if fraction >= 1.0 - f32::EPSILON {
            return (finished_count + 1, None, current.end);
        }

        let partial = MotionSegment {
            start: current.start,
            end: current.start.lerp(current.end, fraction),
            kind: current.kind,
            duration_s: current.duration_s * fraction,
        };
        (finished_count, Some(partial), partial.end)
    }
}

#[cfg(test)]
mod tests {
    use glam::{vec2, vec3};

    use super::*;

    #[test]
    fn progress_uses_motion_time_instead_of_segment_count() {
        let plan = ToolpathPlan {
            source_name: "timing.svg".to_owned(),
            printable_area: PrintableArea::new(220.0, 220.0),
            drawing_origin: vec2(0.0, 0.0),
            drawing_bounds: vec2(110.0, 0.0),
            first_draw_point: Some(vec2(0.0, 0.0)),
            is_out_of_bounds: false,
            segments: vec![
                MotionSegment {
                    start: vec3(0.0, 0.0, 0.0),
                    end: vec3(100.0, 0.0, 0.0),
                    kind: MotionKind::Draw,
                    duration_s: 10.0,
                },
                MotionSegment {
                    start: vec3(100.0, 0.0, 0.0),
                    end: vec3(110.0, 0.0, 0.0),
                    kind: MotionKind::Draw,
                    duration_s: 1.0,
                },
            ],
            segment_end_times_s: vec![10.0, 11.0],
            gcode_lines: Vec::new(),
            warnings: Vec::new(),
            stats: ToolpathStats {
                drawing_distance_mm: 110.0,
                travel_distance_mm: 0.0,
                stroke_count: 1,
                segment_count: 2,
                estimated_duration_s: 11.0,
            },
        };

        let (finished, partial, pen_position) = plan.progress_state(0.5);
        assert_eq!(finished, 0);
        let partial = partial.expect("expected an in-flight segment at 50% of total time");
        assert!((partial.end.x - 55.0).abs() < 1e-3);
        assert!((pen_position.x - 55.0).abs() < 1e-3);
    }

    #[test]
    fn default_settings_use_short_lift_and_direct_start() {
        let settings = ToolSettings::default();
        assert_eq!(settings.lift_height_mm, 0.5);
        assert_eq!(settings.print_start_mode, PrintStartMode::DirectFromCurrentPosition);
    }
}
