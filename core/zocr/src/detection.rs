//! DB detector post-processing: probability map to text-line quads.
//!
//! Flood-fill regions above 0.3, take the hull's minimum rectangle, grow it by
//! `area * 1.5 / perimeter` (the DB "unclip" ratio), and map back to the source
//! image. Lines come out top to bottom, then left to right within a row.

use std::collections::VecDeque;

use clipper2_rust::{inflate_paths_64, EndType, JoinType, Point64};

use crate::geometry::{self, Point, Quad};

/// Probability floor for a text pixel.
const THRESHOLD: f32 = 0.3;
/// Stop after this many regions; a page never has that many real lines.
const MAX_CANDIDATES: usize = 1000;

/// One detected text line.
#[derive(Clone, Debug, PartialEq)]
pub struct Detected {
    /// Corners in source pixels, reading order.
    pub quad: Quad,
    /// Mean probability of the region.
    pub score: f32,
}

/// Turns a `height × width` probability map into lines in a
/// `source_width × source_height` image. `None` when the map size is wrong.
pub fn postprocess(
    probability: &[f32],
    width: usize,
    height: usize,
    source_width: u32,
    source_height: u32,
) -> Option<Vec<Detected>> {
    if width == 0 || height == 0 || width.checked_mul(height) != Some(probability.len()) {
        return None;
    }
    let mut seen = vec![false; probability.len()];
    let mut found = Vec::new();
    let mut candidates = 0;
    for start in 0..probability.len() {
        if seen[start] || probability[start] < THRESHOLD {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        seen[start] = true;
        let mut boundary = Vec::new();
        let mut sum = 0.0_f32;
        let mut count = 0_usize;
        while let Some(p) = queue.pop_front() {
            let x = p % width;
            let y = p / width;
            sum += probability[p];
            count += 1;
            let mut edge = x == 0 || y == 0 || x + 1 == width || y + 1 == height;
            for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let n = ny * width + nx;
                    if probability[n] < THRESHOLD {
                        edge = true;
                    } else if !seen[n] {
                        seen[n] = true;
                        queue.push_back(n);
                    }
                }
            }
            if edge {
                let (fx, fy) = (x as f64, y as f64);
                boundary.extend([
                    [fx, fy],
                    [fx + 1.0, fy],
                    [fx + 1.0, fy + 1.0],
                    [fx, fy + 1.0],
                ]);
            }
        }
        if count < 3 {
            continue;
        }
        candidates += 1;
        if candidates > MAX_CANDIDATES {
            break;
        }
        let hull = geometry::hull(boundary);
        let Some(initial) = geometry::min_quad(&hull) else {
            continue;
        };
        if geometry::distance(initial[0], initial[1])
            .min(geometry::distance(initial[1], initial[2]))
            < 3.0
        {
            continue;
        }
        let (area, perimeter) = geometry::area_perimeter(&hull);
        if perimeter <= 0.0 {
            continue;
        }
        let path: Vec<Point64> = hull
            .iter()
            .map(|p| {
                Point64::new(
                    (p[0] * 1024.0).round() as i64,
                    (p[1] * 1024.0).round() as i64,
                )
            })
            .collect();
        let expanded = inflate_paths_64(
            &vec![path],
            area * 1.5 / perimeter * 1024.0,
            JoinType::Round,
            EndType::Polygon,
            2.0,
            0.0,
        );
        // A line must stay one polygon after growing.
        if expanded.len() != 1 {
            continue;
        }
        let polygon: Vec<Point> = expanded[0]
            .iter()
            .map(|p| [p.x as f64 / 1024.0, p.y as f64 / 1024.0])
            .collect();
        let Some(mut quad) = geometry::min_quad(&geometry::hull(polygon)) else {
            continue;
        };
        let max_x = f64::from(source_width.saturating_sub(1));
        let max_y = f64::from(source_height.saturating_sub(1));
        for p in &mut quad {
            p[0] = (p[0] * f64::from(source_width) / width as f64).clamp(0.0, max_x);
            p[1] = (p[1] * f64::from(source_height) / height as f64).clamp(0.0, max_y);
        }
        if geometry::distance(quad[0], quad[1]).min(geometry::distance(quad[1], quad[2])) < 3.0 {
            continue;
        }
        found.push(Detected {
            quad,
            score: sum / count as f32,
        });
    }
    found.sort_by(|a, b| {
        a.quad[0][1]
            .total_cmp(&b.quad[0][1])
            .then(a.quad[0][0].total_cmp(&b.quad[0][0]))
    });
    let mut i = 0;
    while i < found.len() {
        let y = found[i].quad[0][1];
        let mut end = i + 1;
        while end < found.len() && (found[end].quad[0][1] - y).abs() < 10.0 {
            end += 1;
        }
        found[i..end].sort_by(|a, b| a.quad[0][0].total_cmp(&b.quad[0][0]));
        i = end;
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_two_lines_in_reading_order() {
        let (w, h) = (64, 32);
        let mut map = vec![0.0_f32; w * h];
        // Second line first in memory order would still sort by y then x.
        for y in 20..26 {
            for x in 4..60 {
                map[y * w + x] = 0.9;
            }
        }
        for y in 4..10 {
            for x in 8..40 {
                map[y * w + x] = 0.8;
            }
        }
        let lines = postprocess(&map, w, h, 128, 64).expect("map");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].quad[0][1] < lines[1].quad[0][1]);
        assert!((lines[0].score - 0.8).abs() < 1e-4);
        // Coordinates are scaled to the 128×64 source.
        assert!(lines[1].quad[1][0] > 100.0);
        assert!(postprocess(&map, w, h + 1, 1, 1).is_none());
    }
}
