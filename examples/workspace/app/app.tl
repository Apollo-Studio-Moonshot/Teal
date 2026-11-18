package acme.app

import acme.math

/// Run a small computation against the math package.
pub fn run() -> i64 {
    let p: math::Point = math::origin();
    return math::add(p.x, math::ORIGIN_X);
}

/// A failure type local to this package.
pub enum AppError {
    Negative,
}

fn require_positive(value: i64) -> Result<i64, AppError> {
    if value < 0 {
        return Err(AppError::Negative);
    }
    return Ok(value);
}

pub fn double_positive(value: i64) -> Result<i64, AppError> {
    let checked = require_positive(value)?;
    return Ok(math::add(checked, checked));
}
