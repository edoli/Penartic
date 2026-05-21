use glam::{Vec2, vec2};
use thiserror::Error;
use usvg::{Node, Options, Tree, tiny_skia_path::PathSegment};

use crate::{
    plot::model::{PrintableArea, SvgPlacement},
    res::lang::Language,
};

use super::ir::{DashPattern, Segment, Stroke, StrokeSegments, Strokes};
use super::stroke_processing::{normalize_strokes, stroke_bounds};

const OUT_OF_BOUNDS_TOLERANCE_MM: f32 = 0.01;
const MIN_STROKE_SEGMENT_LENGTH_MM: f32 = 0.5;
const MIN_STROKE_LENGTH_MM: f32 = 0.5;
const MAX_STROKE_JOIN_GAP_MM: f32 = 0.5;

#[derive(Debug, Clone)]
pub struct PreparedSvg {
    pub source_name: String,
    pub strokes: Strokes,
    pub warnings: Vec<String>,
    pub drawing_origin: Vec2,
    pub drawing_bounds: Vec2,
    pub is_out_of_bounds: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedSvg {
    pub source_name: String,
    strokes: Strokes,
    pub warnings: Vec<String>,
    bounds: SourceBounds,
}

#[derive(Debug, Clone)]
struct RawStroke {
    stroke: Stroke,
    dash_pattern: Option<DashPattern>,
}

#[derive(Debug, Clone, Copy)]
struct SourceBounds {
    min: Vec2,
    max: Vec2,
    size: Vec2,
}

#[derive(Debug, Error)]
pub enum SvgParserError {
    #[error("Failed to parse SVG: {0}")]
    Parse(#[from] usvg::Error),
    #[error("No drawable paths were found in the SVG.")]
    NoPaths,
}

impl SvgParserError {
    pub fn localized_message(&self, language: Language) -> String {
        let text = language.strings();
        match self {
            Self::Parse(error) => text.parse_svg_failed(error),
            Self::NoPaths => text.no_drawable_paths_in_svg.to_owned(),
        }
    }
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

    pub fn source_size(&self) -> Vec2 {
        self.bounds.size
    }
}

#[allow(dead_code)]
pub fn parse_svg(
    source_name: impl Into<String>,
    bytes: &[u8],
) -> Result<ParsedSvg, SvgParserError> {
    parse_svg_with_language(source_name, bytes, Language::default())
}

pub fn parse_svg_with_language(
    source_name: impl Into<String>,
    bytes: &[u8],
    language: Language,
) -> Result<ParsedSvg, SvgParserError> {
    let text = language.strings();
    let source_name = source_name.into();
    let tree = Tree::from_data(bytes, &Options::default())?;
    let mut raw_strokes = Vec::new();
    let mut warning_flags = WarningFlags::default();

    collect_group(tree.root(), &mut raw_strokes, &mut warning_flags);

    raw_strokes.retain(|stroke| !stroke.stroke.is_empty());
    if raw_strokes.is_empty() {
        return Err(SvgParserError::NoPaths);
    }

    let Some((min, max)) = stroke_bounds(raw_strokes.iter().map(|raw| &raw.stroke)) else {
        return Err(SvgParserError::NoPaths);
    };
    let source_size = max - min;
    let mut warnings = Vec::new();
    if tree.has_text_nodes() || warning_flags.saw_text {
        warnings.push(text.text_nodes_not_converted.to_owned());
    }
    if warning_flags.saw_image {
        warnings.push(text.image_nodes_not_converted.to_owned());
    }

    let bounds = SourceBounds { min, max, size: source_size };
    let strokes = normalize_strokes(
        strokes_in_source_drawing_space(&raw_strokes, bounds),
        MIN_STROKE_SEGMENT_LENGTH_MM,
        MIN_STROKE_LENGTH_MM,
        MAX_STROKE_JOIN_GAP_MM,
    );
    if strokes.is_empty() {
        return Err(SvgParserError::NoPaths);
    }

    Ok(ParsedSvg { source_name, strokes, warnings, bounds })
}

pub fn prepare_svg(
    parsed: &ParsedSvg,
    placement: SvgPlacement,
    printable_area: PrintableArea,
) -> PreparedSvg {
    let mut placement = placement;
    placement.sanitize();

    let source_size = parsed.source_size();
    let source_center = source_size * 0.5;
    let rotation = placement.rotation_degrees.to_radians();
    let (sin, cos) = rotation.sin_cos();
    let map = |point: Vec2| {
        let scaled = (point - source_center) * placement.scale_mm_per_unit;
        let rotated = vec2(scaled.x * cos - scaled.y * sin, scaled.x * sin + scaled.y * cos);
        rotated + placement.center_mm
    };
    let strokes: Strokes = parsed.strokes.iter().map(|stroke| stroke.transformed(map)).collect();
    let (drawing_origin, drawing_max) =
        stroke_bounds(strokes.iter()).unwrap_or((placement.center_mm, placement.center_mm));
    let drawing_bounds = drawing_max - drawing_origin;

    PreparedSvg {
        source_name: parsed.source_name.clone(),
        strokes,
        warnings: parsed.warnings.clone(),
        drawing_origin,
        drawing_bounds,
        is_out_of_bounds: drawing_out_of_bounds(drawing_origin, drawing_bounds, printable_area),
    }
}

fn strokes_in_source_drawing_space(raw_strokes: &[RawStroke], bounds: SourceBounds) -> Strokes {
    let map = |point: Vec2| vec2(point.x - bounds.min.x, bounds.max.y - point.y);
    let mut strokes = Vec::new();

    for raw_stroke in raw_strokes {
        let transformed = raw_stroke.stroke.transformed(map);
        if let Some(dash_pattern) = &raw_stroke.dash_pattern {
            strokes.extend(transformed.apply_dash_pattern(dash_pattern));
        } else {
            strokes.push(transformed);
        }
    }

    strokes
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

fn sample_path(path: &usvg::Path) -> Strokes {
    let mut strokes: Strokes = Vec::new();
    let mut current_segments: StrokeSegments = Vec::new();
    let transform = path.abs_transform();

    let mut current = Vec2::ZERO;
    let mut move_to = Vec2::ZERO;
    let mut has_current = false;

    let flush = |buffer: &mut StrokeSegments, output: &mut Strokes| {
        if !buffer.is_empty() {
            output.push(Stroke::new(std::mem::take(buffer)));
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
                    current_segments.push(Segment::line(current, mapped));
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
                    current_segments.push(Segment::quadratic(current, control, end));
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
                    current_segments.push(Segment::cubic(current, control_a, control_b, end));
                    current = end;
                }
            }
            PathSegment::Close => {
                if has_current {
                    if current.distance_squared(move_to) > 1e-6 {
                        current_segments.push(Segment::line(current, move_to));
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
    use crate::paths::{Segment, flatten_stroke_to_polyline};
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
    fn preserves_cubic_segments_in_ir() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
                <path d="M 0 0 C 2 10, 8 10, 10 0" />
            </svg>
        "#;

        let parsed = parse_svg("curve.svg", svg).unwrap();
        let printable_area = PrintableArea::new(100.0, 60.0);
        let prepared =
            prepare_svg(&parsed, parsed.centered_native_placement(printable_area), printable_area);

        assert!(matches!(prepared.strokes[0].segments[0], Segment::Cubic(_)));
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
    fn merges_short_svg_segments_before_reordering() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 1">
                <path d="M 0 0 L 10 0 L 10.2 0 L 20 0" />
            </svg>
        "#;

        let parsed = parse_svg("short-segment.svg", svg).unwrap();

        assert_eq!(parsed.strokes.len(), 1);
        assert_eq!(parsed.strokes[0].segments.len(), 2);
        assert_eq!(parsed.strokes[0].segments[0].end_point(), vec2(10.2, 0.0));
    }

    #[test]
    fn removes_tiny_svg_strokes_before_reordering() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 1">
                <path d="M 0 0 L 0.1 0 L 0.2 0" />
                <path d="M 10 0 L 20 0" />
            </svg>
        "#;

        let parsed = parse_svg("tiny-stroke.svg", svg).unwrap();

        assert_eq!(parsed.strokes.len(), 1);
        assert_eq!(parsed.strokes[0].start_point(), Some(vec2(10.0, 0.0)));
        assert_eq!(parsed.strokes[0].end_point(), Some(vec2(20.0, 0.0)));
    }

    #[test]
    fn merges_ordered_strokes_when_endpoint_gap_is_short() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 1">
                <path d="M 0 0 L 10 0" />
                <path d="M 10.2 0 L 20 0" />
            </svg>
        "#;

        let parsed = parse_svg("short-gap.svg", svg).unwrap();

        assert_eq!(parsed.strokes.len(), 1);
        assert_eq!(parsed.strokes[0].segments.len(), 2);
        assert_eq!(parsed.strokes[0].segments[1].start_point(), vec2(10.0, 0.0));
        assert_eq!(parsed.strokes[0].end_point(), Some(vec2(20.0, 0.0)));
    }

    #[test]
    fn keeps_ordered_strokes_separate_when_endpoint_gap_is_long() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 1">
                <path d="M 0 0 L 10 0" />
                <path d="M 11 0 L 20 0" />
            </svg>
        "#;

        let parsed = parse_svg("long-gap.svg", svg).unwrap();

        assert_eq!(parsed.strokes.len(), 2);
    }

    #[test]
    fn reorders_strokes_by_nearest_endpoint_after_placement() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 1">
                <path d="M 90 0 L 100 0" />
                <path d="M 0 0 L 10 0" />
                <path d="M 12 0 L 20 0" />
            </svg>
        "#;

        let parsed = parse_svg("unordered.svg", svg).unwrap();
        let prepared = prepare_svg(
            &parsed,
            SvgPlacement::new(vec2(50.0, 0.0), 1.0),
            PrintableArea::new(120.0, 20.0),
        );

        let starts = prepared
            .strokes
            .iter()
            .map(|stroke| stroke.start_point().unwrap().x)
            .collect::<Vec<_>>();

        assert_eq!(starts, vec![0.0, 12.0, 90.0]);
    }

    #[test]
    fn reverses_stroke_when_its_end_is_closer_to_current_position() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 1">
                <path d="M 20 0 L 10 0" />
            </svg>
        "#;

        let parsed = parse_svg("reverse.svg", svg).unwrap();
        let prepared = prepare_svg(
            &parsed,
            SvgPlacement::new(vec2(10.0, 0.0), 1.0),
            PrintableArea::new(40.0, 20.0),
        );
        let polyline = flatten_stroke_to_polyline(&prepared.strokes[0]);

        assert_eq!(polyline[0], vec2(5.0, 0.0));
        assert_eq!(polyline[1], vec2(15.0, 0.0));
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
