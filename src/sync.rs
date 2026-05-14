// SyncTeX endpoints: translate clicks between editor (.tex line/column) and
// PDF (page + coords). Run the `synctex` binary against the cached
// /work/<scope>/output.synctex.gz left there by latexmk's last compile.
//
// Upstream CLSI:
//   GET .../sync/code?file=...&line=N&column=N  → editor → PDF (forward)
//   GET .../sync/pdf?page=N&h=X.X&v=Y.Y         → PDF → editor (reverse)
// Response shape mirrors upstream's:
//   { "code": [{ file, line, column }], "pdf": [{ page, h, v, height, width }] }

use crate::{errors::ApiError, types::*, AppState};
use axum::{
    extract::{Path as AxumPath, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::process::Command;
use tracing::debug;

#[derive(Deserialize)]
pub struct SyncCodeQuery {
    pub file: String,
    pub line: i32,
    #[serde(default)]
    pub column: i32,
    // editorId/buildId accepted for compat, currently unused — we always run
    // against the latest cached output in the work dir.
    #[serde(rename = "editorId", default)]
    pub _editor_id: Option<String>,
    #[serde(rename = "buildId", default)]
    pub _build_id: Option<String>,
}

#[derive(Deserialize)]
pub struct SyncPdfQuery {
    pub page: u32,
    pub h: f64,
    pub v: f64,
    #[serde(rename = "editorId", default)]
    pub _editor_id: Option<String>,
    #[serde(rename = "buildId", default)]
    pub _build_id: Option<String>,
}

#[derive(Serialize)]
pub struct SyncResponse {
    pub code: Vec<CodePosition>,
    pub pdf: Vec<PdfPosition>,
}

#[derive(Serialize)]
pub struct CodePosition {
    pub file: String,
    pub line: i32,
    pub column: i32,
}

#[derive(Serialize)]
pub struct PdfPosition {
    pub page: u32,
    pub h: f64,
    pub v: f64,
    pub height: f64,
    pub width: f64,
}

pub async fn sync_code_no_user(
    State(s): State<Arc<AppState>>,
    AxumPath(pid): AxumPath<String>,
    Query(q): Query<SyncCodeQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    let pid = validate_project_id(&pid)?;
    do_sync_code(&s, &pid, None, q).await
}

pub async fn sync_code_with_user(
    State(s): State<Arc<AppState>>,
    AxumPath((pid, uid)): AxumPath<(String, String)>,
    Query(q): Query<SyncCodeQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    let pid = validate_project_id(&pid)?;
    let uid = validate_user_id(&uid)?;
    do_sync_code(&s, &pid, Some(&uid), q).await
}

pub async fn sync_pdf_no_user(
    State(s): State<Arc<AppState>>,
    AxumPath(pid): AxumPath<String>,
    Query(q): Query<SyncPdfQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    let pid = validate_project_id(&pid)?;
    do_sync_pdf(&s, &pid, None, q).await
}

pub async fn sync_pdf_with_user(
    State(s): State<Arc<AppState>>,
    AxumPath((pid, uid)): AxumPath<(String, String)>,
    Query(q): Query<SyncPdfQuery>,
) -> Result<Json<SyncResponse>, ApiError> {
    let pid = validate_project_id(&pid)?;
    let uid = validate_user_id(&uid)?;
    do_sync_pdf(&s, &pid, Some(&uid), q).await
}

async fn do_sync_code(
    s: &AppState,
    project_id: &str,
    user_id: Option<&str>,
    q: SyncCodeQuery,
) -> Result<Json<SyncResponse>, ApiError> {
    let scope = match user_id {
        Some(uid) => format!("{project_id}-{uid}"),
        None => project_id.to_owned(),
    };
    let work = s.work_root.join(&scope);
    let pdf = work.join("output.pdf");
    if !pdf.exists() {
        return Err(ApiError::BadRequest("no compiled output for scope".into()));
    }

    // synctex view -i <line>:<column>:<file> -o output.pdf
    let mut cmd = Command::new("synctex");
    cmd.current_dir(&work)
        .arg("view")
        .arg("-i")
        .arg(format!("{}:{}:{}", q.line, q.column, q.file))
        .arg("-o")
        .arg("output.pdf");

    debug!(?cmd, "synctex view");
    let out = cmd
        .output()
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("spawn synctex: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);

    Ok(Json(SyncResponse {
        code: vec![],
        pdf: parse_synctex_pdf(&stdout),
    }))
}

async fn do_sync_pdf(
    s: &AppState,
    project_id: &str,
    user_id: Option<&str>,
    q: SyncPdfQuery,
) -> Result<Json<SyncResponse>, ApiError> {
    let scope = match user_id {
        Some(uid) => format!("{project_id}-{uid}"),
        None => project_id.to_owned(),
    };
    let work = s.work_root.join(&scope);
    let pdf = work.join("output.pdf");
    if !pdf.exists() {
        return Err(ApiError::BadRequest("no compiled output for scope".into()));
    }

    // synctex edit -o <page>:<h>:<v>:output.pdf
    let mut cmd = Command::new("synctex");
    cmd.current_dir(&work)
        .arg("edit")
        .arg("-o")
        .arg(format!("{}:{}:{}:output.pdf", q.page, q.h, q.v));

    debug!(?cmd, "synctex edit");
    let out = cmd
        .output()
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("spawn synctex: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);

    Ok(Json(SyncResponse {
        code: parse_synctex_code(&stdout, &work),
        pdf: vec![],
    }))
}

// synctex's text output looks like:
//   SyncTeX result begin
//   Output:output.pdf
//   Page:1
//   x:135.74
//   y:756.16
//   h:42.5
//   v:760.16
//   W:469
//   H:13.7
//   before:
//   offset:-1
//   middle:
//   after:
//   SyncTeX result end
// One block per match. We pull Page/h/v/H/W out of each block.
fn parse_synctex_pdf(stdout: &str) -> Vec<PdfPosition> {
    let mut out = Vec::new();
    let mut cur: Option<PdfPosition> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line == "SyncTeX result begin" || line == "Output:output.pdf" {
            // begin or output marker
            continue;
        }
        if line == "SyncTeX result end" {
            if let Some(p) = cur.take() {
                out.push(p);
            }
            continue;
        }
        let (k, v) = match line.split_once(':') {
            Some(x) => x,
            None => continue,
        };
        let v = v.trim();
        if k == "Page" {
            if let Some(p) = cur.take() {
                out.push(p);
            }
            cur = Some(PdfPosition {
                page: v.parse().unwrap_or(0),
                h: 0.0,
                v: 0.0,
                height: 0.0,
                width: 0.0,
            });
            continue;
        }
        let Some(p) = cur.as_mut() else { continue };
        match k {
            "h" => p.h = v.parse().unwrap_or(0.0),
            "v" => p.v = v.parse().unwrap_or(0.0),
            "H" => p.height = v.parse().unwrap_or(0.0),
            "W" => p.width = v.parse().unwrap_or(0.0),
            _ => {}
        }
    }
    if let Some(p) = cur {
        out.push(p);
    }
    out
}

// synctex edit output:
//   SyncTeX result begin
//   Output:output.pdf
//   Input:./main.tex
//   Line:42
//   Column:3
//   SyncTeX result end
fn parse_synctex_code(stdout: &str, work_dir: &std::path::Path) -> Vec<CodePosition> {
    let mut out = Vec::new();
    let mut input: Option<String> = None;
    let mut line: i32 = 0;
    let mut column: i32 = 0;
    let mut in_block = false;
    for raw in stdout.lines() {
        let l = raw.trim();
        if l == "SyncTeX result begin" {
            in_block = true;
            input = None;
            line = 0;
            column = 0;
            continue;
        }
        if l == "SyncTeX result end" {
            if let Some(f) = input.take() {
                out.push(CodePosition {
                    file: rel_to_work(&f, work_dir),
                    line,
                    column,
                });
            }
            in_block = false;
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(v) = l.strip_prefix("Input:") {
            input = Some(v.trim().to_owned());
        } else if let Some(v) = l.strip_prefix("Line:") {
            line = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("Column:") {
            column = v.trim().parse().unwrap_or(0);
        }
    }
    out
}

// Convert synctex's Input: value (which can be absolute, e.g.
// "/work/<scope>/main.tex", or relative like "./main.tex" or "main.tex")
// into a project-relative path that matches a file in the user's project
// tree.
fn rel_to_work(path: &str, work_dir: &std::path::Path) -> String {
    // Try stripping the work dir prefix (with and without trailing slash).
    let work_str = work_dir.to_string_lossy();
    if let Some(rest) = path.strip_prefix(work_str.as_ref()) {
        return rest.trim_start_matches('/').to_owned();
    }
    // Some latexmk configurations record paths like "./main.tex".
    if let Some(rest) = path.strip_prefix("./") {
        return rest.to_owned();
    }
    path.to_owned()
}
