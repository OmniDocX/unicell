//! 2D affine transform (2x3 matrix), SVG `matrix(a b c d e f)` layout.

use crate::Point;

/// Affine transform: `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    pub fn rotate_deg(deg: f64) -> Self {
        let r = deg.to_radians();
        let (s, c) = r.sin_cos();
        Self::new(c, s, -s, c, 0.0, 0.0)
    }

    pub fn skew_x_deg(deg: f64) -> Self {
        Self::new(1.0, 0.0, deg.to_radians().tan(), 1.0, 0.0, 0.0)
    }

    pub fn skew_y_deg(deg: f64) -> Self {
        Self::new(1.0, deg.to_radians().tan(), 0.0, 1.0, 0.0, 0.0)
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// Composition `self * other`: apply `other` first, then `self`.
    pub fn mul(&self, o: &Matrix) -> Matrix {
        Matrix {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    /// Apply the transform to a point.
    pub fn apply(&self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
        }
    }

    /// Inverse transform, if the matrix is invertible.
    pub fn invert(&self) -> Option<Matrix> {
        let det = self.a * self.d - self.b * self.c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Matrix {
            a: self.d * inv_det,
            b: -self.b * inv_det,
            c: -self.c * inv_det,
            d: self.a * inv_det,
            e: (self.c * self.f - self.d * self.e) * inv_det,
            f: (self.b * self.e - self.a * self.f) * inv_det,
        })
    }

    /// Average absolute scale factor, used to convert lengths such as
    /// stroke widths between coordinate spaces.
    pub fn mean_scale(&self) -> f64 {
        let sx = (self.a * self.a + self.b * self.b).sqrt();
        let sy = (self.c * self.c + self.d * self.d).sqrt();
        (sx + sy) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_identity() {
        let m = Matrix::new(2.0, 0.0, 0.0, 3.0, 5.0, 7.0);
        assert_eq!(m.mul(&Matrix::IDENTITY), m);
        assert_eq!(Matrix::IDENTITY.mul(&m), m);
    }

    #[test]
    fn translate_then_scale() {
        // scale * translate: translate applied first.
        let m = Matrix::scale(2.0, 2.0).mul(&Matrix::translate(1.0, 1.0));
        let p = m.apply(Point::new(0.0, 0.0));
        assert_eq!(p, Point::new(2.0, 2.0));
    }

    #[test]
    fn invert_roundtrip() {
        let m = Matrix::new(2.0, 1.0, -1.0, 3.0, 5.0, -7.0);
        let inv = m.invert().unwrap();
        let p = Point::new(11.0, 13.0);
        let back = inv.apply(m.apply(p));
        assert!((back.x - p.x).abs() < 1e-12);
        assert!((back.y - p.y).abs() < 1e-12);
    }
}
