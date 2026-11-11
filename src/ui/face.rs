use nice_plug::prelude::{Param, ParamSetter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    #[inline(always)]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[inline(always)]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Scale opacity. RGB stays straight; the backend premultiplies when it paints.
    #[inline(always)]
    pub fn fade(self, factor: f32) -> Self {
        Self {
            r: self.r,
            g: self.g,
            b: self.b,
            a: (self.a as f32 * factor + 0.5) as u8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl Rect {
    #[inline(always)]
    pub fn from_min_max(min: Point, max: Point) -> Self {
        Self {
            min_x: min.x,
            min_y: min.y,
            max_x: max.x,
            max_y: max.y,
        }
    }

    #[inline(always)]
    pub fn from_min_size(min: Point, width: f32, height: f32) -> Self {
        Self {
            min_x: min.x,
            min_y: min.y,
            max_x: min.x + width,
            max_y: min.y + height,
        }
    }

    #[inline(always)]
    pub fn left(self) -> f32 {
        self.min_x
    }

    #[inline(always)]
    pub fn top(self) -> f32 {
        self.min_y
    }

    #[inline(always)]
    pub fn bottom(self) -> f32 {
        self.max_y
    }

    #[inline(always)]
    pub fn width(self) -> f32 {
        self.max_x - self.min_x
    }

    #[inline(always)]
    pub fn height(self) -> f32 {
        self.max_y - self.min_y
    }

    #[inline(always)]
    pub fn center(self) -> Point {
        Point {
            x: (self.min_x + self.max_x) * 0.5,
            y: (self.min_y + self.max_y) * 0.5,
        }
    }

    #[inline(always)]
    pub fn shrink(self, amount: f32) -> Self {
        Self {
            min_x: self.min_x + amount,
            min_y: self.min_y + amount,
            max_x: self.max_x - amount,
            max_y: self.max_y - amount,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Breadth {
    Fixed(f32),
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sense {
    Hover,
    Click,
}

/// Bulk paint into one allocated rectangle. Methods are static dispatch.
pub trait Painter {
    fn fill(&mut self, rect: Rect, radius: f32, color: Color);
    fn line(&mut self, from: Point, to: Point, width: f32, color: Color);
    fn text_centered(&mut self, at: Point, text: &str, size: f32, color: Color);

    /// Playhead mark. It moves every step. The default paints it with `fill`.
    /// A backend can keep it out of a cached mesh so the cells under it stay put.
    fn highlight(&mut self, rect: Rect, radius: f32, color: Color) {
        self.fill(rect, radius, color);
    }
}

/// What the editor can draw. A UI library implements this once.
///
/// The paint loop is generic over the backend, so line and fill calls inline
/// into that library. There is no intermediate command list.
pub trait Surface: Sized {
    type Painter<'a>: Painter
    where
        Self: 'a;

    fn heading(&mut self, text: &str);
    fn label(&mut self, text: &str);
    fn hint(&mut self, text: &str, size: f32);
    fn space(&mut self, amount: f32);
    fn button(&mut self, label: &str, enabled: bool) -> bool;
    fn row(&mut self, body: impl FnOnce(&mut Self));
    fn split_two(&mut self, body: impl FnOnce(&mut Self, &mut Self));
    fn text_field(&mut self, text: &mut String, hint: &str, width: f32);
    fn param_slider<P: Param>(&mut self, label: &str, param: &P, setter: &ParamSetter<'_>);
    fn progress(&mut self, fraction: f32, text: &str);
    fn canvas<'a, R>(
        &'a mut self,
        breadth: Breadth,
        height: f32,
        sense: Sense,
        draw: impl FnOnce(Rect, &mut Self::Painter<'a>, Option<Point>) -> R,
    ) -> R;
    fn request_repaint(&mut self);
}

#[cfg(test)]
mod tests {
    use super::Color;

    #[test]
    fn fade_scales_opacity_not_the_ink() {
        let color = Color::rgb(232, 168, 74).fade(0.28);
        assert_eq!((color.r, color.g, color.b), (232, 168, 74));
        assert!(color.a < 255);
    }
}
