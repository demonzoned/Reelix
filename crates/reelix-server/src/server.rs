//! Process lifecycle: startup sequence, serving and graceful shutdown.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use reelix_api::{ApiConfig, AppState};
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::JobSystem;
use reelix_jobs::builtin::{self, Housekeeping};
use tokio_util::sync::CancellationToken;

use crate::config::{self, Config};
use crate::modules::{self, Deps, Modules};
use crate::web::{self, WebUi};

/// How long open connections (e.g. WebSockets) may delay shutdown after the signal.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// Event bus capacity per subscriber.
const EVENT_BUS_CAPACITY: usize = 1024;

/// Runs the server until Ctrl-C / SIGTERM (or [`crate::lifecycle::shutdown_token`] is cancelled).
pub async fn run(config: Config) -> anyhow::Result<()> {
    for dir in [&config.data_dir, &config.backup_dir, &config.log_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating directory {}", dir.display()))?;
    }
    let wrote_config = config.config_file.is_none()
        && config::write_default_config_if_missing(&config.default_config_path())
            .context("writing default config.toml")?;

    let _log_guard = crate::telemetry::init(&config.log_level, config.log_format, &config.log_dir)
        .context("initialising logging")?;
    tracing::info!(version = reelix_core::VERSION, data_dir = %config.data_dir.display(), "starting Reelix");
    match &config.config_file {
        Some(path) => tracing::info!(path = %path.display(), "loaded config file"),
        None if wrote_config => {
            tracing::info!(path = %config.default_config_path().display(), "wrote default config file")
        }
        None => {}
    }
    // Leftovers of the last self-update (the replaced files, an interrupted download).
    crate::maintenance::self_update::startup().await;

    // A restore staged before the last shutdown replaces the database before it is opened.
    crate::maintenance::apply_pending_restore(&config).await;

    let db = Db::connect(&config.database_url)
        .await
        .context("opening database")?;
    tracing::info!(kind = config.database_kind(), "database ready");

    let secrets = load_secrets(&config)?;
    let events = EventBus::new(EVENT_BUS_CAPACITY);

    let modules = Modules::build(Deps {
        db: db.clone(),
        events: events.clone(),
        secrets: secrets.clone(),
        http: modules::http_client()?,
        data_dir: config.data_dir.clone(),
        config: config.clone(),
    })
    .await
    .context("starting modules")?;

    let mut jobs = modules.job_handlers(JobSystem::builder(db.clone(), events.clone()).handler(Housekeeping));
    for task in builtin::default_tasks() {
        jobs = jobs.task(task);
    }
    let jobs = jobs.build();
    let queue = jobs.queue();
    // Shared with `lifecycle`, so a self-update restart or a Windows service Stop shuts down too.
    let cancel = crate::lifecycle::shutdown_token();
    let jobs_handle = jobs.start(cancel.clone());
    let direct = Arc::new(reelix_direct::DirectService::new(
        db.clone(),
        config.data_dir.clone(),
    ));
    let direct_handle = tokio::spawn(direct.clone().run(cancel.clone()));

    let state = AppState::new(
        db.clone(),
        events,
        queue.clone(),
        secrets,
        ApiConfig {
            url_base: config.url_base.clone(),
            data_dir: config.data_dir.clone(),
            database: config.database_kind().to_owned(),
            start_time: chrono::Utc::now(),
        },
    );
    let state = modules
        .attach(&queue, state)
        .with_extension(direct)
        .with_security(reelix_api::SecurityConfig {
            trusted_proxies: config.trusted_proxies.clone(),
            secure_cookies: config.secure_cookies,
        });
    let state = reelix_api::require_setup_token(state)
        .await
        .map_err(|e| anyhow::anyhow!("checking for first-run setup: {e}"))?;
    let web_dir = web::resolve_web_dir(config.web_dir.as_deref());
    let web_ui = Arc::new(WebUi::load(&web_dir, &config.url_base));
    let app = web::build_app(reelix_api::router(state), web_ui, &config.url_base);

    let (listener, dual_stack) = bind_listener(config.bind, config.port).await?;
    let addr = listener.local_addr().context("reading the listener address")?;
    if dual_stack {
        tracing::info!("listening on http://{addr}{}/ (IPv4 and IPv6)", config.url_base);
    } else {
        tracing::info!("listening on http://{addr}{}/", config.url_base);
    }

    let serve = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal(cancel.clone()));
    let grace = {
        let cancel = cancel.clone();
        async move {
            cancel.cancelled().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        }
    };
    tokio::select! {
        res = serve => res.context("HTTP server error")?,
        () = grace => tracing::warn!("connections still open after {SHUTDOWN_GRACE:?}; closing them"),
    }

    // Covers the server exiting on its own; normally the signal already cancelled.
    cancel.cancel();
    tracing::info!("waiting for background jobs to stop");
    if let Err(e) = jobs_handle.await {
        tracing::error!(error = %e, "job system task failed");
    }
    if let Err(e) = direct_handle.await? {
        tracing::error!(error = %e, "direct download worker stopped");
    }
    db.pool().close().await;
    tracing::info!("shutdown complete");
    Ok(())
}

/// Binds the HTTP listener. An unspecified address (`0.0.0.0` or `::`) gets a dual-stack IPv6
/// socket whose IPv4-mapped addresses serve IPv4 clients too, so both families reach the port;
/// when that is unavailable (no IPv6 stack) it falls back to the configured family alone.
///
/// The flag reports whether the socket is dual-stack.
async fn bind_listener(bind: IpAddr, port: u16) -> anyhow::Result<(tokio::net::TcpListener, bool)> {
    if bind.is_unspecified() {
        match dual_stack_listener(port) {
            Ok(listener) => return Ok((listener, true)),
            Err(e) => {
                tracing::warn!("cannot bind [::]:{port} for IPv4 and IPv6 ({e}); falling back to {bind}")
            }
        }
    }
    let addr = SocketAddr::new(bind, port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    Ok((listener, false))
}

/// Non-blocking `[::]` TCP listener with `IPV6_V6ONLY` cleared (dual-stack).
fn dual_stack_listener(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(false)?;
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    tokio::net::TcpListener::from_std(socket.into())
}

/// `REELIX_SECRET_KEY` (base64, 32 bytes) overrides the key file.
pub(crate) fn load_secrets(config: &Config) -> anyhow::Result<SecretBox> {
    if let Some(encoded) = std::env::var("REELIX_SECRET_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .context("REELIX_SECRET_KEY is not valid base64")?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("REELIX_SECRET_KEY must decode to 32 bytes"))?;
        tracing::info!("using secret key from REELIX_SECRET_KEY");
        return Ok(SecretBox::from_key(key));
    }
    SecretBox::load_or_create(&config.secret_key_file)
        .with_context(|| format!("loading secret key {}", config.secret_key_file.display()))
}

/// Resolves on Ctrl-C or SIGTERM, then cancels `cancel`.
async fn shutdown_signal(cancel: CancellationToken) {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received Ctrl-C, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
        () = cancel.cancelled() => {}
    }
    cancel.cancel();
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    /// IPv4 must always reach a listener created for an unspecified bind, and IPv6 whenever the
    /// platform has a usable dual-stack socket.
    #[tokio::test]
    async fn unspecified_bind_accepts_ipv4_and_ipv6() {
        let (listener, dual_stack) = bind_listener(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
            .await
            .expect("binding an ephemeral port");
        let port = listener.local_addr().unwrap().port();

        for target in ["127.0.0.1", "::1"] {
            if target == "::1" && !dual_stack {
                continue;
            }
            let connect = tokio::net::TcpStream::connect(format!("{target}:{port}"));
            let (client, accepted) = tokio::join!(connect, listener.accept());
            client.unwrap_or_else(|e| panic!("connecting to {target}:{port}: {e}"));
            accepted.expect("accepting a connection");
        }
    }

    /// A specific address keeps its own family.
    #[tokio::test]
    async fn specific_bind_stays_single_family() {
        let bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let (listener, dual_stack) = bind_listener(bind, 0).await.expect("binding loopback");
        assert!(!dual_stack);
        let port = listener.local_addr().unwrap().port();
        let connect = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port));
        let (client, accepted) = tokio::join!(connect, listener.accept());
        client.expect("IPv4 loopback connect");
        accepted.expect("accepting a connection");
    }
}
