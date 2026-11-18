package demo

/// A point in two dimensions.
pub struct Point {
    x: f64,
    y: f64,
}

/// A newtype with derived conformance.
#[derive(Eq, Hash, Show)]
pub struct Id {
    value: i64,
}

/// Closed sum type. Exhaustiveness is decided from this package alone.
pub enum Shape {
    Circle { radius: f64 },
    Rect { w: f64, h: f64 },
    Empty,
}

/// Pattern matching with guards, or-patterns, rest patterns and ranges.
pub fn classify(s: Shape) -> string {
    return match s {
        Shape::Circle { radius } if radius > 100.0 => "big circle",
        Shape::Circle { .. } => "circle",
        Shape::Rect { w, h } if w == h => "square",
        Shape::Rect { .. } => "rect",
        Shape::Empty => "empty",
    };
}

pub fn area(s: Shape) -> f64 {
    return match s {
        Shape::Circle { radius } => 3.141592653589793 * radius * radius,
        Shape::Rect { w, h } => w * h,
        Shape::Empty => 0.0,
    };
}

/// A user-defined declarative macro. Expansion is pure token rewriting, and
/// template-introduced bindings are hygienic.
macro pair! {
    ($a:expr, $b:expr) => { [$a, $b] },
}

pub fn pair_len() -> usize {
    let xs = pair![1, 2];
    return xs.len();
}

/// A nominal interface with an associated type.
interface Iterator {
    type Item

    fn next(self: &mut Self) -> Option<Self::Item>
}

pub struct Counter {
    cur: i64,
    end: i64,
}

/// A struct literal used to build the receiver.
pub fn counter(end: i64) -> Counter {
    return Counter { cur: 0, end: end };
}

impl Iterator for Counter {
    type Item = i64

    fn next(self: &mut Self) -> Option<i64> {
        if self.cur >= self.end {
            return None;
        }
        let value = self.cur;
        self.cur += 1;
        return Some(value);
    }
}

pub fn sum_to(n: i64) -> i64 {
    let mut iter = counter(n);
    let mut total: i64 = 0;
    while let Some(value) = iter.next() {
        total += value;
    }
    return total;
}

/// `?` converts the error type through an explicit `From` impl.
pub enum MathError {
    DivideByZero,
}

impl From<string> for MathError {
    fn from(_message: string) -> MathError {
        return MathError::DivideByZero;
    }
}

fn raw_div(a: i64, b: i64) -> Result<i64, string> {
    if b == 0 {
        return Err("division by zero");
    }
    return Ok(a / b);
}

pub fn checked_div(a: i64, b: i64) -> Result<i64, MathError> {
    let value = raw_div(a, b)?;
    return Ok(value);
}

pub fn describe(o: Option<Shape>) -> string {
    return match o {
        Some(Shape::Empty) => "empty",
        Some(Shape::Circle { .. }) => "circle",
        Some(Shape::Rect { .. }) => "rect",
        None => "none",
    };
}
