use glam::{Vec2, vec2};
use thiserror::Error;
use usvg::{Node, Options, Tree, tiny_skia_path::PathSegment};

use crate::{
    plot::model::{PrintableArea, SvgPlacement},
    svg::ir::{DashPattern, SvgIrSegment, SvgIrStroke},
};

const OUT_OF_BOUNDS_TOLERANCE_MM: f32 = 0.01;

#[derive(Debug, Clone)]
pub struct PreparedSvg {
    pub source_name: String,
    pub strokes: Vec<SvgIrStroke>,
    pub warnings: Vec<String>,
    pub drawing_origin: Vec2,
    pub drawing_bounds: Vec2,
    pub is_out_of_bounds: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedSvg {
    pub source_name: String,
    raw_strokes: Vec<RawStroke>,
    pub warnings: Vec<String>,
    bounds: SourceBounds,
}

#[derive(Debug, Clone)]
struct RawStroke {
    stroke: SvgIrStroke,
    dash_pattern: Option<DashPattern>,
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
    let mut raw_strokes = Vec::new();
    let mut warning_flags = WarningFlags::default();

    collect_group(tree.root(), &mut raw_strokes, &mut warning_flags);

    raw_strokes.retain(|stroke| !stroke.stroke.is_empty());
    if raw_strokes.is_empty() {
        return Err(SvgToolpathError::NoPaths);
    }

    let (min, max) = bounds(&raw_strokes);
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
        raw_strokes,
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

    let map = |point: Vec2| {
        vec2(
            (point.x - parsed.bounds.min.x) * placement.scale_mm_per_unit + drawing_origin.x,
            (parsed.bounds.max.y - point.y) * placement.scale_mm_per_unit + drawing_origin.y,
        )
    };

    let mut strokes = Vec::new();
    for raw_stroke in &parsed.raw_strokes {
        let transformed = raw_stroke.stroke.transformed(map);
        if let Some(dash_pattern) = &raw_stroke.dash_pattern {
            strokes.extend(
                transformed.apply_dash_pattern(&dash_pattern.scaled(placement.scale_mm_per_unit)),
            );
        } else {
            strokes.push(transformed);
        }
    }
    strokes.retain(|stroke| !stroke.is_empty());

    PreparedSvg {
        source_name: parsed.source_name.clone(),
        strokes,
        warnings: parsed.warnings.clone(),
        drawing_origin,
        drawing_bounds,
        is_out_of_bounds: drawing_out_of_bounds(drawing_origin, drawing_bounds, printable_area),
    }
}

fn collect_group(group: &usvg::Group, strokes: &mut Vec<RawStroke>, flags: &mut WarningFlags) {
    for node in group.children() {
        match node {
            Node::Group(group) => collect_group(group, strokes, flags),
            Node::Path(path) if path.is_visible() => {
                let dash_pattern = path
                    .stroke()
                    .and_then(|stroke| DashPattern::new(stroke.dasharray()?, stroke.dashoffset()));
                for stroke in sample_path(path) {
                    strokes.push(RawStroke { stroke, dash_pattern: dash_pattern.clone() });
                }
            }
            Node::Image(_) => flags.saw_image = true,
            Node::Text(_) => flags.saw_text = true,
            _ => {}
        }
    }
}

fn sample_path(path: &usvg::Path) -> Vec<SvgIrStroke> {
    let mut strokes = Vec::new();
    let mut current_segments = Vec::new();
    let transform = path.abs_transform();

    let mut current = Vec2::ZERO;
    let mut move_to = Vec2::ZERO;
    let mut has_current = false;

    let flush = |buffer: &mut Vec<SvgIrSegment>, output: &mut Vec<SvgIrStroke>| {
        if !buffer.is_empty() {
            output.push(SvgIrStroke::new(std::mem::take(buffer)));
        }
    };

    for segment in path.data().segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                flush(&mut current_segments, &mut strokes);
                let mapped = map_point(transform, point);
                current = mapped;
                move_to = mapped;
                has_current = true;
            }
            PathSegment::LineTo(point) => {
                if !has_current {
                    continue;
                }
                let mapped = map_point(transform, point);
                if current.distance_squared(mapped) > 1e-6 {
                    current_segments.push(SvgIrSegment::line(current, mapped));
                    current = mapped;
                }
            }
            PathSegment::QuadTo(control, point) => {
                if !has_current {
                    continue;
                }
                let control = map_point(transform, control);
                let end = map_point(transform, point);
                if current.distance_squared(end) > 1e-6 {
                    current_segments.push(SvgIrSegment::quadratic(current, control, end));
                    current = end;
                }
            }
            PathSegment::CubicTo(control_a, control_b, point) => {
                if !has_current {
                    continue;
                }
                let control_a = map_point(transform, control_a);
                let control_b = map_point(transform, control_b);
                let end = map_point(transform, point);
                if current.distance_squared(end) > 1e-6 {
                    current_segments.push(SvgIrSegment::cubic(current, control_a, control_b, end));
                    current = end;
                }
            }
            PathSegment::Close => {
                if has_current {
                    if current.distance_squared(move_to) > 1e-6 {
                        current_segments.push(SvgIrSegment::line(current, move_to));
                    }
                    flush(&mut current_segments, &mut strokes);
                    current = move_to;
                    has_current = false;
                }
            }
        }
    }

    flush(&mut current_segments, &mut strokes);
    strokes
}

fn bounds(strokes: &[RawStroke]) -> (Vec2, Vec2) {
    let mut min = vec2(f32::INFINITY, f32::INFINITY);
    let mut max = vec2(f32::NEG_INFINITY, f32::NEG_INFINITY);

    for stroke in strokes {
        for segment in &stroke.stroke.segments {
            for point in segment_points_for_bounds(*segment) {
                min.x = min.x.min(point.x);
                min.y = min.y.min(point.y);
                max.x = max.x.max(point.x);
                max.y = max.y.max(point.y);
            }
        }
    }

    (min, max)
}

fn segment_points_for_bounds(segment: SvgIrSegment) -> [Vec2; 4] {
    match segment {
        SvgIrSegment::Line(segment) => [segment.start, segment.end, segment.end, segment.end],
        SvgIrSegment::Quadratic(segment) => {
            [segment.start, segment.control, segment.end, segment.end]
        }
        SvgIrSegment::Cubic(segment) => {
            [segment.start, segment.control_a, segment.control_b, segment.end]
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::ir::{SvgIrSegment, flatten_stroke_to_polyline};
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

        assert_eq!(prepared.strokes.len(), 1);
        let polyline = flatten_stroke_to_polyline(&prepared.strokes[0]);
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
        assert_eq!(prepared.strokes.len(), 1);
        assert_eq!(flatten_stroke_to_polyline(&prepared.strokes[0]).len(), 2);
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
    fn preserves_cubic_segments_in_svg_ir() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
                <path d="M 0 0 C 2 10, 8 10, 10 0" />
            </svg>
        "#;

        let parsed = parse_svg("curve.svg", svg).unwrap();
        let printable_area = PrintableArea::new(100.0, 60.0);
        let prepared =
            prepare_svg(&parsed, parsed.centered_native_placement(printable_area), printable_area);

        assert!(matches!(prepared.strokes[0].segments[0], SvgIrSegment::Cubic(_)));
    }

    #[test]
    fn supports_svg_stroke_dasharray() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 2">
                <path d="M 0 1 L 12 1" fill="none" stroke="black" stroke-dasharray="4 2" />
            </svg>
        "#;

        let parsed = parse_svg("dash.svg", svg).unwrap();
        let printable_area = PrintableArea::new(120.0, 20.0);
        let prepared =
            prepare_svg(&parsed, parsed.centered_native_placement(printable_area), printable_area);

        assert_eq!(prepared.strokes.len(), 2);
        let first = flatten_stroke_to_polyline(&prepared.strokes[0]);
        let second = flatten_stroke_to_polyline(&prepared.strokes[1]);
        assert!((first.first().unwrap().x - 54.0).abs() < 0.05);
        assert!((first.last().unwrap().x - 58.0).abs() < 0.05);
        assert!((second.first().unwrap().x - 60.0).abs() < 0.05);
        assert!((second.last().unwrap().x - 64.0).abs() < 0.05);
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
