use anyhow::Context as _;
use axum::body::StreamBody;
use axum::extract::{FromRef, Path, State};
use axum::middleware::Next;
use axum::routing::delete;
use axum::Json;
use chrono::NaiveDateTime;
use futures::{SinkExt, StreamExt, TryStreamExt};
use hyper::{Body, Request, StatusCode};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::convert::Infallible;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::io::{CopyToBytes, ReaderStream, SinkWriter};
use tokio_util::sync::CancellationToken;
use tokio_util::sync::PollSender;
use tower_http::trace::DefaultOnResponse;
use url::Url;

use crate::auth::parse_jwt_keys;
use crate::connection::config::{DatabaseConfig, DurabilityMode};
use crate::error::{Error, LoadDumpError};
use crate::hrana;
use crate::namespace::{DumpStream, NamespaceName, NamespaceStore, RestoreOption};
use crate::net::Connector;
use crate::SQLANYWHERE_PAGE_SIZE;

pub mod stats;

#[derive(Clone)]
struct Metrics {
    handle: Option<PrometheusHandle>,
}

impl Metrics {
    fn render(&self) -> String {
        self.handle.as_ref().map(|h| h.render()).unwrap_or_default()
    }
}

struct AppState<C> {
    namespaces: NamespaceStore,
    user_http_server: Arc<hrana::http::Server>,
    connector: C,
    metrics: Metrics,
    set_env_filter: Option<Box<dyn Fn(&str) -> anyhow::Result<()> + Sync + Send + 'static>>,
}

impl<C> FromRef<Arc<AppState<C>>> for Metrics {
    fn from_ref(input: &Arc<AppState<C>>) -> Self {
        input.metrics.clone()
    }
}

static PROM_HANDLE: Mutex<OnceCell<PrometheusHandle>> = Mutex::new(OnceCell::new());

pub async fn run<A, C>(
    acceptor: A,
    user_http_server: Arc<hrana::http::Server>,
    namespaces: NamespaceStore,
    connector: C,
    disable_metrics: bool,
    shutdown: CancellationToken,
    drain: std::time::Duration,
    auth: Option<Arc<str>>,
    set_env_filter: Option<Box<dyn Fn(&str) -> anyhow::Result<()> + Sync + Send + 'static>>,
) -> anyhow::Result<()>
where
    A: crate::net::Accept,
    C: Connector,
{
    let app_label = std::env::var("SQLD_APP_LABEL").ok();
    let ver = env!("CARGO_PKG_VERSION");

    let prom_handle = if !disable_metrics {
        let lock = PROM_HANDLE.lock();
        let prom_handle = lock.get_or_init(|| {
            tracing::info!("initializing prometheus metrics");
            let b = PrometheusBuilder::new().idle_timeout(
                metrics_util::MetricKindMask::ALL,
                Some(Duration::from_secs(120)),
            );

            if let Some(app_label) = app_label {
                b.add_global_label("app", app_label)
                    .add_global_label("version", ver)
                    .install_recorder()
                    .unwrap()
            } else {
                b.install_recorder().unwrap()
            }
        });

        tokio::task::spawn(async move {
            loop {
                let runtime = tokio::runtime::Handle::current();
                let metrics = runtime.metrics();
                crate::metrics::TOKIO_RUNTIME_BLOCKING_QUEUE_DEPTH
                    .set(metrics.blocking_queue_depth() as f64);
                crate::metrics::TOKIO_RUNTIME_INJECTION_QUEUE_DEPTH
                    .set(metrics.injection_queue_depth() as f64);
                crate::metrics::TOKIO_RUNTIME_NUM_BLOCKING_THREADS
                    .set(metrics.num_blocking_threads() as f64);
                crate::metrics::TOKIO_RUNTIME_NUM_IDLE_BLOCKING_THREADS
                    .set(metrics.num_idle_blocking_threads() as f64);
                crate::metrics::TOKIO_RUNTIME_NUM_WORKERS.set(metrics.num_workers() as f64);

                crate::metrics::TOKIO_RUNTIME_IO_DRIVER_FD_DEREGISTERED_COUNT
                    .absolute(metrics.io_driver_fd_deregistered_count() as u64);
                crate::metrics::TOKIO_RUNTIME_IO_DRIVER_FD_REGISTERED_COUNT
                    .absolute(metrics.io_driver_fd_registered_count() as u64);
                crate::metrics::TOKIO_RUNTIME_IO_DRIVER_READY_COUNT
                    .absolute(metrics.io_driver_ready_count() as u64);
                crate::metrics::TOKIO_RUNTIME_REMOTE_SCHEDULE_COUNT
                    .absolute(metrics.remote_schedule_count() as u64);

                crate::metrics::SERVER_COUNT.set(1.0);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });

        Some(prom_handle.clone())
    } else {
        None
    };

    fn trace_request<B>(req: &Request<B>, span: &tracing::Span) {
        let _s = span.enter();

        tracing::debug!("{} {} {:?}", req.method(), req.uri(), req.headers());
    }

    metrics::increment_counter!("sqlanywhere_server_count");

    use axum::routing::{get, post};
    let metrics = Metrics {
        handle: prom_handle,
    };
    let router = axum::Router::new()
        .route("/", get(handle_get_index))
        .route(
            "/v1/namespaces/:namespace/config",
            get(handle_get_config).post(handle_post_config),
        )
        .route(
            "/v1/namespaces/:namespace/fork/:to",
            post(handle_fork_namespace),
        )
        .route("/v1/namespaces/:namespace/at", post(handle_namespace_at))
        .route(
            "/v1/namespaces/:namespace/branch/:branch",
            post(handle_namespace_branch),
        )
        .route(
            "/v1/namespaces/:namespace/create",
            post(handle_create_namespace),
        )
        .route(
            "/v1/namespaces/:namespace/checkpoint",
            post(handle_checkpoint),
        )
        .route("/v1/namespaces/:namespace", delete(handle_delete_namespace))
        .route("/v1/namespaces/:namespace/stats", get(stats::handle_stats))
        .route(
            "/v1/namespaces/:namespace/stats/:stats_type",
            delete(stats::handle_delete_stats),
        )
        .route("/v1/diagnostics", get(handle_diagnostics))
        .route("/metrics", get(handle_metrics))
        .route("/profile/heap/enable", post(enable_profile_heap))
        .route("/profile/heap/disable/:id", post(disable_profile_heap))
        .route("/profile/heap/:id", delete(delete_profile_heap))
        .route("/log-filter", post(handle_set_log_filter))
        .with_state(Arc::new(AppState {
            namespaces: namespaces.clone(),
            connector,
            user_http_server,
            metrics,
            set_env_filter,
        }))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .on_request(trace_request)
                .on_response(
                    DefaultOnResponse::new()
                        .level(tracing::Level::DEBUG)
                        .latency_unit(tower_http::LatencyUnit::Micros),
                ),
        );

    let admin_shell = crate::admin_shell::make_svc(namespaces.clone());
    let grpc_router = tonic::transport::Server::builder()
        .accept_http1(true)
        .add_service(tonic_web::enable(admin_shell))
        .into_router();

    let router = router
        .merge(grpc_router)
        .layer(axum::middleware::from_fn_with_state(auth, auth_middleware));

    let server = hyper::server::Server::builder(acceptor)
        .serve(router.into_make_service())
        .with_graceful_shutdown({
            let shutdown = shutdown.clone();
            async move { shutdown.cancelled().await }
        });
    crate::serve_with_bounded_drain(server, shutdown, drain)
        .await
        .context("Could not bind admin HTTP API server")?;

    Ok(())
}

async fn auth_middleware<B>(
    State(auth): State<Option<Arc<str>>>,
    request: Request<B>,
    next: Next<B>,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(ref auth) = auth {
        let Some(auth_header) = request.headers().get("authorization") else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Ok(auth_str) = std::str::from_utf8(auth_header.as_bytes()) else {
            return Err(StatusCode::UNAUTHORIZED);
        };

        let mut split = auth_str.split_whitespace();
        match split.next() {
            Some(s) if s.trim().eq_ignore_ascii_case("basic") => (),
            _ => return Err(StatusCode::UNAUTHORIZED),
        }

        match split.next() {
            Some(s) if s.trim() == auth.as_ref() => (),
            _ => return Err(StatusCode::UNAUTHORIZED),
        }
    }

    Ok(next.run(request).await)
}

async fn handle_get_index() -> &'static str {
    "Welcome to the sqld admin API"
}

async fn handle_metrics(State(metrics): State<Metrics>) -> String {
    metrics.render()
}

async fn handle_get_config<C: Connector>(
    State(app_state): State<Arc<AppState<C>>>,
    Path(namespace): Path<String>,
) -> crate::Result<Json<HttpDatabaseConfig>> {
    let store = app_state
        .namespaces
        .config_store(NamespaceName::from_string(namespace)?)
        .await?;
    let config = store.get();
    let max_db_size = bytesize::ByteSize::b(config.max_db_pages * SQLANYWHERE_PAGE_SIZE);
    let resp = HttpDatabaseConfig {
        block_reads: config.block_reads,
        block_writes: config.block_writes,
        block_reason: config.block_reason.clone(),
        max_db_size: Some(max_db_size),
        heartbeat_url: config.heartbeat_url.clone().map(|u| u.into()),
        jwt_key: config.jwt_key.clone(),
        allow_attach: config.allow_attach,
        txn_timeout_s: config.txn_timeout.map(|d| d.as_secs() as u64),
        durability_mode: Some(config.durability_mode),
    };
    Ok(Json(resp))
}

async fn handle_diagnostics<C>(
    State(app_state): State<Arc<AppState<C>>>,
) -> crate::Result<Json<Vec<String>>> {
    use crate::connection::Connection;
    use hrana::http::stream;

    let server = app_state.user_http_server.as_ref();
    let stream_state = server.stream_state().lock();
    let handles = stream_state.handles();
    let mut diagnostics: Vec<String> = Vec::with_capacity(handles.len());
    for handle in handles.values() {
        let handle_info: String = match handle {
            stream::Handle::Available(stream) => match &stream.db {
                Some(db) => db.diagnostics(),
                None => "[BUG] available-but-closed".into(),
            },
            stream::Handle::Acquired => "acquired".into(),
            stream::Handle::Expired => "expired".into(),
        };
        diagnostics.push(handle_info);
    }
    drop(stream_state);

    tracing::trace!("diagnostics: {diagnostics:?}");
    Ok(Json(diagnostics))
}

#[derive(Debug, Deserialize, Serialize)]
struct HttpDatabaseConfig {
    block_reads: bool,
    block_writes: bool,
    #[serde(default)]
    block_reason: Option<String>,
    #[serde(default)]
    max_db_size: Option<bytesize::ByteSize>,
    #[serde(default)]
    heartbeat_url: Option<String>,
    #[serde(default)]
    jwt_key: Option<String>,
    #[serde(default)]
    allow_attach: bool,
    #[serde(default)]
    txn_timeout_s: Option<u64>,
    #[serde(default)]
    durability_mode: Option<DurabilityMode>,
}

async fn handle_post_config<C>(
    State(app_state): State<Arc<AppState<C>>>,
    Path(namespace): Path<String>,
    Json(req): Json<HttpDatabaseConfig>,
) -> crate::Result<()> {
    if let Some(jwt_key) = req.jwt_key.as_deref() {
        // Check that the jwt keys are correct
        parse_jwt_keys(jwt_key)?;
    }
    let store = app_state
        .namespaces
        .config_store(NamespaceName::from_string(namespace.clone())?)
        .await?;
    let original = (*store.get()).clone();
    let mut updated = original.clone();
    updated.block_reads = req.block_reads;
    updated.block_writes = req.block_writes;
    updated.block_reason = req.block_reason;
    updated.allow_attach = req.allow_attach;
    updated.txn_timeout = req.txn_timeout_s.map(Duration::from_secs);
    if let Some(size) = req.max_db_size {
        updated.max_db_pages = size.as_u64() / SQLANYWHERE_PAGE_SIZE;
    }
    if let Some(url) = req.heartbeat_url {
        updated.heartbeat_url = Some(Url::parse(&url)?);
    }
    updated.jwt_key = req.jwt_key;
    if let Some(mode) = req.durability_mode {
        updated.durability_mode = mode;
    }

    store.store(updated.clone()).await?;
    // we better to not log jwt token - so let's explicitly log necessary fields
    tracing::info!(
        message = "updated db config",
        namespace = namespace,
        block_writes_before = original.block_writes,
        block_writes_after = updated.block_writes,
        block_reads_before = original.block_reads,
        block_reads_after = updated.block_reads,
        allow_attach_before = original.allow_attach,
        allow_attach_after = updated.allow_attach,
        max_db_pages_before = original.max_db_pages,
        max_db_pages_after = updated.max_db_pages,
        durability_mode_before = original.durability_mode.to_string(),
        durability_mode_after = updated.durability_mode.to_string(),
    );

    Ok(())
}

#[derive(Debug, Deserialize)]
struct CreateNamespaceReq {
    dump_url: Option<Url>,
    max_db_size: Option<bytesize::ByteSize>,
    heartbeat_url: Option<String>,
    bottomless_db_id: Option<String>,
    jwt_key: Option<String>,
    txn_timeout_s: Option<u64>,
    max_row_size: Option<u64>,
    /// If true, current namespace acts as a DB used solely for multi-db schema updates.
    #[serde(default)]
    shared_schema: bool,
    /// If some, this is a [NamespaceName] reference to a shared schema DB.
    #[serde(default)]
    shared_schema_name: Option<NamespaceName>,
    #[serde(default)]
    allow_attach: bool,
    #[serde(default)]
    durability_mode: Option<DurabilityMode>,
}

async fn handle_create_namespace<C: Connector>(
    State(app_state): State<Arc<AppState<C>>>,
    Path(namespace): Path<NamespaceName>,
    Json(req): Json<CreateNamespaceReq>,
) -> crate::Result<()> {
    let mut config = DatabaseConfig::default();

    if let Some(jwt_key) = req.jwt_key {
        // Check that the jwt keys are correct
        parse_jwt_keys(&jwt_key)?;
        config.jwt_key = Some(jwt_key);
    }

    if req.shared_schema_name.is_some() && req.dump_url.is_some() {
        return Err(Error::SharedSchemaUsageError(
            "database using shared schema database cannot be created from a dump".to_string(),
        ));
    }

    if let Some(ns) = req.shared_schema_name {
        if req.shared_schema {
            return Err(Error::SharedSchemaCreationError(
                "shared schema database cannot reference another shared schema".to_string(),
            ));
        }
        // TODO: move this check into meta store
        if !app_state.namespaces.exists(&ns).await {
            return Err(Error::NamespaceDoesntExist(ns.to_string()));
        }

        config.shared_schema_name = Some(ns);
    }

    let dump = match req.dump_url {
        Some(ref url) => {
            RestoreOption::Dump(dump_stream_from_url(url, app_state.connector.clone()).await?)
        }
        None => RestoreOption::Latest,
    };

    config.bottomless_db_id = req.bottomless_db_id;
    config.is_shared_schema = req.shared_schema;
    config.heartbeat_url = req.heartbeat_url.as_deref().map(Url::parse).transpose()?;
    config.txn_timeout = req.txn_timeout_s.map(Duration::from_secs);
    config.max_row_size = req.max_row_size.unwrap_or(config.max_row_size);
    config.allow_attach = req.allow_attach;
    if let Some(max_db_size) = req.max_db_size {
        config.max_db_pages = max_db_size.as_u64() / SQLANYWHERE_PAGE_SIZE;
    }
    config.durability_mode = req.durability_mode.unwrap_or(DurabilityMode::default());

    app_state.namespaces.create(namespace, dump, config).await?;

    Ok(())
}

#[derive(Debug, Deserialize)]
struct ForkNamespaceReq {
    timestamp: NaiveDateTime,
}

async fn handle_fork_namespace<C>(
    State(app_state): State<Arc<AppState<C>>>,
    Path((from, to)): Path<(String, String)>,
    req: Option<Json<ForkNamespaceReq>>,
) -> crate::Result<()> {
    let timestamp = req.map(|v| v.timestamp);
    let from = NamespaceName::from_string(from)?;
    let to = NamespaceName::from_string(to)?;
    let from_store = app_state.namespaces.config_store(from.clone()).await?;
    let from_config = from_store.get();
    if from_config.is_shared_schema {
        return Err(Error::SharedSchemaUsageError(
            "database cannot be forked from a shared schema".to_string(),
        ));
    }
    // A fork is kept until deleted, even when its source expires.
    let to_config = DatabaseConfig {
        expires_at: None,
        ..(*from_config).clone()
    };
    app_state
        .namespaces
        .fork(from, to, to_config, timestamp)
        .await?;

    Ok(())
}

#[derive(Debug, Deserialize)]
struct NamespaceAtReq {
    timestamp: NaiveDateTime,
    /// Delete the namespace this many seconds after it is made.
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
struct NamespaceAtResp {
    namespace: String,
}

/// The name of the read-only view of `namespace` as it was at `timestamp`,
/// such as `orders@20261001T120000Z`. Deterministic, so asking for the same
/// moment twice finds the first one.
fn namespace_at_name(namespace: &str, timestamp: NaiveDateTime) -> String {
    format!("{namespace}@{}", timestamp.format("%Y%m%dT%H%M%SZ"))
}

/// `POST /v1/namespaces/:namespace/at` with `{"timestamp": "..."}`: make the
/// namespace as it was at that moment (UTC) available as a read-only namespace
/// that can be attached, restored from the bottomless backup.
///
/// From the live database: `ATTACH "orders@20261001T120000Z" AS past`, then
/// query `past.*` beside `main.*`. Repeating the request returns the existing
/// namespace, with the expiry it was made with. With `ttl_seconds` the primary
/// deletes it, and its backups, that long after it is made; without, delete
/// it like any other when done.
async fn handle_namespace_at<C>(
    State(app_state): State<Arc<AppState<C>>>,
    Path(namespace): Path<String>,
    Json(req): Json<NamespaceAtReq>,
) -> crate::Result<Json<NamespaceAtResp>> {
    let name = namespace_at_name(&namespace, req.timestamp);
    let from = NamespaceName::from_string(namespace)?;
    let to = NamespaceName::from_string(name.clone())?;
    if app_state.namespaces.exists(&to).await {
        return Ok(Json(NamespaceAtResp { namespace: name }));
    }
    let from_store = app_state.namespaces.config_store(from.clone()).await?;
    let from_config = from_store.get();
    if from_config.is_shared_schema {
        return Err(Error::SharedSchemaUsageError(
            "database cannot be forked from a shared schema".to_string(),
        ));
    }
    let to_config = DatabaseConfig {
        allow_attach: true,
        block_writes: true,
        block_reason: Some(format!(
            "read-only: {from} as of {} UTC",
            req.timestamp.format("%Y-%m-%d %H:%M:%S")
        )),
        expires_at: req.ttl_seconds.map(|ttl| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |now| now.as_secs())
                + ttl
        }),
        ..(*from_config).clone()
    };
    match app_state
        .namespaces
        .fork(from, to, to_config, Some(req.timestamp))
        .await
    {
        // Lost a race with an identical request: the namespace is there.
        Ok(()) | Err(Error::NamespaceAlreadyExist(_)) => {}
        Err(e) => return Err(e),
    }
    Ok(Json(NamespaceAtResp { namespace: name }))
}

#[derive(Debug, Serialize)]
struct NamespaceBranchResp {
    branch: String,
    base: String,
}

/// `POST /v1/namespaces/:namespace/branch/:branch`: two forks of the namespace
/// for a three-way merge. `{branch}@base` is the namespace as it is now,
/// read-only; `{branch}` starts from that same state and is writable. Both can
/// be attached, so the live namespace can compare and merge them
/// (`sqlanywhere::merge3`). The branch is forked from the base, not from the
/// live namespace, so a write landing in between cannot set them apart.
/// Delete both when done.
async fn handle_namespace_branch<C>(
    State(app_state): State<Arc<AppState<C>>>,
    Path((namespace, branch)): Path<(String, String)>,
) -> crate::Result<Json<NamespaceBranchResp>> {
    let base = format!("{branch}@base");
    let from = NamespaceName::from_string(namespace)?;
    let base_ns = NamespaceName::from_string(base.clone())?;
    let branch_ns = NamespaceName::from_string(branch.clone())?;
    let from_store = app_state.namespaces.config_store(from.clone()).await?;
    let from_config = from_store.get();
    if from_config.is_shared_schema {
        return Err(Error::SharedSchemaUsageError(
            "database cannot be forked from a shared schema".to_string(),
        ));
    }
    let base_config = DatabaseConfig {
        allow_attach: true,
        block_writes: true,
        block_reason: Some(format!("read-only: the base of branch {branch} of {from}")),
        expires_at: None,
        ..(*from_config).clone()
    };
    let branch_config = DatabaseConfig {
        allow_attach: true,
        expires_at: None,
        ..(*from_config).clone()
    };
    app_state
        .namespaces
        .fork(from, base_ns.clone(), base_config, None)
        .await?;
    if let Err(e) = app_state
        .namespaces
        .fork(base_ns.clone(), branch_ns, branch_config, None)
        .await
    {
        // Leave nothing half made.
        if let Err(cleanup) = app_state.namespaces.destroy(base_ns, true).await {
            tracing::error!("failed to remove {base} after a failed branch: {cleanup}");
        }
        return Err(e);
    }
    Ok(Json(NamespaceBranchResp { branch, base }))
}

async fn dump_stream_from_url<C>(url: &Url, connector: C) -> Result<DumpStream, LoadDumpError>
where
    C: Connector,
{
    match url.scheme() {
        "http" | "https" => {
            let client = hyper::client::Client::builder().build::<_, Body>(connector);
            let uri = url
                .as_str()
                .parse()
                .map_err(|_| LoadDumpError::InvalidDumpUrl)?;
            let resp = client.get(uri).await?;
            let body = resp
                .into_body()
                .map_err(|e| std::io::Error::new(ErrorKind::Other, e));
            Ok(Box::new(body))
        }
        "file" => {
            let path = PathBuf::from(url.path());
            if !path.is_absolute() {
                return Err(LoadDumpError::DumpFilePathNotAbsolute);
            }

            if !path.try_exists()? {
                return Err(LoadDumpError::DumpFileDoesntExist);
            }

            if !path.is_file() {
                return Err(LoadDumpError::NotAFile);
            }

            let f = tokio::fs::File::open(path).await?;

            Ok(Box::new(ReaderStream::new(f)))
        }
        scheme => Err(LoadDumpError::UnsupportedUrlScheme(scheme.to_string())),
    }
}

#[derive(Deserialize, Default)]
struct DeleteNamespaceReq {
    #[serde(default)]
    pub keep_backup: bool,
}

async fn handle_delete_namespace<C>(
    State(app_state): State<Arc<AppState<C>>>,
    Path(namespace): Path<String>,
    payload: Option<Json<DeleteNamespaceReq>>,
) -> crate::Result<()> {
    let prune_all = match payload {
        Some(req) => !req.keep_backup,
        None => true,
    };

    app_state
        .namespaces
        .destroy(NamespaceName::from_string(namespace)?, prune_all)
        .await?;
    Ok(())
}

async fn handle_set_log_filter<C>(
    State(app_state): State<Arc<AppState<C>>>,
    body: String,
) -> crate::Result<()> {
    if let Some(ref cb) = app_state.set_env_filter {
        cb(&body)?;
    }
    Ok(())
}

async fn handle_checkpoint<C>(
    State(app_state): State<Arc<AppState<C>>>,
    Path(namespace): Path<NamespaceName>,
) -> crate::Result<()> {
    app_state.namespaces.checkpoint(namespace).await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct EnableHeapProfileRequest {
    #[serde(default)]
    max_stack_depth: Option<usize>,
    #[serde(default)]
    max_trackers: Option<usize>,
    #[serde(default)]
    tracker_event_buffer_size: Option<usize>,
    #[serde(default)]
    sample_rate: Option<f64>,
}

async fn enable_profile_heap(Json(req): Json<EnableHeapProfileRequest>) -> crate::Result<String> {
    let path = tokio::task::spawn_blocking(move || {
        rheaper::enable_tracking(rheaper::TrackerConfig {
            max_stack_depth: req.max_stack_depth.unwrap_or(30),
            max_trackers: req.max_trackers.unwrap_or(200),
            tracker_event_buffer_size: req.tracker_event_buffer_size.unwrap_or(5_000),
            sample_rate: req.sample_rate.unwrap_or(1.0),
            profile_dir: PathBuf::from("heap_profile"),
        })
        .map_err(|e| crate::Error::Anyhow(anyhow::anyhow!("{e}")))
    })
    .await??;

    Ok(path.file_name().unwrap().to_str().unwrap().to_string())
}

async fn disable_profile_heap(Path(profile): Path<String>) -> impl axum::response::IntoResponse {
    let (tx, rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
    tokio::task::spawn_blocking(move || {
        rheaper::disable_tracking();
        let profile_dir = PathBuf::from("heap_profile").join(&profile);
        let sink =
            PollSender::new(tx).sink_map_err(|_| std::io::Error::from(ErrorKind::BrokenPipe));
        let writer = tokio_util::io::SyncIoBridge::new(SinkWriter::new(CopyToBytes::new(sink)));
        let mut builder = tar::Builder::new(writer);
        if let Err(e) = builder.append_dir_all(&profile, &profile_dir) {
            tracing::error!("io error sending trace: {e}");
            return;
        }
        if let Err(e) = builder.finish() {
            tracing::error!("io error sending trace: {e}");
            return;
        }
    });

    let stream =
        tokio_stream::wrappers::ReceiverStream::new(rx).map(|b| Result::<_, Infallible>::Ok(b));
    let body = StreamBody::new(stream);

    body
}

async fn delete_profile_heap(Path(profile): Path<String>) -> crate::Result<()> {
    let profile_dir = PathBuf::from("heap_profile").join(&profile);
    tokio::fs::remove_dir_all(&profile_dir).await?;
    Ok(())
}
