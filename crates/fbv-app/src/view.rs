//! View transform: world (mils, Y-up, viewed from top) -> screen pixels,
//! with side flip, mirror and 90-degree rotation. All orientation logic
//! lives here and is unit-tested; the canvas only calls through.

use egui::{Pos2, Rect, Vec2};
use fbv_core::Point;

#[derive(Debug, Clone, Copy)]
pub struct View {
    /// World-space pivot everything rotates/flips around (board center).
    pub pivot: Point,
    /// Pan offset in view space (post-flip/rotation world units).
    pub pan: Vec2,
    /// Pixels per mil.
    pub scale: f32,
    /// 0..=3 quarter turns counter-clockwise.
    pub rot: u8,
    /// Viewing the bottom side (renders the board X-flipped, like
    /// physically turning it over).
    pub bottom: bool,
    /// Extra mirror on top of the side flip (OBV's `M`).
    pub mirror: bool,
    /// Set to fit the whole board on the next frame with a known viewport.
    pub fit_pending: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            pivot: Point::new(0.0, 0.0),
            pan: Vec2::ZERO,
            scale: 0.5,
            rot: 0,
            bottom: false,
            mirror: false,
            fit_pending: true,
        }
    }
}

impl View {
    fn flip_x(&self) -> bool {
        self.bottom ^ self.mirror
    }

    /// World -> view space (world units, still Y-up).
    pub fn view_of(&self, p: Point) -> Vec2 {
        let mut x = p.x - self.pivot.x;
        let mut y = p.y - self.pivot.y;
        if self.flip_x() {
            x = -x;
        }
        for _ in 0..self.rot {
            let t = x;
            x = -y;
            y = t;
        }
        Vec2::new(x, y)
    }

    /// View space -> world.
    pub fn world_of_view(&self, v: Vec2) -> Point {
        let mut x = v.x;
        let mut y = v.y;
        // Undo rotation (clockwise turns).
        for _ in 0..self.rot {
            let t = x;
            x = y;
            y = -t;
        }
        if self.flip_x() {
            x = -x;
        }
        Point::new(x + self.pivot.x, y + self.pivot.y)
    }

    pub fn to_screen(&self, p: Point, viewport: Rect) -> Pos2 {
        let v = self.view_of(p);
        let c = viewport.center();
        Pos2::new(
            c.x + (v.x - self.pan.x) * self.scale,
            c.y - (v.y - self.pan.y) * self.scale,
        )
    }

    pub fn view_of_screen(&self, s: Pos2, viewport: Rect) -> Vec2 {
        let c = viewport.center();
        Vec2::new(
            (s.x - c.x) / self.scale + self.pan.x,
            -(s.y - c.y) / self.scale + self.pan.y,
        )
    }

    pub fn world_of_screen(&self, s: Pos2, viewport: Rect) -> Point {
        self.world_of_view(self.view_of_screen(s, viewport))
    }

    /// Pixel radius for a world radius, clamped to stay visible.
    pub fn px(&self, world: f32) -> f32 {
        (world * self.scale).max(1.5)
    }

    pub fn zoom_at(&mut self, screen_pos: Pos2, viewport: Rect, factor: f32) {
        let before = self.view_of_screen(screen_pos, viewport);
        self.scale = (self.scale * factor).clamp(0.001, 500.0);
        let after = self.view_of_screen(screen_pos, viewport);
        self.pan += before - after;
    }

    pub fn pan_screen(&mut self, delta: Vec2) {
        self.pan.x -= delta.x / self.scale;
        self.pan.y += delta.y / self.scale;
    }

    pub fn flip_side(&mut self) {
        self.bottom = !self.bottom;
        self.reflect_pan_x();
    }

    pub fn toggle_mirror(&mut self) {
        self.mirror = !self.mirror;
        self.reflect_pan_x();
    }

    /// Keeps the same board area in view when the X axis flips.
    fn reflect_pan_x(&mut self) {
        // A flip before the rotation negates the view-space X axis when the
        // rotation is 0/180 and the Y axis when it is 90/270.
        if self.rot % 2 == 0 {
            self.pan.x = -self.pan.x;
        } else {
            self.pan.y = -self.pan.y;
        }
    }

    pub fn rotate_ccw(&mut self) {
        self.rot = (self.rot + 1) % 4;
        self.pan = Vec2::new(-self.pan.y, self.pan.x);
    }

    /// Fits a world-space bounding box into the viewport.
    pub fn fit(&mut self, min: Point, max: Point, viewport: Rect) {
        // Transform all four corners to view space to survive rotation.
        let corners = [
            Point::new(min.x, min.y),
            Point::new(max.x, min.y),
            Point::new(max.x, max.y),
            Point::new(min.x, max.y),
        ];
        let vs: Vec<Vec2> = corners.iter().map(|&c| self.view_of(c)).collect();
        let (mut vmin, mut vmax) = (vs[0], vs[0]);
        for v in &vs[1..] {
            vmin = vmin.min(*v);
            vmax = vmax.max(*v);
        }
        let size = vmax - vmin;
        let w = size.x.max(1.0);
        let h = size.y.max(1.0);
        self.pan = (vmin + vmax) / 2.0;
        self.scale = ((viewport.width() / w).min(viewport.height() / h) * 0.9).clamp(0.001, 500.0);
        self.fit_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))
    }

    #[test]
    fn screen_roundtrip_in_every_orientation() {
        for rot in 0..4u8 {
            for bottom in [false, true] {
                for mirror in [false, true] {
                    let v = View {
                        pivot: Point::new(500.0, 400.0),
                        pan: Vec2::new(13.0, -7.0),
                        scale: 0.8,
                        rot,
                        bottom,
                        mirror,
                        fit_pending: false,
                    };
                    let w = Point::new(123.0, 456.0);
                    let s = v.to_screen(w, vp());
                    let back = v.world_of_screen(s, vp());
                    assert!(
                        (back.x - w.x).abs() < 0.01 && (back.y - w.y).abs() < 0.01,
                        "roundtrip failed rot={rot} bottom={bottom} mirror={mirror}: {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn bottom_view_is_mirrored() {
        let mut v = View::default();
        v.pivot = Point::new(0.0, 0.0);
        v.fit(Point::new(-100.0, -100.0), Point::new(100.0, 100.0), vp());
        let right = Point::new(50.0, 0.0);
        let s_top = v.to_screen(right, vp());
        v.flip_side();
        let s_bottom = v.to_screen(right, vp());
        // A point on the right half must appear on the left half after flip.
        assert!(s_top.x > vp().center().x);
        assert!(s_bottom.x < vp().center().x);
        // Mirroring again restores handedness.
        v.toggle_mirror();
        let s_both = v.to_screen(right, vp());
        assert!(s_both.x > vp().center().x);
    }

    #[test]
    fn zoom_keeps_cursor_point() {
        let mut v = View::default();
        v.fit(Point::new(0.0, 0.0), Point::new(1000.0, 800.0), vp());
        let cursor = Pos2::new(200.0, 150.0);
        let before = v.world_of_screen(cursor, vp());
        v.zoom_at(cursor, vp(), 1.5);
        let after = v.world_of_screen(cursor, vp());
        assert!((before.x - after.x).abs() < 0.01);
        assert!((before.y - after.y).abs() < 0.01);
    }

    #[test]
    fn fit_contains_bounds() {
        let mut v = View::default();
        v.rot = 1;
        v.bottom = true;
        v.fit(Point::new(0.0, 0.0), Point::new(2000.0, 500.0), vp());
        for corner in [
            Point::new(0.0, 0.0),
            Point::new(2000.0, 500.0),
            Point::new(0.0, 500.0),
        ] {
            let s = v.to_screen(corner, vp());
            assert!(vp().expand(1.0).contains(s), "{s:?} outside viewport");
        }
    }
}
