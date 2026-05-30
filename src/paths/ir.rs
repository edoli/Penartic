use glam::Vec2;

const LINEAR_SEGMENT_LENGTH_STEP_MM: f32 = 12.0;
const CURVE_ARC_LENGTH_SUBDIVISION_MULTIPLIER: usize = 4;
const MIN_CURVE_ARC_LENGTH_SUBDIVISIONS: usize = 16;
const MAX_CURVE_ARC_LENGTH_SUBDIVISIONS: usize = 256;
const MIN_QUADRATIC_SUBDIVISIONS: usize = 6;
const MAX_QUADRATIC_SUBDIVISIONS: usize = 48;
const MIN_CUBIC_SUBDIVISIONS: usize = 8;
const MAX_CUBIC_SUBDIVISIONS: usize = 96;
const SEGMENT_EPSILON: f32 = 1e-4;

pub type StrokeSegments = Vec<Segment>;
pub type Strokes = Vec<Stroke>;
pub type FillRegions = Vec<FillRegion>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone)]
pub struct FillRegion {
    pub contours: Strokes,
    pub rule: FillRule,
}

impl FillRegion {
    pub fn new(contours: impl Into<Strokes>, rule: FillRule) -> Self {
        Self { contours: contours.into(), rule }
    }

    pub fn is_empty(&self) -> bool {
        self.contours.is_empty() || self.contours.iter().all(Stroke::is_empty)
    }

    pub fn transformed(&self, map: impl Fn(Vec2) -> Vec2 + Copy) -> Self {
        Self {
            contours: self.contours.iter().map(|stroke| stroke.transformed(map)).collect(),
            rule: self.rule,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Stroke {
    pub segments: StrokeSegments,
}

impl Stroke {
    pub fn new(segments: impl Into<StrokeSegments>) -> Self {
        Self { segments: segments.into() }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn start_point(&self) -> Option<Vec2> {
        self.segments.first().map(Segment::start_point)
    }

    pub fn end_point(&self) -> Option<Vec2> {
        self.segments.last().map(Segment::end_point)
    }

    pub fn reversed(&self) -> Self {
        Self { segments: self.segments.iter().rev().map(Segment::reversed).collect() }
    }

    pub fn approximate_length(&self) -> f32 {
        self.segments.iter().map(Segment::approximate_length).sum()
    }

    pub fn flatten_points(&self) -> Vec<Vec2> {
        let Some(first_segment) = self.segments.first() else {
            return Vec::new();
        };

        let mut points = vec![first_segment.start_point()];
        for segment in &self.segments {
            let segment_points = segment.flatten_points();
            points.extend(segment_points.into_iter().skip(1));
        }
        simplify_polyline(&points)
    }

    pub fn merge_short_segments(
        &self,
        min_segment_length: f32,
        min_stroke_length: f32,
    ) -> Option<Self> {
        if self.segments.is_empty() {
            return None;
        }

        let mut merged: StrokeSegments = Vec::with_capacity(self.segments.len());
        let mut pending_start = None;

        for mut segment in self.segments.iter().copied() {
            if let Some(start) = pending_start.take() {
                segment = segment.with_start_point(start);
            }

            if segment.approximate_length() < min_segment_length {
                if let Some(previous) = merged.last_mut() {
                    *previous = previous.with_end_point(segment.end_point());
                } else {
                    pending_start = Some(segment.start_point());
                }
                continue;
            }

            merged.push(segment);
        }

        let merged = Self::new(merged);
        if merged.approximate_length() < min_stroke_length {
            return None;
        }

        Some(merged)
    }

    pub fn append_if_gap_within(&mut self, mut next: Self, max_gap: f32) -> bool {
        let Some(end) = self.end_point() else {
            return false;
        };
        let Some(start) = next.start_point() else {
            return false;
        };

        if end.distance_squared(start) > max_gap * max_gap {
            return false;
        }

        if let Some(first) = next.segments.first_mut() {
            *first = first.with_start_point(end);
        }
        self.segments.extend(next.segments);
        true
    }

    pub fn transformed(&self, map: impl Fn(Vec2) -> Vec2 + Copy) -> Self {
        Self {
            segments: self
                .segments
                .iter()
                .copied()
                .map(|segment| segment.transformed(map))
                .collect(),
        }
    }

    pub fn apply_dash_pattern(&self, dash_pattern: &DashPattern) -> Strokes {
        if self.segments.is_empty() {
            return Vec::new();
        }

        let Some(mut cursor) = DashCursor::new(dash_pattern) else {
            return vec![self.clone()];
        };

        let mut dashed_strokes: Strokes = Vec::new();
        let mut visible_segments: StrokeSegments = Vec::new();

        for segment in &self.segments {
            let segment_length = segment.approximate_length();
            if segment_length <= SEGMENT_EPSILON {
                continue;
            }

            let mut consumed_length = 0.0;
            while consumed_length < segment_length - SEGMENT_EPSILON {
                let next_length = (consumed_length + cursor.remaining_length)
                    .min(segment_length)
                    .max(consumed_length);

                if cursor.draw && next_length > consumed_length + SEGMENT_EPSILON {
                    if let Some(slice) =
                        segment.slice_by_arc_length(consumed_length, next_length, segment_length)
                    {
                        visible_segments.push(slice);
                    }
                }

                let step_length = next_length - consumed_length;
                consumed_length = next_length;
                cursor.consume(step_length.max(0.0));

                if cursor.just_switched_to_gap() && !visible_segments.is_empty() {
                    dashed_strokes.push(Self::new(std::mem::take(&mut visible_segments)));
                }
            }
        }

        if !visible_segments.is_empty() {
            dashed_strokes.push(Self::new(visible_segments));
        }

        dashed_strokes
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Segment {
    Line(LineSegment),
    Quadratic(QuadraticBezierSegment),
    Cubic(CubicBezierSegment),
}

impl Segment {
    pub fn line(start: Vec2, end: Vec2) -> Self {
        Self::Line(LineSegment { start, end })
    }

    pub fn quadratic(start: Vec2, control: Vec2, end: Vec2) -> Self {
        Self::Quadratic(QuadraticBezierSegment { start, control, end })
    }

    pub fn cubic(start: Vec2, control_a: Vec2, control_b: Vec2, end: Vec2) -> Self {
        Self::Cubic(CubicBezierSegment { start, control_a, control_b, end })
    }

    pub fn start_point(&self) -> Vec2 {
        match self {
            Self::Line(segment) => segment.start,
            Self::Quadratic(segment) => segment.start,
            Self::Cubic(segment) => segment.start,
        }
    }

    pub fn end_point(&self) -> Vec2 {
        match self {
            Self::Line(segment) => segment.end,
            Self::Quadratic(segment) => segment.end,
            Self::Cubic(segment) => segment.end,
        }
    }

    pub fn transformed(&self, map: impl Fn(Vec2) -> Vec2 + Copy) -> Self {
        match self {
            Self::Line(segment) => Self::line(map(segment.start), map(segment.end)),
            Self::Quadratic(segment) => {
                Self::quadratic(map(segment.start), map(segment.control), map(segment.end))
            }
            Self::Cubic(segment) => Self::cubic(
                map(segment.start),
                map(segment.control_a),
                map(segment.control_b),
                map(segment.end),
            ),
        }
    }

    pub fn reversed(&self) -> Self {
        match self {
            Self::Line(segment) => Self::line(segment.end, segment.start),
            Self::Quadratic(segment) => {
                Self::quadratic(segment.end, segment.control, segment.start)
            }
            Self::Cubic(segment) => {
                Self::cubic(segment.end, segment.control_b, segment.control_a, segment.start)
            }
        }
    }

    fn with_start_point(&self, start: Vec2) -> Self {
        match self {
            Self::Line(segment) => Self::line(start, segment.end),
            Self::Quadratic(segment) => Self::quadratic(start, segment.control, segment.end),
            Self::Cubic(segment) => {
                Self::cubic(start, segment.control_a, segment.control_b, segment.end)
            }
        }
    }

    fn with_end_point(&self, end: Vec2) -> Self {
        match self {
            Self::Line(segment) => Self::line(segment.start, end),
            Self::Quadratic(segment) => Self::quadratic(segment.start, segment.control, end),
            Self::Cubic(segment) => {
                Self::cubic(segment.start, segment.control_a, segment.control_b, end)
            }
        }
    }

    pub fn approximate_length(&self) -> f32 {
        let points = self.flatten_points();
        polyline_length(&points)
    }

    pub fn flatten_points(&self) -> Vec<Vec2> {
        match self {
            Self::Line(segment) => vec![segment.start, segment.end],
            Self::Quadratic(segment) => {
                let steps = quadratic_subdivisions(segment.start, segment.control, segment.end);
                let mut points = Vec::with_capacity(steps + 1);
                points.push(segment.start);
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    points.push(quadratic(segment.start, segment.control, segment.end, t));
                }
                points
            }
            Self::Cubic(segment) => {
                let steps = cubic_subdivisions(
                    segment.start,
                    segment.control_a,
                    segment.control_b,
                    segment.end,
                );
                let mut points = Vec::with_capacity(steps + 1);
                points.push(segment.start);
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    points.push(cubic(
                        segment.start,
                        segment.control_a,
                        segment.control_b,
                        segment.end,
                        t,
                    ));
                }
                points
            }
        }
    }

    pub fn slice_by_arc_length(
        &self,
        start_length: f32,
        end_length: f32,
        total_length: f32,
    ) -> Option<Self> {
        let clamped_start = start_length.clamp(0.0, total_length);
        let clamped_end = end_length.clamp(clamped_start, total_length);
        if clamped_end - clamped_start <= SEGMENT_EPSILON {
            return None;
        }

        if total_length <= SEGMENT_EPSILON {
            return None;
        }

        let start_t = self.t_at_arc_length(clamped_start, total_length);
        let end_t = self.t_at_arc_length(clamped_end, total_length);
        self.subsegment(start_t, end_t)
    }

    pub fn point_at(&self, t: f32) -> Vec2 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Line(segment) => segment.start.lerp(segment.end, t),
            Self::Quadratic(segment) => quadratic(segment.start, segment.control, segment.end, t),
            Self::Cubic(segment) => {
                cubic(segment.start, segment.control_a, segment.control_b, segment.end, t)
            }
        }
    }

    pub fn tangent_at(&self, t: f32) -> Vec2 {
        let derivative = self.derivative_at(t.clamp(0.0, 1.0));
        if derivative.length_squared() > SEGMENT_EPSILON * SEGMENT_EPSILON {
            return derivative.normalize();
        }

        for fallback_t in [0.001_f32, 0.01, 0.99, 0.999] {
            let derivative = self.derivative_at(fallback_t);
            if derivative.length_squared() > SEGMENT_EPSILON * SEGMENT_EPSILON {
                return derivative.normalize();
            }
        }

        Vec2::X
    }

    pub fn point_and_tangent_at_arc_length(
        &self,
        target_length: f32,
        total_length: f32,
    ) -> (Vec2, Vec2) {
        let t = self.t_at_arc_length(target_length, total_length);
        (self.point_at(t), self.tangent_at(t))
    }

    fn t_at_arc_length(&self, target_length: f32, total_length: f32) -> f32 {
        if target_length <= SEGMENT_EPSILON {
            return 0.0;
        }
        if total_length - target_length <= SEGMENT_EPSILON {
            return 1.0;
        }

        let steps = arc_length_subdivisions(self);
        let mut previous_point = self.point_at(0.0);
        let mut previous_t = 0.0;
        let mut accumulated = 0.0;

        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let point = self.point_at(t);
            let segment_length = previous_point.distance(point);
            let next_accumulated = accumulated + segment_length;

            if target_length <= next_accumulated {
                let local_fraction = if segment_length <= SEGMENT_EPSILON {
                    0.0
                } else {
                    (target_length - accumulated) / segment_length
                };
                return previous_t + (t - previous_t) * local_fraction.clamp(0.0, 1.0);
            }

            accumulated = next_accumulated;
            previous_point = point;
            previous_t = t;
        }

        1.0
    }

    fn derivative_at(&self, t: f32) -> Vec2 {
        match self {
            Self::Line(segment) => segment.end - segment.start,
            Self::Quadratic(segment) => {
                2.0 * (1.0 - t) * (segment.control - segment.start)
                    + 2.0 * t * (segment.end - segment.control)
            }
            Self::Cubic(segment) => {
                3.0 * (1.0 - t).powi(2) * (segment.control_a - segment.start)
                    + 6.0 * (1.0 - t) * t * (segment.control_b - segment.control_a)
                    + 3.0 * t.powi(2) * (segment.end - segment.control_b)
            }
        }
    }

    fn subsegment(&self, start_t: f32, end_t: f32) -> Option<Self> {
        let start_t = start_t.clamp(0.0, 1.0);
        let end_t = end_t.clamp(start_t, 1.0);
        if end_t - start_t <= SEGMENT_EPSILON {
            return None;
        }
        if start_t <= SEGMENT_EPSILON && end_t >= 1.0 - SEGMENT_EPSILON {
            return Some(*self);
        }

        let (left, _) = self.split(end_t);
        if start_t <= SEGMENT_EPSILON {
            return Some(left);
        }

        let relative_t = if end_t <= SEGMENT_EPSILON { 0.0 } else { start_t / end_t };
        let (_, middle) = left.split(relative_t);
        Some(middle)
    }

    fn split(&self, t: f32) -> (Self, Self) {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Line(segment) => {
                let split = segment.start.lerp(segment.end, t);
                (Self::line(segment.start, split), Self::line(split, segment.end))
            }
            Self::Quadratic(segment) => {
                let a = segment.start.lerp(segment.control, t);
                let b = segment.control.lerp(segment.end, t);
                let split = a.lerp(b, t);
                (Self::quadratic(segment.start, a, split), Self::quadratic(split, b, segment.end))
            }
            Self::Cubic(segment) => {
                let a = segment.start.lerp(segment.control_a, t);
                let b = segment.control_a.lerp(segment.control_b, t);
                let c = segment.control_b.lerp(segment.end, t);
                let d = a.lerp(b, t);
                let e = b.lerp(c, t);
                let split = d.lerp(e, t);
                (Self::cubic(segment.start, a, d, split), Self::cubic(split, e, c, segment.end))
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LineSegment {
    pub start: Vec2,
    pub end: Vec2,
}

#[derive(Debug, Clone, Copy)]
pub struct QuadraticBezierSegment {
    pub start: Vec2,
    pub control: Vec2,
    pub end: Vec2,
}

#[derive(Debug, Clone, Copy)]
pub struct CubicBezierSegment {
    pub start: Vec2,
    pub control_a: Vec2,
    pub control_b: Vec2,
    pub end: Vec2,
}

#[derive(Debug, Clone)]
pub struct DashPattern {
    pattern: Vec<f32>,
    offset: f32,
}

impl DashPattern {
    pub fn new(pattern: &[f32], offset: f32) -> Option<Self> {
        let mut cleaned = pattern
            .iter()
            .copied()
            .filter(|entry| entry.is_finite() && *entry > SEGMENT_EPSILON)
            .collect::<Vec<_>>();

        if cleaned.is_empty() {
            return None;
        }

        if cleaned.len() % 2 == 1 {
            let duplicated = cleaned.clone();
            cleaned.extend(duplicated);
        }

        Some(Self { pattern: cleaned, offset })
    }

    pub fn scaled(&self, factor: f32) -> Self {
        Self {
            pattern: self.pattern.iter().map(|entry| entry * factor).collect(),
            offset: self.offset * factor,
        }
    }

    fn total_length(&self) -> f32 {
        self.pattern.iter().sum()
    }
}

#[derive(Debug, Clone)]
struct DashCursor {
    pattern: Vec<f32>,
    index: usize,
    remaining_length: f32,
    draw: bool,
    just_switched_to_gap: bool,
}

impl DashCursor {
    fn new(dash_pattern: &DashPattern) -> Option<Self> {
        let total_length = dash_pattern.total_length();
        if total_length <= SEGMENT_EPSILON {
            return None;
        }

        let mut offset = dash_pattern.offset.rem_euclid(total_length);
        let mut index = 0usize;
        while offset >= dash_pattern.pattern[index] - SEGMENT_EPSILON {
            offset -= dash_pattern.pattern[index];
            index = (index + 1) % dash_pattern.pattern.len();
        }

        let remaining_length = (dash_pattern.pattern[index] - offset).max(SEGMENT_EPSILON);
        Some(Self {
            pattern: dash_pattern.pattern.clone(),
            index,
            remaining_length,
            draw: index % 2 == 0,
            just_switched_to_gap: false,
        })
    }

    fn consume(&mut self, amount: f32) {
        self.just_switched_to_gap = false;
        self.remaining_length -= amount;
        while self.remaining_length <= SEGMENT_EPSILON {
            self.index = (self.index + 1) % self.pattern.len();
            self.draw = self.index % 2 == 0;
            self.just_switched_to_gap = !self.draw;
            self.remaining_length += self.pattern[self.index];
        }
    }

    fn just_switched_to_gap(&self) -> bool {
        self.just_switched_to_gap
    }
}

fn quadratic(start: Vec2, control: Vec2, end: Vec2, t: f32) -> Vec2 {
    let omt = 1.0 - t;
    omt * omt * start + 2.0 * omt * t * control + t * t * end
}

fn cubic(start: Vec2, control_a: Vec2, control_b: Vec2, end: Vec2, t: f32) -> Vec2 {
    let omt = 1.0 - t;
    omt * omt * omt * start
        + 3.0 * omt * omt * t * control_a
        + 3.0 * omt * t * t * control_b
        + t * t * t * end
}

fn quadratic_subdivisions(start: Vec2, control: Vec2, end: Vec2) -> usize {
    let control_length = start.distance(control) + control.distance(end);
    ((control_length / LINEAR_SEGMENT_LENGTH_STEP_MM).ceil() as usize)
        .clamp(MIN_QUADRATIC_SUBDIVISIONS, MAX_QUADRATIC_SUBDIVISIONS)
}

fn cubic_subdivisions(start: Vec2, control_a: Vec2, control_b: Vec2, end: Vec2) -> usize {
    let control_length =
        start.distance(control_a) + control_a.distance(control_b) + control_b.distance(end);
    ((control_length / LINEAR_SEGMENT_LENGTH_STEP_MM).ceil() as usize)
        .clamp(MIN_CUBIC_SUBDIVISIONS, MAX_CUBIC_SUBDIVISIONS)
}

fn arc_length_subdivisions(segment: &Segment) -> usize {
    let preview_steps = match segment {
        Segment::Line(_) => 1,
        Segment::Quadratic(segment) => {
            quadratic_subdivisions(segment.start, segment.control, segment.end)
        }
        Segment::Cubic(segment) => {
            cubic_subdivisions(segment.start, segment.control_a, segment.control_b, segment.end)
        }
    };
    (preview_steps * CURVE_ARC_LENGTH_SUBDIVISION_MULTIPLIER)
        .clamp(MIN_CURVE_ARC_LENGTH_SUBDIVISIONS, MAX_CURVE_ARC_LENGTH_SUBDIVISIONS)
}

fn polyline_length(points: &[Vec2]) -> f32 {
    points.windows(2).map(|segment| segment[0].distance(segment[1])).sum()
}

pub fn simplify_polyline(polyline: &[Vec2]) -> Vec<Vec2> {
    if polyline.len() <= 2 {
        return polyline.to_vec();
    }

    let mut simplified = Vec::with_capacity(polyline.len());
    simplified.push(polyline[0]);

    for point in polyline.iter().copied().skip(1) {
        simplified.push(point);

        while simplified.len() >= 3 {
            let len = simplified.len();
            let a = simplified[len - 3];
            let b = simplified[len - 2];
            let c = simplified[len - 1];
            if is_mergeable_collinear_triplet(a, b, c) {
                simplified.remove(len - 2);
            } else {
                break;
            }
        }
    }

    simplified
}

fn is_mergeable_collinear_triplet(a: Vec2, b: Vec2, c: Vec2) -> bool {
    let ab = b - a;
    let bc = c - b;
    let ac = c - a;

    if ab.length_squared() <= 1e-6 || bc.length_squared() <= 1e-6 || ac.length_squared() <= 1e-6 {
        return true;
    }

    let ab_dir = ab.normalize();
    let bc_dir = bc.normalize();
    let direction_match = ab_dir.dot(bc_dir) >= 0.9995;
    let deviation_sq = point_to_segment_distance_sq(b, a, c);
    direction_match && deviation_sq <= 0.05_f32.powi(2)
}

fn point_to_segment_distance_sq(point: Vec2, start: Vec2, end: Vec2) -> f32 {
    let segment = end - start;
    let length_sq = segment.length_squared();
    if length_sq <= 1e-6 {
        return point.distance_squared(start);
    }

    let t = ((point - start).dot(segment) / length_sq).clamp(0.0, 1.0);
    let projected = start + segment * t;
    point.distance_squared(projected)
}

#[cfg(test)]
pub fn flatten_stroke_to_polyline(stroke: &Stroke) -> Vec<Vec2> {
    stroke.flatten_points()
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    #[test]
    fn dashes_line_stroke_into_visible_substrokes() {
        let stroke = Stroke::new(vec![Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0))]);
        let pattern = DashPattern::new(&[3.0, 2.0], 0.0).unwrap();

        let dashed = stroke.apply_dash_pattern(&pattern);

        assert_eq!(dashed.len(), 2);
        assert_eq!(dashed[0].start_point(), Some(vec2(0.0, 0.0)));
        assert_eq!(dashed[0].end_point(), Some(vec2(3.0, 0.0)));
        assert_eq!(dashed[1].start_point(), Some(vec2(5.0, 0.0)));
        assert_eq!(dashed[1].end_point(), Some(vec2(8.0, 0.0)));
    }

    #[test]
    fn merges_short_segments_into_neighbors() {
        let stroke = Stroke::new(vec![
            Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0)),
            Segment::line(vec2(10.0, 0.0), vec2(10.2, 0.0)),
            Segment::line(vec2(10.2, 0.0), vec2(20.0, 0.0)),
        ]);

        let merged = stroke.merge_short_segments(0.5, 0.5).unwrap();

        assert_eq!(merged.segments.len(), 2);
        assert_eq!(merged.segments[0].end_point(), vec2(10.2, 0.0));
        assert_eq!(merged.segments[1].start_point(), vec2(10.2, 0.0));
    }

    #[test]
    fn removes_stroke_when_merged_length_is_too_short() {
        let stroke = Stroke::new(vec![
            Segment::line(vec2(0.0, 0.0), vec2(0.1, 0.0)),
            Segment::line(vec2(0.1, 0.0), vec2(0.3, 0.0)),
        ]);

        assert!(stroke.merge_short_segments(0.5, 0.5).is_none());
    }

    #[test]
    fn appends_stroke_when_gap_is_within_threshold() {
        let mut stroke = Stroke::new(vec![Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0))]);
        let next = Stroke::new(vec![Segment::line(vec2(10.2, 0.0), vec2(20.0, 0.0))]);

        assert!(stroke.append_if_gap_within(next, 0.5));
        assert_eq!(stroke.segments.len(), 2);
        assert_eq!(stroke.segments[1].start_point(), vec2(10.0, 0.0));
    }

    #[test]
    fn keeps_strokes_separate_when_gap_exceeds_threshold() {
        let mut stroke = Stroke::new(vec![Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0))]);
        let next = Stroke::new(vec![Segment::line(vec2(11.0, 0.0), vec2(20.0, 0.0))]);

        assert!(!stroke.append_if_gap_within(next, 0.5));
        assert_eq!(stroke.segments.len(), 1);
    }
}
