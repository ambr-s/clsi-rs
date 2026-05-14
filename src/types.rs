use serde::{Deserialize, Serialize};

// ============================================================================
// Request shape — mirrors RequestParser.js in upstream CE.
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct CompileRequest {
    pub compile: CompileBody,
}

#[derive(Debug, Deserialize)]
pub struct CompileBody {
    #[serde(default)]
    pub options: CompileOptions,
    #[serde(rename = "rootResourcePath")]
    pub root_resource_path: Option<String>,
    #[serde(default)]
    pub resources: Vec<Resource>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CompileOptions {
    pub compiler: Option<String>,    // pdflatex | latex | xelatex | lualatex
    pub timeout: Option<u64>,        // seconds
    pub draft: Option<bool>,
    #[serde(rename = "stopOnFirstError")]
    pub stop_on_first_error: Option<bool>,
    #[serde(rename = "syncType")]
    pub sync_type: Option<String>,
    #[serde(rename = "syncState")]
    pub sync_state: Option<String>,
    #[serde(rename = "editorId")]
    pub editor_id: Option<String>,
    #[serde(rename = "buildId")]
    pub build_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    pub path: String,
    pub content: Option<String>,
    pub url: Option<String>,
}

// ============================================================================
// Response shape — mirrors what CompileController.js sends, and what web's
// ClsiManager._parseOutputFiles + _postToClsi expect.
// ============================================================================

#[derive(Debug, Serialize)]
pub struct CompileResponse {
    pub compile: CompileResult,
}

#[derive(Debug, Serialize)]
pub struct CompileResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "baseHistoryVersion")]
    pub base_history_version: Option<serde_json::Value>,
    pub stats: serde_json::Map<String, serde_json::Value>,
    pub timings: serde_json::Map<String, serde_json::Value>,
    #[serde(rename = "buildId")]
    pub build_id: String,
    #[serde(rename = "clsiCacheShard")]
    pub clsi_cache_shard: Option<serde_json::Value>,
    #[serde(rename = "outputUrlPrefix")]
    pub output_url_prefix: String,
    #[serde(rename = "outputFiles")]
    pub output_files: Vec<OutputFile>,
}

#[derive(Debug, Serialize)]
pub struct OutputFile {
    pub path: String,
    /// Host-less path like /project/<pid>/build/<bid>/output/<file>.
    /// Web reads this via `new URL(file.url).pathname` and routes through its
    /// own _proxyToClsi which (in our fork) presigns an R2 URL.
    pub url: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub build: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

// ============================================================================
// Validation — match CE's regexes so requests we accept match what web sends.
// ============================================================================

const PROJECT_ID_OK: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";
const HEX_OK: &str = "0123456789abcdef";

pub fn validate_project_id(s: &str) -> Result<String, crate::errors::ApiError> {
    if s.is_empty() || s.len() > 64 || s.chars().any(|c| !PROJECT_ID_OK.contains(c)) {
        return Err(crate::errors::ApiError::BadRequest(
            "invalid project id".into(),
        ));
    }
    Ok(s.to_owned())
}

pub fn validate_user_id(s: &str) -> Result<String, crate::errors::ApiError> {
    // Mongo ObjectId: 24 hex chars
    if s.len() != 24 || s.chars().any(|c| !HEX_OK.contains(c)) {
        return Err(crate::errors::ApiError::BadRequest("invalid user id".into()));
    }
    Ok(s.to_owned())
}

pub fn new_build_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    let (a, b) = bytes.split_at(6);
    format!("{}-{}", hex::encode(a), hex::encode(b))
}
