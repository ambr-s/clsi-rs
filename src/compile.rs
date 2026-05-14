use crate::{auth, errors::ApiError, types::*, AppState};
use axum::{
    extract::{Path as AxumPath, State},
    http::HeaderMap,
    Json,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio::process::Command;
use tracing::{debug, info, warn};

// File names latexmk produces that we want to surface to web. Anything else in
// the work dir is treated as aux state to be reused on the next compile.
const KEEP_OUTPUT_FILES: &[&str] = &[
    "output.pdf",
    "output.log",
    "output.synctex.gz",
    "output.fls",
    "output.stdout",
    "output.stderr",
];

pub async fn compile_no_user(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(pid): AxumPath<String>,
    Json(body): Json<CompileRequest>,
) -> Result<Json<CompileResponse>, ApiError> {
    auth::check(&s, &headers)?;
    let pid = validate_project_id(&pid)?;
    do_compile(&s, &pid, None, body).await
}

pub async fn compile_with_user(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((pid, uid)): AxumPath<(String, String)>,
    Json(body): Json<CompileRequest>,
) -> Result<Json<CompileResponse>, ApiError> {
    auth::check(&s, &headers)?;
    let pid = validate_project_id(&pid)?;
    let uid = validate_user_id(&uid)?;
    do_compile(&s, &pid, Some(&uid), body).await
}

async fn do_compile(
    s: &AppState,
    project_id: &str,
    user_id: Option<&str>,
    req: CompileRequest,
) -> Result<Json<CompileResponse>, ApiError> {
    let scope = match user_id {
        Some(uid) => format!("{project_id}-{uid}"),
        None => project_id.to_owned(),
    };

    // Per-scope lock: serialize concurrent compiles for the same project so
    // they don't fight over the same work dir. Different scopes run in parallel.
    // We wait rather than 423-ing because web's auto-retry adds round-trip
    // latency; a short wait here is strictly faster.
    let lock = s.locks.get(&scope).await;
    let _guard = lock.lock().await;

    let opts = &req.compile.options;
    let compiler = opts.compiler.as_deref().unwrap_or("pdflatex");
    if !matches!(compiler, "pdflatex" | "latex" | "xelatex" | "lualatex") {
        return Err(ApiError::BadRequest(format!("invalid compiler {compiler}")));
    }
    let timeout_secs = opts.timeout.unwrap_or(60).min(600);
    let draft = opts.draft.unwrap_or(false);
    let stop_on_first_error = opts.stop_on_first_error.unwrap_or(false);
    let root = req
        .compile
        .root_resource_path
        .as_deref()
        .unwrap_or("main.tex")
        .to_owned();
    if root.contains("..") || root.starts_with('/') {
        return Err(ApiError::BadRequest("bad rootResourcePath".into()));
    }
    let build_id = opts.build_id.clone().unwrap_or_else(new_build_id);

    let work_dir = s.work_root.join(&scope);
    tokio::fs::create_dir_all(&work_dir).await?;

    // ----- Stage resources -----
    let staging_start = Instant::now();
    write_resources(&work_dir, &req.compile.resources).await?;
    let staging_ms = staging_start.elapsed().as_millis() as u64;

    // ----- Run latexmk -----
    let latex_start = Instant::now();
    let exit_code = run_latexmk(
        &work_dir,
        compiler,
        &root,
        timeout_secs,
        draft,
        stop_on_first_error,
    )
    .await?;
    let latex_ms = latex_start.elapsed().as_millis() as u64;

    // ----- Collect outputs -----
    let upload_start = Instant::now();
    let output_files =
        collect_and_upload(s, &scope, &build_id, &work_dir, project_id, user_id).await?;
    let upload_ms = upload_start.elapsed().as_millis() as u64;

    let pdf_exists = output_files.iter().any(|f| f.path == "output.pdf");
    let status = if pdf_exists {
        "success"
    } else if exit_code == 124 {
        "timedout"
    } else {
        "failure"
    };
    info!(
        scope = scope.as_str(),
        build = build_id.as_str(),
        exit_code,
        latex_ms,
        upload_ms,
        staging_ms,
        status,
        n_outputs = output_files.len(),
        "compile done"
    );

    let mut timings = serde_json::Map::new();
    timings.insert("compileE2E".into(), json!(latex_ms + upload_ms + staging_ms));
    timings.insert("latex".into(), json!(latex_ms));
    timings.insert("staging".into(), json!(staging_ms));
    timings.insert("upload".into(), json!(upload_ms));

    let mut stats = serde_json::Map::new();
    stats.insert("latex-runs".into(), json!(1));

    Ok(Json(CompileResponse {
        compile: CompileResult {
            status: status.into(),
            error: if status == "success" {
                None
            } else {
                Some(format!("latex exit {exit_code}"))
            },
            base_history_version: None,
            stats,
            timings,
            build_id,
            clsi_cache_shard: None,
            output_url_prefix: String::new(),
            output_files,
        },
    }))
}

async fn write_resources(work_dir: &PathBuf, resources: &[Resource]) -> Result<(), ApiError> {
    // Fetch URL-backed resources in parallel; write inline content sequentially
    // (it's small and async overhead exceeds the win for typical .tex sizes).
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| ApiError::Internal(e.into()))?;

    let mut url_jobs = Vec::new();

    for res in resources {
        if res.path.contains("..") || res.path.starts_with('/') {
            return Err(ApiError::BadRequest(format!(
                "bad resource path: {}",
                res.path
            )));
        }
        let dst = work_dir.join(&res.path);
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        match (&res.content, &res.url) {
            (Some(content), _) => {
                tokio::fs::write(&dst, content.as_bytes()).await?;
            }
            (None, Some(url)) => {
                let http = http.clone();
                let url = url.clone();
                let dst = dst.clone();
                url_jobs.push(tokio::spawn(async move {
                    let bytes = http
                        .get(&url)
                        .send()
                        .await
                        .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?
                        .error_for_status()
                        .map_err(|e| anyhow::anyhow!("fetch {url} status: {e}"))?
                        .bytes()
                        .await
                        .map_err(|e| anyhow::anyhow!("fetch {url} body: {e}"))?;
                    tokio::fs::write(&dst, &bytes).await?;
                    Ok::<_, anyhow::Error>(())
                }));
            }
            (None, None) => {
                return Err(ApiError::BadRequest(format!(
                    "resource {} has neither content nor url",
                    res.path
                )));
            }
        }
    }
    for j in url_jobs {
        j.await.map_err(|e| ApiError::Internal(e.into()))??;
    }
    Ok(())
}

async fn run_latexmk(
    work_dir: &PathBuf,
    compiler: &str,
    root: &str,
    timeout_secs: u64,
    draft: bool,
    stop_on_first_error: bool,
) -> Result<i32, ApiError> {
    // latexmk handles the multi-pass loop (bibtex / rerun / etc.). We invoke
    // it once and let it iterate; aux files persist in work_dir across calls
    // so steady-state compiles are single-pass.
    let engine_flag = match compiler {
        "pdflatex" => "-pdf",
        "latex" => "-dvi",
        "xelatex" => "-xelatex",
        "lualatex" => "-lualatex",
        _ => unreachable!("validated upstream"),
    };

    let mut cmd = Command::new("latexmk");

    // Sandboxing: strip secrets from the env we hand to latexmk and its
    // descendants (pdflatex/lualatex/bibtex etc.). A user's malicious .tex
    // can otherwise read them via e.g. `\directlua{os.getenv(...)}` and write
    // them into output.pdf or output.log.
    for key in [
        "CLSI_SHARED_AUTH",
        "R2_ENDPOINT",
        "R2_BUCKET",
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
        // aws-sdk-s3 also reads these from the default credential chain.
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    ] {
        cmd.env_remove(key);
    }

    cmd.current_dir(work_dir)
        // Ignore any user-supplied .latexmkrc — otherwise a project's rc file
        // could re-enable shell-escape, change engines, or run arbitrary Perl.
        .arg("-norc")
        .arg("-cd")
        .arg("-jobname=output")
        .arg("-auxdir=.")
        .arg("-outdir=.")
        .arg("-synctex=1")
        .arg("-interaction=batchmode")
        // Force every engine off shell-escape. -no-shell-escape overrides
        // texmf.cnf's `shell_escape` setting unconditionally.
        //
        // lualatex note: we deliberately don't pass --safer here because it
        // breaks luaotfload (a transitively-required package for most fonts).
        // The defense for lua-based env leaks is the env_remove block above —
        // os.getenv("R2_SECRET_ACCESS_KEY") returns nil because we stripped it
        // before latexmk's spawn. os.execute still works inside the rootless
        // gVisor sandbox, but it has nothing useful to run since secrets are
        // gone and the container's user has no extra privileges.
        .arg("-pdflatex=pdflatex -no-shell-escape %O %S")
        .arg("-lualatex=lualatex -no-shell-escape %O %S")
        .arg("-xelatex=xelatex -no-shell-escape %O %S")
        .arg(engine_flag);

    if stop_on_first_error {
        cmd.arg("-halt-on-error");
    } else {
        cmd.arg("-f");
    }
    if draft {
        cmd.arg("-r")
            .arg("/dev/stdin"); // unused; placeholder if we ever want a draft rc file
    }
    cmd.arg(root);

    let stdout_path = work_dir.join("output.stdout");
    let stderr_path = work_dir.join("output.stderr");
    let stdout_file = std::fs::File::create(&stdout_path)?;
    let stderr_file = std::fs::File::create(&stderr_path)?;
    cmd.stdout(stdout_file).stderr(stderr_file);

    debug!(?cmd, "spawning latexmk");
    let mut child = cmd.spawn().map_err(|e| {
        warn!(error = ?e, "failed to spawn latexmk");
        ApiError::Internal(anyhow::anyhow!("spawn latexmk: {e}"))
    })?;

    let wait_with_timeout =
        tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait());
    match wait_with_timeout.await {
        Ok(Ok(status)) => Ok(status.code().unwrap_or(-1)),
        Ok(Err(e)) => Err(ApiError::Internal(e.into())),
        Err(_) => {
            // Timed out. Kill and report 124 (matching GNU timeout convention).
            let _ = child.kill().await;
            Ok(124)
        }
    }
}

async fn collect_and_upload(
    s: &AppState,
    scope: &str,
    build_id: &str,
    work_dir: &PathBuf,
    project_id: &str,
    user_id: Option<&str>,
) -> Result<Vec<OutputFile>, ApiError> {
    let mut out = Vec::new();
    // Critical path: output.pdf must finish uploading before we return — the
    // browser fetches it immediately on response receipt.
    let mut blocking_jobs: Vec<
        std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>,
    > = Vec::new();

    for name in KEEP_OUTPUT_FILES {
        let path = work_dir.join(name);
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        let bytes = tokio::fs::read(&path).await?;
        let key = format!("project/{scope}/build/{build_id}/output/{name}");
        let content_type = mime_guess::from_path(name)
            .first_or_octet_stream()
            .to_string();

        let url = match user_id {
            Some(uid) => format!(
                "{}/project/{}/user/{}/build/{}/output/{}",
                s.output_url_base, project_id, uid, build_id, name
            ),
            None => format!(
                "{}/project/{}/build/{}/output/{}",
                s.output_url_base, project_id, build_id, name
            ),
        };

        let kind = file_kind(name);
        out.push(OutputFile {
            path: (*name).to_owned(),
            url,
            kind,
            build: build_id.to_owned(),
            size: if *name == "output.pdf" {
                Some(size)
            } else {
                None
            },
        });

        if *name == "output.pdf" {
            // Blocking — must be in R2 before we tell the client about its URL.
            blocking_jobs.push(Box::pin(async move {
                s.r2.put(&key, bytes, &content_type).await
            }));
        } else {
            // Background — log/synctex/fls/stdout/stderr are only fetched on
            // demand (log on error, synctex on inverse-search click, etc.). The
            // response lists their URLs but the browser won't hit them
            // immediately, so we have plenty of time to finish in the background.
            let r2 = s.r2.clone();
            tokio::spawn(async move {
                if let Err(e) = r2.put(&key, bytes, &content_type).await {
                    tracing::warn!(error = ?e, key = %key, "background upload failed");
                }
            });
        }
    }
    futures::future::try_join_all(blocking_jobs).await?;

    Ok(out)
}

fn file_kind(name: &str) -> String {
    if let Some(dot) = name.rfind('.') {
        name[dot + 1..].to_owned()
    } else {
        "bin".into()
    }
}
