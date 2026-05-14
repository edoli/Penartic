use glam::{Vec2, vec2};
use thiserror::Error;
use usvg::{Node, Options, Tree, tiny_skia_path::PathSegment};

use crate::plot::model::PrintableArea;

#[derive(Debug, Clone)]
pub struct PreparedSvg {
    pub source_name: String,
    pub polylines: Vec<Vec<Vec2>>,
    pub warnings: Vec<String>,
    pub drawing_bounds: Vec2,
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

pub fn prepare_svg(
    source_name: impl Into<String>,
    bytes: &[u8],
    printable_area: PrintableArea,
) -> Result<PreparedSvg, SvgToolpathError> {
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
    let safe_width = source_size.x.max(1e-3);
    let safe_height = source_size.y.max(1e-3);
    let scale = if source_size.x <= 1e-3 {
        printable_area.height_mm / safe_height
    } else if source_size.y <= 1e-3 {
        printable_area.width_mm / safe_width
    } else {
        (printable_area.width_mm / safe_width).min(printable_area.height_mm / safe_height)
    };

    let drawing_bounds = vec2(source_size.x * scale, source_size.y * scale);
    let offset = vec2(
        (printable_area.width_mm - drawing_bounds.x) * 0.5,
        (printable_area.height_mm - drawing_bounds.y) * 0.5,
    );

    let polylines = raw_polylines
        .into_iter()
        .map(|polyline| {
            polyline
                .into_iter()
                .map(|point| {
                    vec2((point.x - min.x) * scale + offset.x, (max.y - point.y) * scale + offset.y)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

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

    Ok(PreparedSvg { source_name: source_name.into(), polylines, warnings, drawing_bounds })
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
    fn prepares_simple_svg_into_centered_points() {
        let svg = br#"
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
                <path d="M 0 0 L 10 10" />
            </svg>
        "#;

        let prepared = prepare_svg("line.svg", svg, PrintableArea::new(100.0, 60.0)).unwrap();

        assert_eq!(prepared.polylines.len(), 1);
        let polyline = &prepared.polylines[0];
        assert!(polyline[0].x >= 0.0 && polyline[0].x <= 100.0);
        assert!(polyline[0].y >= 0.0 && polyline[0].y <= 60.0);
        assert!(polyline[1].x >= 0.0 && polyline[1].x <= 100.0);
        assert!(polyline[1].y >= 0.0 && polyline[1].y <= 60.0);
        assert!(prepared.drawing_bounds.x <= 100.0 + f32::EPSILON);
        assert!(prepared.drawing_bounds.y <= 60.0 + f32::EPSILON);
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

            prepare_svg(file_name, &bytes, PrintableArea::default()).unwrap_or_else(|error| {
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
