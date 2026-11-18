package acme.math

/// A point in two dimensions.
pub struct Point {
    x: i64,
    y: i64,
}

/// Add two integers.
pub fn add(a: i64, b: i64) -> i64 {
    return a + b;
}

/// The origin.
pub fn origin() -> Point {
    return Point { x: 0, y: 0 };
}

/// Manifest a canonical origin constant.
pub const ORIGIN_X: i64 = 0
