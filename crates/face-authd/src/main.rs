mod camera;
mod error;
mod inference;
mod model_store;
mod session;

use error::DaemonError;
use face_auth_core::config::Config;
use face_auth_core::framing::{read_message, write_message};
use face_auth_core::protocol::{AuthOutcome, DaemonMessage, PamRequest, PROTOCOL_VERSION};
use model_store::ModelStore;
use session::SessionManager;
use std::io::BufWriter;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// Live config — atomically swappable on SIGHUP.
/// Sessions snapshot the inner Arc at connection time; in-flight sessions
/// are unaffected by reloads.
type LiveConfig = Arc<RwLock<Arc<Config>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load config first (needed for log level)
    let initial_config = Arc::new(Config::load_system().unwrap_or_else(|e| {
        eprintln!("config load warning: {e}, using defaults");
        Config::default()
    }));

    // Init tracing — pretty for TTY, JSON for journald/pipe
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{},ort=warn", &initial_config.logging.level)));
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if is_tty {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    }

    tracing::info!("face-authd starting");

    // Pre-load ML models (once, reused across all auth sessions)
    let ep = initial_config.daemon.execution_provider.clone();
    let initial_models =
        inference::ModelCache::load(&ep).expect("failed to load ML models at startup");
    tracing::info!(execution_provider = %ep, "ML models loaded (SCRFD + ArcFace)");
    let models = Arc::new(tokio::sync::Mutex::new(ModelStore::new(
        initial_models,
        &ep,
    )));

    // Wrap config for live reload
    let config: LiveConfig = Arc::new(RwLock::new(initial_config));

    // Create socket directory
    let socket_path = config.read().unwrap().daemon.socket_path.clone();
    let ui_socket_path = config.read().unwrap().daemon.ui_socket_path.clone();

    let socket_dir = Path::new(&socket_path)
        .parent()
        .expect("socket path must have parent directory");
    std::fs::create_dir_all(socket_dir)?;

    // Remove stale sockets
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&ui_socket_path);

    // Bind PAM socket
    let pam_listener = tokio::net::UnixListener::bind(&socket_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o666))?;
    }
    tracing::info!(path = %socket_path, "PAM socket bound");

    // Bind UI socket
    let _ui_listener = tokio::net::UnixListener::bind(&ui_socket_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ui_socket_path, std::fs::Permissions::from_mode(0o666))?;
    }
    tracing::info!(path = %ui_socket_path, "UI socket bound");

    // Session manager
    let session_manager = Arc::new(tokio::sync::Mutex::new(SessionManager::new()));

    // Idle model unload background task — reads live config each tick
    {
        let models_idle = models.clone();
        let config_idle = config.clone();
        tokio::spawn(async move {
            let interval = Duration::from_secs(30);
            loop {
                tokio::time::sleep(interval).await;
                let idle_s = config_idle.read().unwrap().daemon.idle_unload_s;
                if idle_s > 0 {
                    let mut store = models_idle.lock().await;
                    if store.is_loaded() {
                        store.maybe_unload(idle_s);
                    }
                }
            }
        });
    }

    // Signal handling
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    // Notify systemd we're ready (Type=notify)
    sd_notify_ready();

    tracing::info!("face-authd ready, waiting for connections");

    // Main accept loop
    loop {
        tokio::select! {
            result = pam_listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        // Snapshot current config for this connection
                        let cfg = config.read().unwrap().clone();
                        let sm = session_manager.clone();
                        let mdl = models.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_pam_connection(stream, cfg, sm, mdl).await {
                                tracing::error!("PAM connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("accept error: {e}");
                    }
                }
            }
            _ = sighup.recv() => {
                reload_config(&config, &models).await;
            }
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM received, shutting down");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("SIGINT received, shutting down");
                break;
            }
        }
    }

    // Cleanup
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&ui_socket_path);
    tracing::info!("face-authd stopped");

    Ok(())
}

/// Reload config from disk on SIGHUP.
/// If execution_provider changed, also reloads ML models.
async fn reload_config(config: &LiveConfig, models: &Arc<tokio::sync::Mutex<ModelStore>>) {
    tracing::info!("SIGHUP received — reloading config");

    let old_ep = config.read().unwrap().daemon.execution_provider.clone();

    match Config::load_system() {
        Ok(new_cfg) => {
            let new_ep = new_cfg.daemon.execution_provider.clone();
            *config.write().unwrap() = Arc::new(new_cfg);
            tracing::info!("config reloaded from /etc/face-auth/config.toml");

            // Reload models only if execution provider changed
            if new_ep != old_ep {
                tracing::info!(old = %old_ep, new = %new_ep, "execution_provider changed — reloading models");
                let mut store = models.lock().await;
                if let Err(e) = store.reload_with_ep(&new_ep) {
                    tracing::error!("model reload failed: {e}");
                }
            }
        }
        Err(e) => {
            tracing::warn!("config reload failed, keeping current config: {e}");
        }
    }
}

async fn handle_pam_connection(
    stream: tokio::net::UnixStream,
    config: Arc<Config>,
    session_manager: Arc<tokio::sync::Mutex<SessionManager>>,
    model_store: Arc<tokio::sync::Mutex<ModelStore>>,
) -> Result<(), DaemonError> {
    // Convert to std for sync framing
    let std_stream = stream.into_std()?;
    std_stream.set_nonblocking(false)?;

    // Read request
    let mut reader = std_stream.try_clone()?;
    let request: PamRequest = tokio::task::spawn_blocking(move || read_message(&mut reader))
        .await
        .map_err(|e| DaemonError::Join(e.to_string()))??;

    match request {
        PamRequest::Auth {
            version,
            username,
            session_id,
        } => {
            if version != PROTOCOL_VERSION {
                tracing::warn!(
                    version,
                    expected = PROTOCOL_VERSION,
                    "protocol version mismatch"
                );
            }
            tracing::info!(session_id, "auth request received");
            tracing::debug!(session_id, %username, "auth request details");

            // Try to acquire session slot
            {
                let mut sm = session_manager.lock().await;
                if sm.try_start(session_id).is_err() {
                    tracing::info!(session_id, "rejected: session already active");
                    let writer_stream = std_stream.try_clone()?;
                    tokio::task::spawn_blocking(move || {
                        let msg = DaemonMessage::AuthResult {
                            session_id,
                            outcome: AuthOutcome::Failed,
                        };
                        let mut writer = BufWriter::new(&writer_stream);
                        write_message(&mut writer, &msg)?;
                        std::io::Write::flush(&mut writer)
                    })
                    .await
                    .map_err(|e| DaemonError::Join(e.to_string()))??;
                    return Ok(());
                }
            }

            // Get (or reload) models for this session
            let models = model_store.lock().await.get_or_load()?;

            // Run auth session (handles its own cleanup)
            session::run_auth_session(
                session_id,
                username,
                config,
                session_manager,
                models,
                model_store,
                std_stream,
            )
            .await;
        }
        PamRequest::Cancel { session_id } => {
            tracing::info!(session_id, "cancel request (standalone)");
            let mut sm = session_manager.lock().await;
            sm.end(session_id);
        }
    }

    Ok(())
}

/// Send sd_notify READY=1 to systemd (if NOTIFY_SOCKET is set).
fn sd_notify_ready() {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let sock = match std::os::unix::net::UnixDatagram::unbound() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("sd_notify: socket create failed: {e}");
            return;
        }
    };
    match sock.send_to(b"READY=1", &path) {
        Ok(_) => tracing::info!("sd_notify: READY=1"),
        Err(e) => tracing::warn!("sd_notify failed: {e}"),
    }
}
