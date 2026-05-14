use glam::{Vec2, vec2};
use thiserror::Error;
use usvg::{Node, Options, Tree, tiny_skia_path::PathSegment};

use crate::plot::model::{PrintableArea, SvgPlacement};

const COLLINEAR_SIMPLIFY_DISTANCE_MM: f32 = 0.05;
const COLLINEAR_SIMPLIFY_DOT_THRESHOLD: f32 = 0.9995;
const OUT_OF_BOUNDS_TOLERANCE_MM: f32 = 0.01;

#[derive(Debug, Clone)]
pub struct PreparedSvg {
    pub source_name: String,
    pub polylines: Vec<Vec<Vec2>>,
    pub warnings: Vec<String>,
    pub drawing_origin: Vec2,
    pub drawing_bounds: Vec2,
    pub is_out_of_bounds: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedSvg {
    pub source_name: String,
    raw_polylines: Vec<Vec<Vec2>>,
    pub warnings: Vec<String>,
    bounds: SourceBounds,
}

#[derive(Debug, Clone, Copy)]
struct SourceBounds {
    min: Vec2,
    max: Vec2,
    size: Vec2,
}

#[derive(Debug, Error)]
pub enum SvgToolpathError {
    #[error("SVG를 읽을 수 없습니다: {0}")]
    Parse(#[from] usvg::Error),
    #[error("SVG 안에서 그릴 수 있는 path를 찾지 못했습니다.")]
    NoPaths,
}

#[derive(Default)]
struct WarningFlags {
    saw_text: bool,
    saw_image: bool,
}

impl ParsedSvg {
    pub fn centered_native_placement(&self, printable_area: PrintableArea) -> SvgPlacement {
        let scale_mm_per_unit = 1.0;
        let center_mm = vec2(printable_area.width_mm * 0.5, printable_area.height_mm * 0.5);
        SvgPlacement::new(center_mm, scale_mm_per_unit)
    }

    pub fn drawing_size_for(&self, placement: SvgPlacement) -> Vec2 {
        self.bounds.size * placement.scale_mm_per_unit
    }
}

pub fn parse_svg(
    source_name: impl Into<String>,
    bytes: &[u8],
) -> Result<ParsedSvg, SvgToolpathError> {
    let source_name = source_name.into();
    let tree = Tree::from_data(bytes, &Options::default())?;
    let mut raw_polylines = Vec::new();
    let mut warning_flags = WarningFlags::default();

    collect_group(tree.root(), &mut raw_polylines, &mut warning_flags);

    raw_polylines.retain(|polyline| polyline.len() >= 2);
    if raw_polylines.is_empty() {
        return Err(SvgToolpathError::NoPaths);
    }

    let (min, max) = bounds(&raw_polylines);
    let source_size = max - min;
    let mut warnings = Vec::new();
    if tree.has_text_nodes() || warning_flags.saw_text {
        warnings
            .push("텍스트 노드는 현재 툴패스로 변환되지 않아 미리보기에서 제외됩니다.".to_owned());
    }
    if warning_flags.saw_image {
        warnings.push(
            "내장 이미지 노드는 현재 툴패스로 변환되지 않아 미리보기에서 제외됩니다.".to_owned(),
        );
    }

    Ok(ParsedSvg {
        source_name,
        raw_polylines,
        warnings,
        bounds: SourceBounds { min, max, size: source_size },
    })
}

pub fn prepare_svg(
    parsed: &ParsedSvg,
    placement: SvgPlacement,
    printable_area: PrintableArea,
) -> PreparedSvg {
    let mut placement = placement;
    placement.sanitize();

    let drawing_bounds = parsed.drawing_size_for(placement);
    let drawing_origin = placement.drawing_origin(drawing_bounds);
    let polylines = parsed
        .raw_polylines
        .iter()
        .map(|polyline| {
            let scaled = polyline
                .iter()
                .copied()
                .map(|point| {
                    vec2(
                        (point.x - parsed.bounds.min.x) * placement.scale_mm_per_unit
                            + drawing_origin.x,
                        (parsed.bounds.max.y - point.y) * placement.scale_mm_per_unit
                            + drawing_origin.y,
                    )
                })
                .collect::<Vec<_>>();
            simplify_polyline(&scaled)
        })
        .collect::<Vec<_>>();

    PreparedSvg {
        source_name: parsed.source_name.clone(),
        polylines,
        warnings: parsed.warnings.clone(),
        drawing_origin,
        drawing_bounds,
        is_out_of_bounds: drawing_out_of_bounds(drawing_origin, drawing_bounds, printable_area),
    }
}

fn collect_group(group: &usvg::Group, polylines: &mut Vec<Vec<Vec2>>, flags: &mut WarningFlags) {
    for node in group.children() {
        match node {
            Node::Group(group) => collect_group(group, polylines, flags),
            Node::Path(path) if path.is_visible() => {
                polylines.extend(sample_path(path));
            }
            Node::Image(_) => flags.saw_image = true,
            Node::Text(_) => flags.saw_text = true,
            _ => {}
        }
    }
}

fn sample_path(path: &usvg::Path) -> Vec<Vec<Vec2>> {
    let mut polylines = Vec::new();
    let mut current_polyline = Vec::new();
    let transform = path.abs_transform();

    let mut current = Vec2::ZERO;
    let mut move_to = Vec2::ZERO;
    let mut has_current = false;

    let flush = |buffer: &mut Vec<Vec2>, output: &mut Vec<Vec<Vec2>>| {
        dedupe_polyline(buffer);
        if buffer.len() >= 2 {
            output.push(std::mem::take(buffer));
        } else {
            buffer.clear();
        }
    };

    for segment in path.data().segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                flush(&mut current_polyline, &mut polylines);
                let mapped = map_point(transform, point);
                current_polyline.push(mapped);
                current = mapped;
                move_to = mapped;
                has_current = true;
            }
            PathSegment::LineTo(point) => {
                if !has_current {
                    continue;
                }
                let mapped = map_point(transform, point);
                push_unique(&mut current_polyline, mapped);
                current = mapped;
            }
            PathSegment::QuadTo(control, point) => {
                if !has_current {
                    continue;
                }
                let control = map_point(transform, control);
                let end = map_point(transform, point);
                append_quadratic(&mut current_polyline, current, control, end);
                current = end;
            }
            PathSegment::CubicTo(control_a, control_b, point) => {
                if !has_current {
                    continue;
                }
                let control_a = map_point(transform, control_a);
                let control_b = map_point(transform, control_b);
                let end = map_point(transform, point);
                append_cubic(&mut current_polyline, current, control_a, control_b, end);
                current = end;
            }
            PathSegment::Close => {
                if has_current {
                    push_unique(&mut current_polyline, move_to);
                    flush(&mut current_polyline, &mut polylines);
                    current = move_to;
                    has_current = false;
                }
            }
        }
    }

    flush(&mut current_polyline, &mut polylines);
    polylines
}

fn bounds(polylines: &[Vec<Vec2>]) -> (Vec2, Vec2) {
    let mut min = vec2(f32::INFINITY, f32::INFINITY);
    let mut max = vec2(f32::NEG_INFINITY, f32::NEG_INFINITY);

    for polyline in polylines {
        for point in polyline {
            min.x = min.x.min(point.x);
            min.y = min.y.min(point.y);
            max.x = max.x.max(point.x);
            max.y = max.y.max(point.y);
        }
    }

    (min, max)
}

fn drawing_out_of_bounds(origin: Vec2, size: Vec2, printable_area: PrintableArea) -> bool {
    origin.x < -OUT_OF_BOUNDS_TOLERANCE_MM
        || origin.y < -OUT_OF_BOUNDS_TOLERANCE_MM
        || origin.x + size.x > printable_area.width_mm + OUT_OF_BOUNDS_TOLERANCE_MM
        || origin.y + size.y > printable_area.height_mm + OUT_OF_BOUNDS_TOLERANCE_MM
}

fn map_point(transform: usvg::Transform, point: usvg::tiny_skia_path::Point) -> Vec2 {
    let mut point = point;
    transform.map_point(&mut point);
    vec2(point.x, point.y)
}

fn push_unique(polyline: &mut Vec<Vec2>, point: Vec2) {
    if polyline.last().is_none_or(|last| last.distance_squared(point) > 1e-6) {
        polyline.push(point);
    }
}

fn dedupe_polyline(polyline: &mut Vec<Vec2>) {
    let mut cleaned = Vec::with_capacity(polyline.len());

    for point in polyline.iter().copied() {
        if cleaned.last().is_none_or(|last: &Vec2| last.distance_squared(point) > 1e-6) {
            cleaned.push(point);
        }
    }

    *polyline = cleaned;
}

fn simplify_polyline(polyline: &[Vec2]) -> Vec<Vec2> {
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
    let direction_match = ab_dir.dot(bc_dir) >= COLLINEAR_SIMPLIFY_DOT_THRESHOLD;
    let deviation_sq = point_to_segment_distance_sq(b, a, c);
    direction_match && deviation_sq <= COLLINEAR_SIMPLIFY_DISTANCE_MM.powi(2)
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

fn append_quadratic(polyline: &mut Vec<Vec2>, start: Vec2, control: Vec2, end: Vec2) {
    let control_length = start.distance(control) + control.distance(end);
    let steps = ((control_length / 12.0).ceil() as usize).clamp(6, 48);

    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let point = quadratic(start, control, end, t);
        push_unique(polyline, point);
    }
}

fn append_cubic(
    polyline: &mut Vec<Vec2>,
    start: Vec2,
    control_a: Vec2,
    control_b: Vec2,
    end: Vec2,
) {
    let control_length =
        start.distance(control_a) + control_a.distance(control_b) + control_b.distance(end);
    let steps = ((control_length / 12.0).ceil() as usize).clamp(8, 96);

    for step in 1..=steps {
        let t = step as f32 / steps as f32;
        let point = cubic(start, control_a, control_b, end, t);
        push_unique(polyline, point);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    #[test]
    fn prepares_simple_svg_with_native_mm_size() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
                <path d="M 0 0 L 10 10" />
            </svg>
        "#;

        let parsed = parse_svg("line.svg", svg).unwrap();
        let printable_area = PrintableArea::new(100.0, 60.0);
        let prepared =
            prepare_svg(&parsed, parsed.centered_native_placement(printable_area), printable_area);

        assert_eq!(prepared.polylines.len(), 1);
        let polyline = &prepared.polylines[0];
        assert!((prepared.drawing_bounds.x - 10.0).abs() <= f32::EPSILON);
        assert!((prepared.drawing_bounds.y - 10.0).abs() <= f32::EPSILON);
        assert!((prepared.drawing_origin.x - 45.0).abs() <= f32::EPSILON);
        assert!((prepared.drawing_origin.y - 25.0).abs() <= f32::EPSILON);
        assert!((polyline[0].x - 45.0).abs() <= f32::EPSILON);
        assert!((polyline[0].y - 35.0).abs() <= f32::EPSILON);
        assert!((polyline[1].x - 55.0).abs() <= f32::EPSILON);
        assert!((polyline[1].y - 25.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn simplifies_collinear_svg_points_after_scaling() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 1">
                <path d="M 0 0 L 3 0 L 6 0 L 10 0" />
            </svg>
        "#;

        let parsed = parse_svg("line.svg", svg).unwrap();
        let printable_area = PrintableArea::new(100.0, 60.0);
        let prepared =
            prepare_svg(&parsed, parsed.centered_native_placement(printable_area), printable_area);
        assert_eq!(prepared.polylines.len(), 1);
        assert_eq!(prepared.polylines[0].len(), 2);
    }

    #[test]
    fn keeps_svg_size_and_position_when_printable_area_changes() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10">
                <path d="M 0 0 L 20 10" />
            </svg>
        "#;

        let parsed = parse_svg("line.svg", svg).unwrap();
        let placement = parsed.centered_native_placement(PrintableArea::new(100.0, 60.0));
        let small = prepare_svg(&parsed, placement, PrintableArea::new(100.0, 60.0));
        let large = prepare_svg(&parsed, placement, PrintableArea::new(240.0, 180.0));

        assert_eq!(small.drawing_origin, large.drawing_origin);
        assert_eq!(small.drawing_bounds, large.drawing_bounds);
        assert!(!large.is_out_of_bounds);
    }

    #[test]
    fn marks_svg_as_out_of_bounds_when_it_exits_printable_area() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
                <path d="M 0 0 L 10 10" />
            </svg>
        "#;

        let parsed = parse_svg("line.svg", svg).unwrap();
        let placement = SvgPlacement::new(vec2(80.0, 50.0), 10.0);
        let prepared = prepare_svg(&parsed, placement, PrintableArea::new(100.0, 100.0));

        assert!(prepared.is_out_of_bounds);
    }

    #[test]
    fn loads_all_sample_svg_assets_from_repository() {
        let sample_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let entries = fs::read_dir(&sample_dir).unwrap_or_else(|error| {
            panic!("failed to read sample dir {}: {error}", sample_dir.display())
        });

        let mut loaded_svg_count = 0;

        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("svg") {
                continue;
            }

            let bytes = fs::read(&path).unwrap_or_else(|error| {
                panic!("failed to read sample SVG {}: {error}", path.display())
            });
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());

            parse_svg(file_name, &bytes).unwrap_or_else(|error| {
                panic!("failed to load sample SVG {}: {error}", path.display())
            });

            loaded_svg_count += 1;
        }

        assert!(
            loaded_svg_count > 0,
            "expected at least one sample SVG in {}",
            sample_dir.display()
        );
    }
}
