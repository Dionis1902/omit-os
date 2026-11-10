// SPDX-License-Identifier: GPL-2.0-only
//! Points and rectangles in pixel coordinates. The origin is the top-left
//! corner; x grows to the right and y downwards. Coordinates are signed so
//! that things can be partly off-screen.

/// A position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    /// Horizontal position.
    pub x: i32,
    /// Vertical position.
    pub y: i32,
}

impl Point {
    /// Creates a point.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A rectangle: the pixels with `x <= px < x + w` and `y <= py < y + h`.
/// A rectangle with zero width or height contains nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

impl Rect {
    /// Creates a rectangle; negative sizes are treated as empty.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            x,
            y,
            w: if w > 0 { w } else { 0 },
            h: if h > 0 { h } else { 0 },
        }
    }

    /// Whether the rectangle contains no pixels.
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// The x coordinate just past the right edge.
    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    /// The y coordinate just past the bottom edge.
    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }

    /// Whether `point` is inside.
    pub const fn contains(&self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// The part common to both rectangles (possibly empty).
    pub fn intersect(&self, other: &Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Self::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    /// The smallest rectangle containing both. An empty rectangle
    /// contributes nothing.
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    /// The rectangle moved by (`dx`, `dy`).
    pub const fn translate(&self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
            w: self.w,
            h: self.h,
        }
    }

    /// The rectangle shrunk by `margin` on every side.
    pub const fn inset(&self, margin: i32) -> Self {
        Self::new(
            self.x.saturating_add(margin),
            self.y.saturating_add(margin),
            self.w.saturating_sub(2 * margin),
            self.h.saturating_sub(2 * margin),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_and_edges() {
        let r = Rect::new(10, 20, 30, 40);
        assert_eq!((r.right(), r.bottom()), (40, 60));
        assert!(r.contains(Point::new(10, 20)));
        assert!(r.contains(Point::new(39, 59)));
        assert!(!r.contains(Point::new(40, 59)));
        assert!(!r.contains(Point::new(39, 60)));
        assert!(!r.contains(Point::new(9, 20)));
        assert!(Rect::new(0, 0, -5, 3).is_empty());
        assert!(!Rect::new(0, 0, 0, 3).contains(Point::new(0, 0)));
    }

    #[test]
    fn intersection_and_union() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        let far = Rect::new(100, 100, 5, 5);
        assert!(a.intersect(&far).is_empty());
        assert_eq!(a.union(&Rect::default()), a);
        assert_eq!(Rect::default().union(&b), b);
        assert_eq!(a.translate(-3, 4), Rect::new(-3, 4, 10, 10));
        assert_eq!(a.inset(2), Rect::new(2, 2, 6, 6));
        assert!(a.inset(6).is_empty());
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        let huge = Rect::new(i32::MAX - 1, i32::MAX - 1, i32::MAX, i32::MAX);
        assert_eq!(huge.right(), i32::MAX);
        let _ = huge.translate(i32::MAX, i32::MAX);
        let _ = huge.union(&Rect::new(i32::MIN, i32::MIN, 1, 1));
        let _ = huge.intersect(&Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX));
    }
}
