use std::f32::consts::TAU;

use glam::{Vec2, vec2, vec3};

use crate::{
    paths::{CubicBezierSegment, FillRegion, FillRule, PreparedSvg, Segment, Stroke},
    plot::model::{
        CurveOutputMode, FillPattern, MotionKind, MotionSegment, PrintStartMode, ToolSettings,
        ToolpathPlan, ToolpathStats,
    },
    res::lang::Language,
};

const ARC_PREVIEW_SEGMENT_LENGTH_MM: f32 = 2.0;
const MIN_ARC_PREVIEW_SUBDIVISIONS: usize = 4;
const MAX_ARC_PREVIEW_SUBDIVISIONS: usize = 96;
const CORNER_EPSILON: f32 = 1e-4;
const MAX_SMOOTHABLE_CORNER_TURN_DEG: f32 = 170.0;
const ARC_RADIUS_TOLERANCE_MM: f32 = 0.05;
const ARC_RADIUS_TOLERANCE_RATIO: f32 = 0.005;
const ARC_DETECTION_TANGENT_MIN_DOT: f32 = 0.98;
const MIN_DETECTABLE_ARC_SWEEP_RAD: f32 = 0.05;
const MIN_FILL_SPACING_MM: f32 = 0.6;
const MAX_FILL_SPACING_MM: f32 = 8.0;

#[derive(Debug, Clone, Copy)]
enum DrawPrimitive {
    Path(Segment),
    Arc(ArcSegment),
}

impl DrawPrimitive {
    fn start_point(self) -> Vec2 {
        match self {
            Self::Path(segment) => segment.start_point(),
            Self::Arc(segment) => segment.start,
        }
    }

    fn end_point(self) -> Vec2 {
        match self {
            Self::Path(segment) => segment.end_point(),
            Self::Arc(segment) => segment.end,
        }
    }

    fn approximate_length(self) -> f32 {
        match self {
            Self::Path(segment) => segment.approximate_length(),
            Self::Arc(segment) => segment.approximate_length(),
        }
    }

    fn flatten_points(self) -> Vec<Vec2> {
        match self {
            Self::Path(segment) => segment.flatten_points(),
            Self::Arc(segment) => segment.flatten_points(),
        }
    }

    fn to_cubic_bezier(self) -> Option<CubicBezierSegment> {
        match self {
            Self::Path(segment) => segment.to_cubic_bezier(),
            Self::Arc(_) => None,
        }
    }

    fn detected_arc(self) -> Option<ArcSegment> {
        match self {
            Self::Arc(segment) => Some(segment),
            Self::Path(_) => None,
        }
    }

    fn slice_by_arc_length(
        self,
        start_length: f32,
        end_length: f32,
        total_length: f32,
    ) -> Option<Self> {
        match self {
            Self::Path(segment) => {
                segment.slice_by_arc_length(start_length, end_length, total_length).map(Self::Path)
            }
            Self::Arc(segment) => {
                segment.slice_by_arc_length(start_length, end_length, total_length).map(Self::Arc)
            }
        }
    }

    fn point_and_tangent_at_arc_length(
        self,
        target_length: f32,
        total_length: f32,
    ) -> (Vec2, Vec2) {
        match self {
            Self::Path(segment) => {
                segment.point_and_tangent_at_arc_length(target_length, total_length)
            }
            Self::Arc(segment) => {
                segment.point_and_tangent_at_arc_length(target_length, total_length)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ArcSegment {
    start: Vec2,
    end: Vec2,
    center: Vec2,
    clockwise: bool,
}

impl ArcSegment {
    fn radius(self) -> f32 {
        self.start.distance(self.center)
    }

    fn signed_sweep_radians(self) -> f32 {
        signed_sweep_between(self.start - self.center, self.end - self.center, self.clockwise)
    }

    fn approximate_length(self) -> f32 {
        self.radius() * self.signed_sweep_radians().abs()
    }

    fn flatten_points(self) -> Vec<Vec2> {
        let radius = self.radius();
        let sweep = self.signed_sweep_radians();
        if radius <= CORNER_EPSILON || sweep.abs() <= CORNER_EPSILON {
            return vec![self.start, self.end];
        }

        let steps = ((self.approximate_length() / ARC_PREVIEW_SEGMENT_LENGTH_MM).ceil() as usize)
            .clamp(MIN_ARC_PREVIEW_SUBDIVISIONS, MAX_ARC_PREVIEW_SUBDIVISIONS);
        let mut points = Vec::with_capacity(steps + 1);
        points.push(self.start);

        for step in 1..=steps {
            let fraction = step as f32 / steps as f32;
            points.push(self.point_and_tangent_at_fraction(fraction).0);
        }

        if let Some(last) = points.last_mut() {
            *last = self.end;
        }

        points
    }

    fn point_and_tangent_at_arc_length(
        self,
        target_length: f32,
        total_length: f32,
    ) -> (Vec2, Vec2) {
        if total_length <= CORNER_EPSILON {
            return (self.start, expected_arc_tangent(self.clockwise, self.start, self.center));
        }

        let fraction = (target_length / total_length).clamp(0.0, 1.0);
        self.point_and_tangent_at_fraction(fraction)
    }

    fn slice_by_arc_length(
        self,
        start_length: f32,
        end_length: f32,
        total_length: f32,
    ) -> Option<Self> {
        if total_length <= CORNER_EPSILON {
            return None;
        }

        let clamped_start = start_length.clamp(0.0, total_length);
        let clamped_end = end_length.clamp(clamped_start, total_length);
        if clamped_end - clamped_start <= CORNER_EPSILON {
            return None;
        }

        if clamped_start <= CORNER_EPSILON && total_length - clamped_end <= CORNER_EPSILON {
            return Some(self);
        }

        let start = self.point_and_tangent_at_arc_length(clamped_start, total_length).0;
        let end = self.point_and_tangent_at_arc_length(clamped_end, total_length).0;
        Some(Self { start, end, center: self.center, clockwise: self.clockwise })
    }

    fn point_and_tangent_at_fraction(self, fraction: f32) -> (Vec2, Vec2) {
        let radius = self.radius();
        let sweep = self.signed_sweep_radians();
        let start_angle = (self.start.y - self.center.y).atan2(self.start.x - self.center.x);
        let angle = start_angle + sweep * fraction.clamp(0.0, 1.0);
        let point = self.center + vec2(angle.cos(), angle.sin()) * radius;
        (point, expected_arc_tangent(self.clockwise, point, self.center))
    }
}

#[allow(dead_code)]
pub fn build_plan(prepared: PreparedSvg, settings: &ToolSettings) -> ToolpathPlan {
    build_plan_with_language(prepared, settings, Language::default())
}

pub fn build_plan_with_language(
    prepared: PreparedSvg,
    settings: &ToolSettings,
    language: Language,
) -> ToolpathPlan {
    let text = language.strings();
    let PreparedSvg {
        source_name,
        strokes,
        fill_regions,
        warnings,
        drawing_origin,
        drawing_bounds,
        is_out_of_bounds,
    } = prepared;

    let mut drawable_strokes = Vec::new();
    if settings.fill_enabled {
        for region in &fill_regions {
            drawable_strokes.extend(generate_fill_strokes(region, settings));
        }
    }
    drawable_strokes.extend(strokes);

    let stroke_primitives = drawable_strokes
        .iter()
        .map(|stroke| build_draw_primitives(stroke, settings))
        .filter(|primitives| !primitives.is_empty())
        .collect::<Vec<_>>();

    let mut segments = Vec::new();
    let mut segment_end_times_s = Vec::new();
    let mut gcode_lines = Vec::new();

    let draw_feed = settings.print_feed_rate();
    let travel_feed = settings.travel_feed_rate();
    let draw_speed = (draw_feed / 60.0).max(1.0);
    let travel_speed = (travel_feed / 60.0).max(1.0);
    let lift = settings.lift_height_mm;
    let first_draw_point = stroke_primitives
        .iter()
        .find_map(|primitives| primitives.first().map(|primitive| primitive.start_point()));
    let has_arc_primitives = stroke_primitives
        .iter()
        .flat_map(|primitives| primitives.iter())
        .any(|primitive| primitive.detected_arc().is_some());
    let has_g5_primitives =
        stroke_primitives.iter().flat_map(|primitives| primitives.iter()).any(|primitive| {
            primitive.to_cubic_bezier().is_some()
                && (!settings.curve_output_mode.prefers_g2g3()
                    || primitive.detected_arc().is_none())
        });

    gcode_lines.push("; Generated by Penartic".to_owned());
    gcode_lines.push(format!("; Source: {}", source_name));
    gcode_lines.push("G21".to_owned());
    gcode_lines.push("G90".to_owned());

    let mut current = match settings.print_start_mode {
        PrintStartMode::HomeBeforePrint => {
            let mut current = vec3(0.0, 0.0, 0.0);
            let lifted_origin = vec3(0.0, 0.0, lift);
            push_relative_z_motion_if_needed(
                &mut segments,
                &mut segment_end_times_s,
                &mut gcode_lines,
                &mut current,
                lifted_origin,
                travel_speed,
                travel_feed,
            );

            gcode_lines.push("G28 X Y".to_owned());
            lifted_origin
        }
        PrintStartMode::DirectFromCurrentPosition => vec3(0.0, 0.0, 0.0),
    };

    let stroke_count = stroke_primitives.len();
    let mut active_feed_rate = None;
    for primitives in &stroke_primitives {
        let start_draw = primitives
            .first()
            .map(|primitive| primitive.start_point())
            .map(|point| vec3(point.x, point.y, 0.0))
            .unwrap();
        push_position_to_stroke_start(
            &mut segments,
            &mut segment_end_times_s,
            &mut gcode_lines,
            &mut current,
            start_draw,
            lift,
            travel_speed,
            travel_feed,
            &mut active_feed_rate,
        );

        for primitive in primitives {
            append_draw_primitive(
                &mut segments,
                &mut segment_end_times_s,
                &mut gcode_lines,
                &mut current,
                *primitive,
                settings.curve_output_mode,
                draw_speed,
                draw_feed,
                &mut active_feed_rate,
            );
        }

        let raised = vec3(current.x, current.y, lift);
        push_relative_z_motion_if_needed(
            &mut segments,
            &mut segment_end_times_s,
            &mut gcode_lines,
            &mut current,
            raised,
            travel_speed,
            travel_feed,
        );
        active_feed_rate = None;
    }

    gcode_lines.push("M400".to_owned());

    let mut stats =
        ToolpathStats { stroke_count, segment_count: segments.len(), ..Default::default() };

    for segment in &segments {
        match segment.kind {
            MotionKind::Travel => {
                stats.travel_distance_mm += segment.length();
                stats.estimated_duration_s += segment.duration_s;
            }
            MotionKind::Draw => {
                stats.drawing_distance_mm += segment.length();
                stats.estimated_duration_s += segment.duration_s;
            }
        }
    }

    let mut warnings = warnings;
    if settings.curve_output_mode.prefers_g2g3() && has_arc_primitives {
        warnings.push(text.g2g3_firmware_warning.to_owned());
    }
    if settings.curve_output_mode.prefers_g5() && has_g5_primitives {
        warnings.push(text.g5_firmware_warning.to_owned());
    }

    ToolpathPlan {
        source_name,
        printable_area: settings.printable_area,
        drawing_origin,
        drawing_bounds,
        first_draw_point,
        is_out_of_bounds,
        segments,
        segment_end_times_s,
        gcode_lines,
        warnings,
        stats,
    }
}

fn generate_fill_strokes(region: &FillRegion, settings: &ToolSettings) -> Vec<Stroke> {
    if region.is_empty() || settings.fill_density_percent <= 0.0 {
        return Vec::new();
    }

    match settings.fill_pattern {
        FillPattern::Lines => {
            fill_hatch_strokes(region, settings.fill_angle_degrees, settings, false)
        }
        FillPattern::Crosshatch => {
            let mut strokes =
                fill_hatch_strokes(region, settings.fill_angle_degrees, settings, false);
            strokes.extend(fill_hatch_strokes(
                region,
                settings.fill_angle_degrees + 90.0,
                settings,
                false,
            ));
            strokes
        }
        FillPattern::Zigzag => {
            fill_hatch_strokes(region, settings.fill_angle_degrees, settings, true)
        }
    }
}

fn fill_hatch_strokes(
    region: &FillRegion,
    angle_degrees: f32,
    settings: &ToolSettings,
    alternate_direction: bool,
) -> Vec<Stroke> {
    let contours = region
        .contours
        .iter()
        .map(|stroke| closed_polyline(stroke.flatten_points()))
        .filter(|points| points.len() >= 4)
        .collect::<Vec<_>>();
    if contours.is_empty() {
        return Vec::new();
    }

    let angle = angle_degrees.to_radians();
    let direction = vec2(angle.cos(), angle.sin()).normalize_or_zero();
    let normal = vec2(-direction.y, direction.x);
    let spacing = fill_spacing_mm(settings.fill_density_percent);
    let mut min_offset = f32::INFINITY;
    let mut max_offset = f32::NEG_INFINITY;

    for point in contours.iter().flatten() {
        let offset = point.dot(normal);
        min_offset = min_offset.min(offset);
        max_offset = max_offset.max(offset);
    }

    if !min_offset.is_finite() || !max_offset.is_finite() {
        return Vec::new();
    }

    let mut strokes = Vec::new();
    let mut row = 0usize;
    let mut offset = (min_offset / spacing).floor() * spacing;
    while offset <= max_offset + spacing * 0.5 {
        let mut intervals =
            fill_intervals_at_offset(&contours, region.rule, direction, normal, offset);
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        if alternate_direction && row % 2 == 1 {
            intervals.reverse();
        }

        for (a, b) in intervals {
            if b - a <= CORNER_EPSILON {
                continue;
            }
            let (start_projection, end_projection) =
                if alternate_direction && row % 2 == 1 { (b, a) } else { (a, b) };
            let start = direction * start_projection + normal * offset;
            let end = direction * end_projection + normal * offset;
            strokes.push(Stroke::new(vec![Segment::line(start, end)]));
        }

        row += 1;
        offset += spacing;
    }

    strokes
}

fn fill_spacing_mm(density_percent: f32) -> f32 {
    let density = (density_percent.clamp(1.0, 100.0) - 1.0) / 99.0;
    MAX_FILL_SPACING_MM + (MIN_FILL_SPACING_MM - MAX_FILL_SPACING_MM) * density
}

fn closed_polyline(mut points: Vec<Vec2>) -> Vec<Vec2> {
    if points.len() >= 2 && !points_match(points[0], *points.last().unwrap()) {
        points.push(points[0]);
    }
    points
}

fn fill_intervals_at_offset(
    contours: &[Vec<Vec2>],
    rule: FillRule,
    direction: Vec2,
    normal: Vec2,
    offset: f32,
) -> Vec<(f32, f32)> {
    let mut events = Vec::new();
    for contour in contours {
        for edge in contour.windows(2) {
            let a_offset = edge[0].dot(normal) - offset;
            let b_offset = edge[1].dot(normal) - offset;
            if (a_offset <= 0.0 && b_offset <= 0.0) || (a_offset > 0.0 && b_offset > 0.0) {
                continue;
            }
            let denominator = a_offset - b_offset;
            if denominator.abs() <= CORNER_EPSILON {
                continue;
            }
            let t = (a_offset / denominator).clamp(0.0, 1.0);
            let point = edge[0].lerp(edge[1], t);
            let winding_delta = if b_offset > a_offset { 1 } else { -1 };
            events.push((point.dot(direction), winding_delta));
        }
    }

    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut intervals = Vec::new();
    let mut winding = 0i32;
    let mut active_start = None;

    for (projection, delta) in events {
        let was_inside = fill_rule_inside(rule, winding);
        winding += delta;
        let is_inside = fill_rule_inside(rule, winding);
        match (was_inside, is_inside, active_start) {
            (false, true, None) => active_start = Some(projection),
            (true, false, Some(start)) if projection > start + CORNER_EPSILON => {
                intervals.push((start, projection));
                active_start = None;
            }
            (true, false, _) => active_start = None,
            _ => {}
        }
    }

    intervals
}

fn fill_rule_inside(rule: FillRule, winding: i32) -> bool {
    match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding.rem_euclid(2) != 0,
    }
}

fn build_draw_primitives(stroke: &Stroke, settings: &ToolSettings) -> Vec<DrawPrimitive> {
    let base_primitives = stroke
        .segments
        .iter()
        .copied()
        .map(DrawPrimitive::Path)
        .filter(|primitive| {
            primitive_is_finite(*primitive) && primitive.approximate_length() > CORNER_EPSILON
        })
        .collect::<Vec<_>>();

    apply_corner_smoothing(base_primitives, settings)
}

fn apply_corner_smoothing(
    base_primitives: Vec<DrawPrimitive>,
    settings: &ToolSettings,
) -> Vec<DrawPrimitive> {
    if base_primitives.len() < 2
        || !settings.corner_smoothing_enabled
        || settings.corner_smoothing_radius_mm <= CORNER_EPSILON
    {
        return base_primitives;
    }

    let closed = base_primitives.len() > 1
        && points_match(
            base_primitives[0].start_point(),
            base_primitives.last().unwrap().end_point(),
        );
    let join_count =
        if closed { base_primitives.len() } else { base_primitives.len().saturating_sub(1) };
    if join_count == 0 {
        return base_primitives;
    }

    let lengths =
        base_primitives.iter().map(|primitive| primitive.approximate_length()).collect::<Vec<_>>();
    let mut join_trims = (0..join_count)
        .map(|index| {
            let next_index = if index + 1 < base_primitives.len() { index + 1 } else { 0 };
            desired_trim_for_join(base_primitives[index], base_primitives[next_index], settings)
        })
        .collect::<Vec<_>>();

    limit_join_trims(&lengths, closed, &mut join_trims);

    if join_trims.iter().all(|trim| *trim <= CORNER_EPSILON) {
        return base_primitives;
    }

    let mut result = Vec::new();

    for index in 0..base_primitives.len() {
        let start_trim = primitive_start_trim(index, base_primitives.len(), closed, &join_trims);
        let end_trim = primitive_end_trim(index, base_primitives.len(), closed, &join_trims);
        let primitive_length = lengths[index];

        if let Some(trimmed) = base_primitives[index].slice_by_arc_length(
            start_trim,
            primitive_length - end_trim,
            primitive_length,
        ) {
            result.push(trimmed);
        }

        let has_next = index + 1 < base_primitives.len();
        if has_next || closed {
            let next_index = if has_next { index + 1 } else { 0 };
            if let Some(transition) = build_transition_primitive(
                base_primitives[index],
                lengths[index],
                base_primitives[next_index],
                lengths[next_index],
                join_trims[index],
            ) {
                result.push(transition);
            }
        }
    }

    result.into_iter().filter(|primitive| primitive.approximate_length() > CORNER_EPSILON).collect()
}

fn desired_trim_for_join(
    left: DrawPrimitive,
    right: DrawPrimitive,
    settings: &ToolSettings,
) -> f32 {
    let left_length = left.approximate_length();
    let right_length = right.approximate_length();
    if left_length <= CORNER_EPSILON || right_length <= CORNER_EPSILON {
        return 0.0;
    }

    let (_, left_tangent) = left.point_and_tangent_at_arc_length(left_length, left_length);
    let (_, right_tangent) = right.point_and_tangent_at_arc_length(0.0, right_length);
    let turn = left_tangent.dot(right_tangent).clamp(-1.0, 1.0).acos();
    if turn < settings.corner_smoothing_angle_deg.to_radians()
        || turn > MAX_SMOOTHABLE_CORNER_TURN_DEG.to_radians()
        || cross_2d(left_tangent, right_tangent).abs() <= CORNER_EPSILON
    {
        return 0.0;
    }

    let tan_half_turn = (turn * 0.5).tan();
    if !tan_half_turn.is_finite() || tan_half_turn <= CORNER_EPSILON {
        return 0.0;
    }

    settings.corner_smoothing_radius_mm * tan_half_turn
}

fn limit_join_trims(lengths: &[f32], closed: bool, join_trims: &mut [f32]) {
    if lengths.is_empty() || join_trims.is_empty() {
        return;
    }

    for _ in 0..lengths.len().saturating_mul(4).max(1) {
        let mut changed = false;

        for index in 0..lengths.len() {
            let start_join = if closed {
                Some((index + lengths.len() - 1) % lengths.len())
            } else if index > 0 {
                Some(index - 1)
            } else {
                None
            };
            let end_join = if closed {
                Some(index)
            } else if index + 1 < lengths.len() {
                Some(index)
            } else {
                None
            };

            let start_trim = start_join.map(|join| join_trims[join]).unwrap_or(0.0);
            let end_trim = end_join.map(|join| join_trims[join]).unwrap_or(0.0);
            let total_trim = start_trim + end_trim;
            let available_length = (lengths[index] - CORNER_EPSILON).max(0.0);
            if total_trim <= available_length || total_trim <= CORNER_EPSILON {
                continue;
            }

            let scale = available_length / total_trim;
            if let Some(join) = start_join {
                join_trims[join] *= scale;
            }
            if let Some(join) = end_join {
                join_trims[join] *= scale;
            }
            changed = true;
        }

        if !changed {
            break;
        }
    }
}

fn primitive_start_trim(index: usize, count: usize, closed: bool, join_trims: &[f32]) -> f32 {
    if closed {
        join_trims[(index + count - 1) % count]
    } else if index > 0 {
        join_trims[index - 1]
    } else {
        0.0
    }
}

fn primitive_end_trim(index: usize, count: usize, closed: bool, join_trims: &[f32]) -> f32 {
    if closed {
        join_trims[index]
    } else if index + 1 < count {
        join_trims[index]
    } else {
        0.0
    }
}

fn build_transition_primitive(
    left: DrawPrimitive,
    left_length: f32,
    right: DrawPrimitive,
    right_length: f32,
    trim: f32,
) -> Option<DrawPrimitive> {
    if trim <= CORNER_EPSILON || left_length <= CORNER_EPSILON || right_length <= CORNER_EPSILON {
        return None;
    }

    let (start_point, start_tangent) =
        left.point_and_tangent_at_arc_length(left_length - trim, left_length);
    let (end_point, end_tangent) = right.point_and_tangent_at_arc_length(trim, right_length);
    if let Some(arc) =
        build_tangent_rounding_arc(start_point, start_tangent, end_point, end_tangent)
    {
        return Some(DrawPrimitive::Arc(arc));
    }

    build_tangent_cubic_transition(start_point, start_tangent, end_point, end_tangent)
        .map(DrawPrimitive::Path)
}

fn build_tangent_rounding_arc(
    start_point: Vec2,
    start_tangent: Vec2,
    end_point: Vec2,
    end_tangent: Vec2,
) -> Option<ArcSegment> {
    if points_match(start_point, end_point) {
        return None;
    }

    let turn_cross = cross_2d(start_tangent, end_tangent);
    if turn_cross.abs() <= CORNER_EPSILON {
        return None;
    }

    let start_normal =
        if turn_cross > 0.0 { left_normal(start_tangent) } else { right_normal(start_tangent) };
    let end_normal =
        if turn_cross > 0.0 { left_normal(end_tangent) } else { right_normal(end_tangent) };
    let center = line_intersection(start_point, start_normal, end_point, end_normal)?;
    let radius = center.distance(start_point);
    if !radius.is_finite() || radius <= CORNER_EPSILON {
        return None;
    }

    let tolerance = arc_radius_tolerance(radius);
    if (center.distance(end_point) - radius).abs() > tolerance {
        return None;
    }

    let arc =
        ArcSegment { start: start_point, end: end_point, center, clockwise: turn_cross < 0.0 };
    if arc.signed_sweep_radians().abs() <= MIN_DETECTABLE_ARC_SWEEP_RAD {
        return None;
    }

    let expected_start_tangent = expected_arc_tangent(arc.clockwise, arc.start, arc.center);
    let expected_end_tangent = expected_arc_tangent(arc.clockwise, arc.end, arc.center);
    if start_tangent.dot(expected_start_tangent) < ARC_DETECTION_TANGENT_MIN_DOT
        || end_tangent.dot(expected_end_tangent) < ARC_DETECTION_TANGENT_MIN_DOT
    {
        return None;
    }

    Some(arc)
}

fn arc_radius_tolerance(radius: f32) -> f32 {
    ARC_RADIUS_TOLERANCE_MM.max(radius * ARC_RADIUS_TOLERANCE_RATIO)
}

fn build_tangent_cubic_transition(
    start_point: Vec2,
    start_tangent: Vec2,
    end_point: Vec2,
    end_tangent: Vec2,
) -> Option<Segment> {
    let chord = end_point - start_point;
    let chord_length = chord.length();
    if chord_length <= CORNER_EPSILON {
        return None;
    }

    let control_length = (chord_length / 3.0).max(CORNER_EPSILON);
    let control_a = start_point + start_tangent * control_length;
    let control_b = end_point - end_tangent * control_length;
    Some(Segment::cubic(start_point, control_a, control_b, end_point))
}

fn signed_sweep_between(start_vector: Vec2, end_vector: Vec2, clockwise: bool) -> f32 {
    let sweep = cross_2d(start_vector, end_vector).atan2(start_vector.dot(end_vector));
    if clockwise {
        if sweep > 0.0 { sweep - TAU } else { sweep }
    } else if sweep < 0.0 {
        sweep + TAU
    } else {
        sweep
    }
}

fn expected_arc_tangent(clockwise: bool, point: Vec2, center: Vec2) -> Vec2 {
    let radius_direction = (point - center).normalize_or_zero();
    if clockwise { right_normal(radius_direction) } else { left_normal(radius_direction) }
}

fn line_intersection(
    point_a: Vec2,
    direction_a: Vec2,
    point_b: Vec2,
    direction_b: Vec2,
) -> Option<Vec2> {
    let denominator = cross_2d(direction_a, direction_b);
    if denominator.abs() <= CORNER_EPSILON {
        return None;
    }

    let t = cross_2d(point_b - point_a, direction_b) / denominator;
    Some(point_a + direction_a * t)
}

fn points_match(a: Vec2, b: Vec2) -> bool {
    a.distance_squared(b) <= CORNER_EPSILON * CORNER_EPSILON
}

fn cross_2d(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

fn left_normal(direction: Vec2) -> Vec2 {
    vec2(-direction.y, direction.x)
}

fn right_normal(direction: Vec2) -> Vec2 {
    vec2(direction.y, -direction.x)
}

fn append_draw_primitive(
    segments: &mut Vec<MotionSegment>,
    segment_end_times_s: &mut Vec<f32>,
    gcode_lines: &mut Vec<String>,
    current: &mut glam::Vec3,
    primitive: DrawPrimitive,
    curve_output_mode: CurveOutputMode,
    draw_speed: f32,
    draw_feed: f32,
    active_feed_rate: &mut Option<i32>,
) {
    let points = primitive.flatten_points();
    for window in points.windows(2) {
        let next = vec3(window[1].x, window[1].y, 0.0);
        if current.distance_squared(next) <= 1e-6 {
            continue;
        }
        push_segment(segments, segment_end_times_s, *current, next, MotionKind::Draw, draw_speed);
        *current = next;
    }

    if curve_output_mode.prefers_g2g3() {
        if let Some(arc) = primitive.detected_arc() {
            push_g2g3_move(gcode_lines, active_feed_rate, draw_feed, arc);
            return;
        }
    }

    if curve_output_mode.prefers_g5() {
        if let Some(cubic) = primitive.to_cubic_bezier() {
            push_g5_move(gcode_lines, active_feed_rate, draw_feed, cubic);
            return;
        }
    }

    for point in points.into_iter().skip(1) {
        push_g1_move(gcode_lines, active_feed_rate, draw_feed, Some(point.x), Some(point.y), None);
    }
}

fn push_position_to_stroke_start(
    segments: &mut Vec<MotionSegment>,
    segment_end_times_s: &mut Vec<f32>,
    gcode_lines: &mut Vec<String>,
    current: &mut glam::Vec3,
    start_draw: glam::Vec3,
    lift: f32,
    travel_speed: f32,
    travel_feed: f32,
    active_feed_rate: &mut Option<i32>,
) {
    let lifted_current = vec3(current.x, current.y, lift);
    push_relative_z_motion_if_needed(
        segments,
        segment_end_times_s,
        gcode_lines,
        current,
        lifted_current,
        travel_speed,
        travel_feed,
    );

    let start_lifted = vec3(start_draw.x, start_draw.y, lift);
    if current.distance_squared(start_lifted) > 1e-6 {
        push_segment(
            segments,
            segment_end_times_s,
            *current,
            start_lifted,
            MotionKind::Travel,
            travel_speed,
        );
        push_g1_move(
            gcode_lines,
            active_feed_rate,
            travel_feed,
            Some(start_lifted.x),
            Some(start_lifted.y),
            None,
        );
        *current = start_lifted;
    }

    push_relative_z_motion_if_needed(
        segments,
        segment_end_times_s,
        gcode_lines,
        current,
        start_draw,
        travel_speed,
        travel_feed,
    );
    *active_feed_rate = None;
}

fn push_relative_z_motion_if_needed(
    segments: &mut Vec<MotionSegment>,
    segment_end_times_s: &mut Vec<f32>,
    gcode_lines: &mut Vec<String>,
    current: &mut glam::Vec3,
    end: glam::Vec3,
    speed_mm_s: f32,
    feed_rate_mm_min: f32,
) {
    if !current.is_finite() {
        if end.is_finite() {
            *current = end;
        }
        return;
    }
    if !end.is_finite() {
        return;
    }

    if current.distance_squared(end) <= 1e-6 {
        return;
    }

    if (current.x - end.x).abs() > 1e-6 || (current.y - end.y).abs() > 1e-6 {
        push_segment(segments, segment_end_times_s, *current, end, MotionKind::Travel, speed_mm_s);
        gcode_lines.push(format!(
            "G1 X{:.2} Y{:.2} Z{:.3} F{:.0}",
            end.x,
            end.y,
            end.z,
            feed_rate_mm_min.round()
        ));
        *current = end;
        return;
    }

    push_segment(segments, segment_end_times_s, *current, end, MotionKind::Travel, speed_mm_s);
    gcode_lines.push("G91".to_owned());
    gcode_lines.push(format!("G1 Z{:.3} F{:.0}", end.z - current.z, feed_rate_mm_min.round()));
    gcode_lines.push("G90".to_owned());
    *current = end;
}

fn push_g1_move(
    gcode_lines: &mut Vec<String>,
    active_feed_rate: &mut Option<i32>,
    feed_rate_mm_min: f32,
    x: Option<f32>,
    y: Option<f32>,
    z: Option<f32>,
) {
    let mut line = "G1".to_owned();

    if let Some(x) = x {
        line.push_str(&format!(" X{x:.2}"));
    }
    if let Some(y) = y {
        line.push_str(&format!(" Y{y:.2}"));
    }
    if let Some(z) = z {
        line.push_str(&format!(" Z{z:.3}"));
    }

    let rounded_feed_rate = feed_rate_mm_min.round() as i32;
    if active_feed_rate.is_none_or(|active| active != rounded_feed_rate) {
        line.push_str(&format!(" F{rounded_feed_rate}"));
        *active_feed_rate = Some(rounded_feed_rate);
    }

    gcode_lines.push(line);
}

fn primitive_is_finite(primitive: DrawPrimitive) -> bool {
    match primitive {
        DrawPrimitive::Path(segment) => match segment {
            Segment::Line(segment) => segment.start.is_finite() && segment.end.is_finite(),
            Segment::Quadratic(segment) => {
                segment.start.is_finite() && segment.control.is_finite() && segment.end.is_finite()
            }
            Segment::Cubic(segment) => {
                segment.start.is_finite()
                    && segment.control_a.is_finite()
                    && segment.control_b.is_finite()
                    && segment.end.is_finite()
            }
        },
        DrawPrimitive::Arc(segment) => {
            segment.start.is_finite() && segment.end.is_finite() && segment.center.is_finite()
        }
    }
}

fn push_g2g3_move(
    gcode_lines: &mut Vec<String>,
    active_feed_rate: &mut Option<i32>,
    feed_rate_mm_min: f32,
    arc: ArcSegment,
) {
    let command = if arc.clockwise { "G2" } else { "G3" };
    let mut line = format!(
        "{command} X{:.2} Y{:.2} I{:.3} J{:.3}",
        arc.end.x,
        arc.end.y,
        arc.center.x - arc.start.x,
        arc.center.y - arc.start.y,
    );

    let rounded_feed_rate = feed_rate_mm_min.round() as i32;
    if active_feed_rate.is_none_or(|active| active != rounded_feed_rate) {
        line.push_str(&format!(" F{rounded_feed_rate}"));
        *active_feed_rate = Some(rounded_feed_rate);
    }

    gcode_lines.push(line);
}

fn push_g5_move(
    gcode_lines: &mut Vec<String>,
    active_feed_rate: &mut Option<i32>,
    feed_rate_mm_min: f32,
    cubic: CubicBezierSegment,
) {
    let mut line = format!(
        "G5 I{:.3} J{:.3} P{:.3} Q{:.3} X{:.2} Y{:.2}",
        cubic.control_a.x - cubic.start.x,
        cubic.control_a.y - cubic.start.y,
        cubic.control_b.x - cubic.end.x,
        cubic.control_b.y - cubic.end.y,
        cubic.end.x,
        cubic.end.y,
    );

    let rounded_feed_rate = feed_rate_mm_min.round() as i32;
    if active_feed_rate.is_none_or(|active| active != rounded_feed_rate) {
        line.push_str(&format!(" F{rounded_feed_rate}"));
        *active_feed_rate = Some(rounded_feed_rate);
    }

    gcode_lines.push(line);
}

fn push_segment(
    segments: &mut Vec<MotionSegment>,
    segment_end_times_s: &mut Vec<f32>,
    start: glam::Vec3,
    end: glam::Vec3,
    kind: MotionKind,
    speed_mm_s: f32,
) {
    let duration_s = start.distance(end) / speed_mm_s.max(1e-3);
    segments.push(MotionSegment { start, end, kind, duration_s });
    let cumulative_s = segment_end_times_s.last().copied().unwrap_or(0.0) + duration_s;
    segment_end_times_s.push(cumulative_s);
}

#[cfg(test)]
mod tests {
    use glam::vec2;

    use super::*;
    use crate::paths::{FillRegion, FillRule};
    use crate::plot::model::{CurveOutputMode, PrintStartMode, PrintableArea, ToolSettings};

    fn line_prepared_svg() -> PreparedSvg {
        PreparedSvg {
            source_name: "shape.svg".to_owned(),
            strokes: vec![Stroke::new(vec![Segment::line(vec2(10.0, 10.0), vec2(40.0, 10.0))])],
            fill_regions: Vec::new(),
            warnings: Vec::new(),
            drawing_origin: vec2(10.0, 10.0),
            drawing_bounds: vec2(30.0, 0.0),
            is_out_of_bounds: false,
        }
    }

    fn right_angle_prepared_svg() -> PreparedSvg {
        PreparedSvg {
            source_name: "corner.svg".to_owned(),
            strokes: vec![Stroke::new(vec![
                Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0)),
                Segment::line(vec2(10.0, 0.0), vec2(10.0, 10.0)),
            ])],
            fill_regions: Vec::new(),
            warnings: Vec::new(),
            drawing_origin: vec2(0.0, 0.0),
            drawing_bounds: vec2(10.0, 10.0),
            is_out_of_bounds: false,
        }
    }

    #[test]
    fn generated_gcode_lifts_then_homes_xy() {
        let plan = build_plan(
            line_prepared_svg(),
            &ToolSettings {
                printable_area: PrintableArea::new(100.0, 100.0),
                print_speed_mm_s: 25.0,
                lift_height_mm: 2.0,
                print_start_mode: PrintStartMode::HomeBeforePrint,
                curve_output_mode: CurveOutputMode::LinearSegments,
                ..ToolSettings::default()
            },
        );

        assert_eq!(plan.gcode_lines[2], "G21");
        assert_eq!(plan.gcode_lines[3], "G90");
        assert_eq!(plan.gcode_lines[4], "G91");
        assert_eq!(plan.gcode_lines[5], "G1 Z2.000 F3000");
        assert_eq!(plan.gcode_lines[6], "G90");
        assert_eq!(plan.gcode_lines[7], "G28 X Y");
        assert_eq!(plan.gcode_lines[8], "G1 X10.00 Y10.00 F3000");
        assert_eq!(plan.gcode_lines[9], "G91");
        assert_eq!(plan.gcode_lines[10], "G1 Z-2.000 F3000");
        assert_eq!(plan.gcode_lines[11], "G90");
        assert_eq!(plan.gcode_lines[12], "G1 X40.00 Y10.00 F1500");
        assert_eq!(plan.first_draw_point, Some(vec2(10.0, 10.0)));
    }

    #[test]
    fn direct_start_lifts_before_moving_to_first_draw_point() {
        let plan = build_plan(
            line_prepared_svg(),
            &ToolSettings {
                printable_area: PrintableArea::new(100.0, 100.0),
                print_speed_mm_s: 25.0,
                lift_height_mm: 2.0,
                print_start_mode: PrintStartMode::DirectFromCurrentPosition,
                curve_output_mode: CurveOutputMode::LinearSegments,
                ..ToolSettings::default()
            },
        );

        assert_eq!(plan.gcode_lines[2], "G21");
        assert_eq!(plan.gcode_lines[3], "G90");
        assert_eq!(plan.gcode_lines[4], "G91");
        assert_eq!(plan.gcode_lines[5], "G1 Z2.000 F3000");
        assert_eq!(plan.gcode_lines[6], "G90");
        assert_eq!(plan.gcode_lines[7], "G1 X10.00 Y10.00 F3000");
        assert_eq!(plan.gcode_lines[8], "G91");
        assert_eq!(plan.gcode_lines[9], "G1 Z-2.000 F3000");
        assert_eq!(plan.gcode_lines[10], "G90");
        assert_eq!(plan.gcode_lines[11], "G1 X40.00 Y10.00 F1500");
        assert!(!plan.gcode_lines.iter().any(|line| line == "G28 X Y"));
        assert!(matches!(
            plan.segments.first().map(|segment| segment.kind),
            Some(MotionKind::Travel)
        ));
    }

    #[test]
    fn emits_g5_for_curve_segments_when_enabled() {
        let prepared = PreparedSvg {
            source_name: "curve.svg".to_owned(),
            strokes: vec![Stroke::new(vec![Segment::quadratic(
                vec2(0.0, 0.0),
                vec2(5.0, 10.0),
                vec2(10.0, 0.0),
            )])],
            fill_regions: Vec::new(),
            warnings: Vec::new(),
            drawing_origin: vec2(0.0, 0.0),
            drawing_bounds: vec2(10.0, 10.0),
            is_out_of_bounds: false,
        };

        let plan = build_plan(
            prepared,
            &ToolSettings {
                printable_area: PrintableArea::new(100.0, 100.0),
                print_speed_mm_s: 25.0,
                lift_height_mm: 2.0,
                print_start_mode: PrintStartMode::DirectFromCurrentPosition,
                curve_output_mode: CurveOutputMode::PreferG5,
                ..ToolSettings::default()
            },
        );

        assert!(plan.gcode_lines.iter().any(|line| line.starts_with("G5 ")));
        assert!(plan.warnings.iter().any(|warning| warning.contains("G5")));
    }

    #[test]
    fn emits_g3_for_smoothed_right_angle_when_arc_mode_enabled() {
        let plan = build_plan(
            right_angle_prepared_svg(),
            &ToolSettings {
                printable_area: PrintableArea::new(100.0, 100.0),
                print_speed_mm_s: 25.0,
                lift_height_mm: 2.0,
                print_start_mode: PrintStartMode::DirectFromCurrentPosition,
                curve_output_mode: CurveOutputMode::PreferG2G3,
                corner_smoothing_enabled: true,
                corner_smoothing_radius_mm: 1.0,
                corner_smoothing_angle_deg: 45.0,
                ..ToolSettings::default()
            },
        );

        assert!(plan.gcode_lines.iter().any(|line| line.starts_with("G3 ")));
        assert!(!plan.gcode_lines.iter().any(|line| line == "G1 X10.00 Y0.00"));
        assert!(plan.warnings.iter().any(|warning| warning.contains("G2/G3")));
    }

    #[test]
    fn smooths_curve_to_line_join_based_on_endpoint_tangents() {
        let prepared = PreparedSvg {
            source_name: "curve-line.svg".to_owned(),
            strokes: vec![Stroke::new(vec![
                Segment::quadratic(vec2(0.0, 0.0), vec2(10.0, 0.0), vec2(10.0, 10.0)),
                Segment::line(vec2(10.0, 10.0), vec2(20.0, 10.0)),
            ])],
            fill_regions: Vec::new(),
            warnings: Vec::new(),
            drawing_origin: vec2(0.0, 0.0),
            drawing_bounds: vec2(20.0, 10.0),
            is_out_of_bounds: false,
        };

        let unsmoothed = build_plan(
            prepared.clone(),
            &ToolSettings {
                printable_area: PrintableArea::new(100.0, 100.0),
                print_speed_mm_s: 25.0,
                lift_height_mm: 2.0,
                print_start_mode: PrintStartMode::DirectFromCurrentPosition,
                curve_output_mode: CurveOutputMode::PreferG2G3,
                corner_smoothing_enabled: false,
                corner_smoothing_radius_mm: 1.0,
                corner_smoothing_angle_deg: 45.0,
                ..ToolSettings::default()
            },
        );
        let smoothing_settings = ToolSettings {
            printable_area: PrintableArea::new(100.0, 100.0),
            print_speed_mm_s: 25.0,
            lift_height_mm: 2.0,
            print_start_mode: PrintStartMode::DirectFromCurrentPosition,
            curve_output_mode: CurveOutputMode::PreferG2G3,
            corner_smoothing_enabled: true,
            corner_smoothing_radius_mm: 1.0,
            corner_smoothing_angle_deg: 45.0,
            ..ToolSettings::default()
        };
        let smoothed_primitives = build_draw_primitives(&prepared.strokes[0], &smoothing_settings);

        assert!(
            !unsmoothed
                .gcode_lines
                .iter()
                .any(|line| line.starts_with("G2 ") || line.starts_with("G3 "))
        );
        assert_eq!(smoothed_primitives.len(), 3);
        assert!(!points_match(smoothed_primitives[0].end_point(), vec2(10.0, 10.0)));
        assert!(!points_match(smoothed_primitives[1].start_point(), vec2(10.0, 10.0)));
        assert!(!points_match(smoothed_primitives[1].end_point(), vec2(10.0, 10.0)));
    }

    #[test]
    fn omits_redundant_feed_rate_from_consecutive_draw_moves() {
        let prepared = PreparedSvg {
            source_name: "shape.svg".to_owned(),
            strokes: vec![Stroke::new(vec![
                Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0)),
                Segment::line(vec2(10.0, 0.0), vec2(20.0, 0.0)),
            ])],
            fill_regions: Vec::new(),
            warnings: Vec::new(),
            drawing_origin: vec2(0.0, 0.0),
            drawing_bounds: vec2(20.0, 0.0),
            is_out_of_bounds: false,
        };

        let plan = build_plan(prepared, &ToolSettings::default());
        let draw_lines = plan
            .gcode_lines
            .iter()
            .filter(|line| line.starts_with("G1 X"))
            .cloned()
            .collect::<Vec<_>>();

        assert!(draw_lines.iter().any(|line| line.contains(" F")));
        assert!(draw_lines.iter().any(|line| !line.contains(" F")));
    }

    #[test]
    fn converts_fill_regions_to_hatch_draw_strokes() {
        let square = Stroke::new(vec![
            Segment::line(vec2(0.0, 0.0), vec2(10.0, 0.0)),
            Segment::line(vec2(10.0, 0.0), vec2(10.0, 10.0)),
            Segment::line(vec2(10.0, 10.0), vec2(0.0, 10.0)),
            Segment::line(vec2(0.0, 10.0), vec2(0.0, 0.0)),
        ]);
        let prepared = PreparedSvg {
            source_name: "filled.svg".to_owned(),
            strokes: Vec::new(),
            fill_regions: vec![FillRegion::new(vec![square], FillRule::NonZero)],
            warnings: Vec::new(),
            drawing_origin: vec2(0.0, 0.0),
            drawing_bounds: vec2(10.0, 10.0),
            is_out_of_bounds: false,
        };

        let plan = build_plan(
            prepared,
            &ToolSettings {
                printable_area: PrintableArea::new(100.0, 100.0),
                print_start_mode: PrintStartMode::DirectFromCurrentPosition,
                fill_density_percent: 100.0,
                fill_angle_degrees: 0.0,
                ..ToolSettings::default()
            },
        );

        assert!(plan.stats.stroke_count > 1);
        assert!(plan.stats.drawing_distance_mm > 10.0);
        assert!(plan.gcode_lines.iter().any(|line| line.starts_with("G1 X")));
    }

    #[test]
    fn z_motion_falls_back_to_absolute_travel_when_xy_differs() {
        let mut segments = Vec::new();
        let mut segment_end_times_s = Vec::new();
        let mut gcode_lines = Vec::new();
        let mut current = vec3(1.0, 2.0, 3.0);

        push_relative_z_motion_if_needed(
            &mut segments,
            &mut segment_end_times_s,
            &mut gcode_lines,
            &mut current,
            vec3(4.0, 5.0, 6.0),
            10.0,
            600.0,
        );

        assert_eq!(current, vec3(4.0, 5.0, 6.0));
        assert_eq!(gcode_lines, vec!["G1 X4.00 Y5.00 Z6.000 F600"]);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, MotionKind::Travel);
    }

    #[test]
    fn z_motion_does_not_emit_non_finite_gcode() {
        let mut segments = Vec::new();
        let mut segment_end_times_s = Vec::new();
        let mut gcode_lines = Vec::new();
        let mut current = vec3(f32::NAN, 2.0, 3.0);

        push_relative_z_motion_if_needed(
            &mut segments,
            &mut segment_end_times_s,
            &mut gcode_lines,
            &mut current,
            vec3(4.0, 5.0, 6.0),
            10.0,
            600.0,
        );

        assert_eq!(current, vec3(4.0, 5.0, 6.0));
        assert!(segments.is_empty());
        assert!(gcode_lines.is_empty());
    }
}
