use glam::Vec2;

use super::ir::{Segment, Stroke, Strokes};

const KDTREE_STROKE_ORDERING_THRESHOLD: usize = 64;

pub(crate) fn normalize_strokes(
    strokes: Strokes,
    min_segment_length: f32,
    min_stroke_length: f32,
    max_join_gap: f32,
) -> Strokes {
    let mut strokes = merge_short_stroke_segments(strokes, min_segment_length, min_stroke_length);
    if strokes.is_empty() {
        return strokes;
    }

    optimize_stroke_order(&mut strokes);
    merge_close_ordered_strokes(strokes, max_join_gap)
}

pub(crate) fn stroke_bounds<'a>(
    strokes: impl IntoIterator<Item = &'a Stroke>,
) -> Option<(Vec2, Vec2)> {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut saw_segment = false;

    for stroke in strokes {
        for segment in &stroke.segments {
            saw_segment = true;
            for point in segment_points_for_bounds(*segment) {
                min.x = min.x.min(point.x);
                min.y = min.y.min(point.y);
                max.x = max.x.max(point.x);
                max.y = max.y.max(point.y);
            }
        }
    }

    saw_segment.then_some((min, max))
}

fn merge_short_stroke_segments(
    strokes: Strokes,
    min_segment_length: f32,
    min_stroke_length: f32,
) -> Strokes {
    strokes
        .into_iter()
        .filter_map(|stroke| stroke.merge_short_segments(min_segment_length, min_stroke_length))
        .collect()
}

fn merge_close_ordered_strokes(strokes: Strokes, max_join_gap: f32) -> Strokes {
    let mut merged: Strokes = Vec::with_capacity(strokes.len());

    for stroke in strokes {
        if let Some(previous) = merged.last_mut() {
            if previous.append_if_gap_within(stroke.clone(), max_join_gap) {
                continue;
            }
        }

        merged.push(stroke);
    }

    merged
}

fn optimize_stroke_order(strokes: &mut Strokes) {
    if strokes.len() <= 1 || strokes.len() < KDTREE_STROKE_ORDERING_THRESHOLD {
        optimize_stroke_order_exact(strokes);
        return;
    }

    let Some(endpoint_tree) = StrokeEndpointTree::new(strokes) else {
        optimize_stroke_order_exact(strokes);
        return;
    };

    let mut unordered = std::mem::take(strokes).into_iter().map(Some).collect::<Vec<_>>();
    let mut active = vec![true; unordered.len()];
    let mut remaining = unordered.len();
    let mut ordered = Vec::with_capacity(unordered.len());
    let mut current = Vec2::ZERO;

    while remaining > 0 {
        let Some(endpoint) = endpoint_tree.nearest(current, &active) else {
            break;
        };

        active[endpoint.stroke_index] = false;
        remaining -= 1;

        let Some(stroke) = unordered[endpoint.stroke_index].take() else {
            continue;
        };
        let stroke = if endpoint.reverse { stroke.reversed() } else { stroke };
        if let Some(end) = stroke.end_point() {
            current = end;
        }
        ordered.push(stroke);
    }

    ordered.extend(unordered.into_iter().flatten());
    *strokes = ordered;
}

fn optimize_stroke_order_exact(strokes: &mut Strokes) {
    let mut unordered = std::mem::take(strokes);
    let mut ordered = Vec::with_capacity(unordered.len());
    let mut current = Vec2::ZERO;

    while !unordered.is_empty() {
        let Some((best_index, reverse)) = nearest_stroke(&unordered, current) else {
            break;
        };

        let stroke = unordered.swap_remove(best_index);
        let stroke = if reverse { stroke.reversed() } else { stroke };
        if let Some(end) = stroke.end_point() {
            current = end;
        }
        ordered.push(stroke);
    }

    *strokes = ordered;
}

#[derive(Clone, Copy)]
struct StrokeEndpoint {
    stroke_index: usize,
    reverse: bool,
    point: Vec2,
}

impl StrokeEndpoint {
    fn new(stroke_index: usize, reverse: bool, point: Vec2) -> Self {
        Self { stroke_index, reverse, point }
    }

    fn coordinate(self, axis: usize) -> f32 {
        coordinate(self.point, axis)
    }
}

struct StrokeEndpointTree {
    endpoints: Vec<StrokeEndpoint>,
    nodes: Vec<StrokeEndpointNode>,
    root: Option<usize>,
}

struct StrokeEndpointNode {
    endpoint_index: usize,
    axis: usize,
    left: Option<usize>,
    right: Option<usize>,
}

impl StrokeEndpointTree {
    fn new(strokes: &[Stroke]) -> Option<Self> {
        let mut endpoints = Vec::with_capacity(strokes.len() * 2);
        for (stroke_index, stroke) in strokes.iter().enumerate() {
            endpoints.push(StrokeEndpoint::new(stroke_index, false, stroke.start_point()?));
            endpoints.push(StrokeEndpoint::new(stroke_index, true, stroke.end_point()?));
        }

        let mut tree = Self { endpoints, nodes: Vec::with_capacity(strokes.len() * 2), root: None };
        let mut endpoint_indices = (0..tree.endpoints.len()).collect::<Vec<_>>();
        tree.root = tree.build(&mut endpoint_indices, 0);
        Some(tree)
    }

    fn build(&mut self, endpoint_indices: &mut [usize], depth: usize) -> Option<usize> {
        if endpoint_indices.is_empty() {
            return None;
        }

        let axis = depth % 2;
        endpoint_indices.sort_unstable_by(|left, right| {
            self.endpoints[*left]
                .coordinate(axis)
                .total_cmp(&self.endpoints[*right].coordinate(axis))
                .then_with(|| left.cmp(right))
        });

        let mid = endpoint_indices.len() / 2;
        let (left_indices, pivot_and_right) = endpoint_indices.split_at_mut(mid);
        let (pivot, right_indices) = pivot_and_right.split_at_mut(1);
        let node_index = self.nodes.len();
        self.nodes.push(StrokeEndpointNode {
            endpoint_index: pivot[0],
            axis,
            left: None,
            right: None,
        });

        let left = self.build(left_indices, depth + 1);
        let right = self.build(right_indices, depth + 1);
        self.nodes[node_index].left = left;
        self.nodes[node_index].right = right;
        Some(node_index)
    }

    fn nearest(&self, current: Vec2, active: &[bool]) -> Option<StrokeEndpoint> {
        let mut best = None;
        self.nearest_in_node(self.root?, current, active, &mut best);
        best.map(|(endpoint_index, _)| self.endpoints[endpoint_index])
    }

    fn nearest_in_node(
        &self,
        node_index: usize,
        current: Vec2,
        active: &[bool],
        best: &mut Option<(usize, f32)>,
    ) {
        let node = &self.nodes[node_index];
        let endpoint = self.endpoints[node.endpoint_index];
        if active.get(endpoint.stroke_index).copied().unwrap_or(false) {
            let distance = current.distance_squared(endpoint.point);
            if should_replace_nearest(*best, node.endpoint_index, distance) {
                *best = Some((node.endpoint_index, distance));
            }
        }

        let current_coordinate = coordinate(current, node.axis);
        let split_coordinate = endpoint.coordinate(node.axis);
        let (near, far) = if current_coordinate <= split_coordinate {
            (node.left, node.right)
        } else {
            (node.right, node.left)
        };

        if let Some(near) = near {
            self.nearest_in_node(near, current, active, best);
        }

        let axis_distance = current_coordinate - split_coordinate;
        if best.is_none_or(|(_, best_distance)| axis_distance * axis_distance <= best_distance) {
            if let Some(far) = far {
                self.nearest_in_node(far, current, active, best);
            }
        }
    }
}

fn coordinate(point: Vec2, axis: usize) -> f32 {
    if axis == 0 { point.x } else { point.y }
}

fn should_replace_nearest(
    best: Option<(usize, f32)>,
    candidate_index: usize,
    candidate_distance: f32,
) -> bool {
    let Some((best_index, best_distance)) = best else {
        return true;
    };
    candidate_distance < best_distance
        || (candidate_distance == best_distance && candidate_index < best_index)
}

fn nearest_stroke(strokes: &[Stroke], current: Vec2) -> Option<(usize, bool)> {
    strokes
        .iter()
        .enumerate()
        .filter_map(|(index, stroke)| {
            let start = stroke.start_point()?;
            let end = stroke.end_point()?;
            let start_distance = current.distance_squared(start);
            let end_distance = current.distance_squared(end);
            if end_distance < start_distance {
                Some((index, true, end_distance))
            } else {
                Some((index, false, start_distance))
            }
        })
        .min_by(|left, right| left.2.total_cmp(&right.2))
        .map(|(index, reverse, _)| (index, reverse))
}

fn segment_points_for_bounds(segment: Segment) -> [Vec2; 4] {
    match segment {
        Segment::Line(segment) => [segment.start, segment.end, segment.end, segment.end],
        Segment::Quadratic(segment) => [segment.start, segment.control, segment.end, segment.end],
        Segment::Cubic(segment) => {
            [segment.start, segment.control_a, segment.control_b, segment.end]
        }
    }
}

#[cfg(test)]
mod tests {
    use glam::vec2;

    use super::*;
    use crate::paths::flatten_stroke_to_polyline;

    #[test]
    fn kdtree_ordering_reverses_stroke_when_its_end_is_closer() {
        let mut strokes = vec![Stroke::new(vec![Segment::line(vec2(100.0, 0.0), vec2(1.0, 0.0))])];
        for index in 0..KDTREE_STROKE_ORDERING_THRESHOLD {
            let x = 200.0 + index as f32 * 10.0;
            strokes.push(Stroke::new(vec![Segment::line(vec2(x, 0.0), vec2(x + 1.0, 0.0))]));
        }

        optimize_stroke_order(&mut strokes);
        let polyline = flatten_stroke_to_polyline(&strokes[0]);

        assert_eq!(polyline[0], vec2(1.0, 0.0));
        assert_eq!(polyline[1], vec2(100.0, 0.0));
    }
}
