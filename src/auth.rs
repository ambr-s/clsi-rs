use crate::errors::ApiError;
use axum::http::HeaderMap;

pub fn check(state: &crate::AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state.auth_token.as_deref() else {
        // No token configured — let it through. Useful for local dev; main.rs
        // logs a warning in this mode.
        return Ok(());
    };
    let got = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
