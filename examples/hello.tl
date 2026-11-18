package demo

pub struct Point {
    x: f64,
    y: f64,
}

pub enum Shape {
    Circle { radius: f64 },
    Rect { w: f64, h: f64 },
    Empty,
}

pub fn classify(s: Shape) -> string {
    return match s {
        Shape::Circle { radius } => "circle",
        Shape::Rect { w, h } => "rect",
        Shape::Empty => "empty",
    };
}
