//! Model selection and secret handling.
//!
//! The default model is a single constant, overridable by environment variable;
//! the API key is read from the environment at runtime and never hard-coded or
//! committed. The precedence logic lives in pure resolvers ([`resolve_model`],
//! [`resolve_api_key`]) so it is unit-tested without mutating the process
//! environment — the `*_from_env` wrappers just feed them [`std::env::var`].

use crate::error::AiError;

/// The pinned default model id. Overridable at runtime via `EE_MODEL` or
/// `ANTHROPIC_MODEL`; see [`model_from_env`].
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";

/// Environment variable checked first for a model override.
pub const ENV_MODEL_PRIMARY: &str = "EE_MODEL";
/// Environment variable checked second for a model override.
pub const ENV_MODEL_FALLBACK: &str = "ANTHROPIC_MODEL";
/// Environment variable holding the API key.
pub const ENV_API_KEY: &str = "ANTHROPIC_API_KEY";

/// Resolve the model id from the two possible overrides, falling back to
/// [`DEFAULT_MODEL`]. `EE_MODEL` wins over `ANTHROPIC_MODEL`; a value that is
/// empty or only whitespace is treated as unset.
///
/// Pure and deterministic — this is the function under test; [`model_from_env`]
/// is the thin wrapper that reads the real environment.
pub fn resolve_model(ee_model: Option<&str>, anthropic_model: Option<&str>) -> String {
    [ee_model, anthropic_model]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|v| !v.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string()
}

/// Validate a possible API key value: present and non-blank, or
/// [`AiError::MissingApiKey`]. Pure; [`api_key_from_env`] wraps it.
pub fn resolve_api_key(raw: Option<&str>) -> Result<String, AiError> {
    match raw.map(str::trim) {
        Some(key) if !key.is_empty() => Ok(key.to_string()),
        _ => Err(AiError::MissingApiKey),
    }
}

/// The model id to use, consulting `EE_MODEL`, then `ANTHROPIC_MODEL`, then the
/// [`DEFAULT_MODEL`] default.
pub fn model_from_env() -> String {
    let primary = std::env::var(ENV_MODEL_PRIMARY).ok();
    let fallback = std::env::var(ENV_MODEL_FALLBACK).ok();
    resolve_model(primary.as_deref(), fallback.as_deref())
}

/// The API key from `ANTHROPIC_API_KEY`, or [`AiError::MissingApiKey`] if it is
/// absent or blank. The key is returned, never logged.
pub fn api_key_from_env() -> Result<String, AiError> {
    let raw = std::env::var(ENV_API_KEY).ok();
    resolve_api_key(raw.as_deref())
}
