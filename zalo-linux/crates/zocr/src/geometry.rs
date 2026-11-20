//! Convex hulls, bounding quads and perspective projection for text boxes.

/// A point `[x, y]` in pixels.
pub type Point = [f64; 2];
/// Four corners in reading order: top-left, top-right, bottom-right, bottom-left.
pub type Quad = [Point; 4];

fn cross(a: Point, b: Point, c: Point) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Convex hull (monotone chain), counter-clockwise, no repeated point.
pub fn hull(mut points: Vec<Point>) -> Vec<Point> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    if points.len() <= 2 {
        return points;
    }
    let mut lower: Vec<Point> = Vec::new();
    for &p in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<Point> = Vec::new();
    for &p in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Area and perimeter of a closed polygon.
pub fn area_perimeter(p: &[Point]) -> (f64, f64) {
    let mut area = 0.0;
    let mut perimeter = 0.0;
    for i in 0..p.len() {
        let a = p[i];
        let b = p[(i + 1) % p.len()];
        area += a[0] * b[1] - a[1] * b[0];
        perimeter += distance(a, b);
    }
    (area.abs() * 0.5, perimeter)
}

/// Euclidean distance.
pub fn distance(a: Point, b: Point) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Minimum-area bounding rectangle of a convex polygon, rotated so the most
/// horizontal edge is the reading edge and points rightward.
pub fn min_quad(p: &[Point]) -> Option<Quad> {
    if p.len() < 3 {
        return None;
    }
    let mut best = f64::INFINITY;
    let mut quad = [[0.0; 2]; 4];
    for i in 0..p.len() {
        let a = p[i];
        let b = p[(i + 1) % p.len()];
        let d = distance(a, b);
        if d < 1e-9 {
            continue;
        }
        let u = [(b[0] - a[0]) / d, (b[1] - a[1]) / d];
        let v = [-u[1], u[0]];
        let (mut lo_u, mut hi_u, mut lo_v, mut hi_v) = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for q in p {
            let x = q[0] * u[0] + q[1] * u[1];
            let y = q[0] * v[0] + q[1] * v[1];
            lo_u = lo_u.min(x);
            hi_u = hi_u.max(x);
            lo_v = lo_v.min(y);
            hi_v = hi_v.max(y);
        }
        let size = (hi_u - lo_u) * (hi_v - lo_v);
        if size < best {
            best = size;
            let at = |x: f64, y: f64| [x * u[0] + y * v[0], x * u[1] + y * v[1]];
            quad = [
                at(lo_u, lo_v),
                at(hi_u, lo_v),
                at(hi_u, hi_v),
                at(lo_u, hi_v),
            ];
        }
    }
    if !best.is_finite() || best <= 0.0 {
        return None;
    }
    let horizontal = |i: usize| {
        let a = quad[i];
        let b = quad[(i + 1) % 4];
        (b[0] - a[0]).abs() / distance(a, b)
    };
    let mut edge = 0;
    for i in 1..4 {
        if horizontal(i) > horizontal(edge) {
            edge = i;
        }
    }
    if quad[(edge + 1) % 4][0] < quad[edge][0] {
        edge = (edge + 2) % 4;
    }
    Some(std::array::from_fn(|i| quad[(edge + i) % 4]))
}

/// Unit-square to quadrilateral homography (handles projective skew).
pub fn project(q: Quad, u: f64, v: f64) -> Option<Point> {
    let dx1 = q[1][0] - q[2][0];
    let dx2 = q[3][0] - q[2][0];
    let dx3 = q[0][0] - q[1][0] + q[2][0] - q[3][0];
    let dy1 = q[1][1] - q[2][1];
    let dy2 = q[3][1] - q[2][1];
    let dy3 = q[0][1] - q[1][1] + q[2][1] - q[3][1];
    let (g, h) = if dx3.abs() + dy3.abs() < 1e-9 {
        (0.0, 0.0)
    } else {
        let det = dx1 * dy2 - dx2 * dy1;
        if det.abs() < 1e-9 {
            return None;
        }
        ((dx3 * dy2 - dx2 * dy3) / det, (dx1 * dy3 - dx3 * dy1) / det)
    };
    let den = g * u + h * v + 1.0;
    if den.abs() < 1e-9 {
        return None;
    }
    Some([
        ((q[1][0] - q[0][0] + g * q[1][0]) * u + (q[3][0] - q[0][0] + h * q[3][0]) * v + q[0][0])
            / den,
        ((q[1][1] - q[0][1] + g * q[1][1]) * u + (q[3][1] - q[0][1] + h * q[3][1]) * v + q[0][1])
            / den,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_and_min_quad_of_a_rectangle() {
        let points = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 4.0], [0.0, 4.0], [5.0, 2.0]];
        let h = hull(points);
        assert_eq!(h.len(), 4);
        let (area, perimeter) = area_perimeter(&h);
        assert!((area - 40.0).abs() < 1e-9);
        assert!((perimeter - 28.0).abs() < 1e-9);
        let q = min_quad(&h).expect("quad");
        assert!(q[1][0] > q[0][0], "reading edge points right");
        assert!((distance(q[0], q[1]) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn project_maps_unit_corners() {
        let q = [[1.0, 1.0], [9.0, 1.0], [9.0, 5.0], [1.0, 5.0]];
        assert_eq!(project(q, 0.0, 0.0), Some([1.0, 1.0]));
        assert_eq!(project(q, 1.0, 1.0), Some([9.0, 5.0]));
        let mid = project(q, 0.5, 0.5).expect("mid");
        assert!((mid[0] - 5.0).abs() < 1e-9 && (mid[1] - 3.0).abs() < 1e-9);
    }
}
