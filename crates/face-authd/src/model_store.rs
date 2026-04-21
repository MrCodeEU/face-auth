use crate::error::DaemonError;
use crate::inference::ModelCache;
use std::sync::Arc;
use std::time::Instant;

/// Manages the ML model lifecycle: loads on demand, unloads after idle timeout.
pub struct ModelStore {
    inner: Option<Arc<ModelCache>>,
    last_used: Instant,
    ep_name: String,
}

impl ModelStore {
    pub fn new(models: ModelCache, ep_name: &str) -> Self {
        Self {
            inner: Some(Arc::new(models)),
            last_used: Instant::now(),
            ep_name: ep_name.to_string(),
        }
    }

    /// Return shared model handle, reloading from disk if previously unloaded.
    pub fn get_or_load(&mut self) -> Result<Arc<ModelCache>, DaemonError> {
        if self.inner.is_none() {
            tracing::info!(ep = %self.ep_name, "reloading ML models after idle unload");
            let models = ModelCache::load(&self.ep_name)
                .map_err(|e| DaemonError::Camera(format!("model reload: {e}")))?;
            self.inner = Some(Arc::new(models));
            tracing::info!("ML models reloaded");
        }
        self.last_used = Instant::now();
        Ok(Arc::clone(self.inner.as_ref().unwrap()))
    }

    /// Update last-used timestamp (call when an auth session ends).
    pub fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    /// Unload models if idle for longer than `idle_unload_s` seconds.
    /// `idle_unload_s == 0` disables idle unloading.
    /// Returns true if models were unloaded.
    pub fn maybe_unload(&mut self, idle_unload_s: u64) -> bool {
        if idle_unload_s == 0 {
            return false;
        }
        if self.inner.is_none() {
            return false;
        }
        if self.last_used.elapsed().as_secs() >= idle_unload_s {
            // Only unload if no other holder has a live Arc clone
            // (i.e., no active auth session holds a reference)
            let models = self.inner.take().unwrap();
            match Arc::try_unwrap(models) {
                Ok(_) => {
                    tracing::info!(
                        idle_s = self.last_used.elapsed().as_secs(),
                        "ML models unloaded (idle timeout)"
                    );
                    true
                }
                Err(arc) => {
                    // Session still running — put it back, try again later
                    self.inner = Some(arc);
                    false
                }
            }
        } else {
            false
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.inner.is_some()
    }

    /// Reload models with a new execution provider.
    /// Drops existing models first, then loads fresh ones.
    pub fn reload_with_ep(&mut self, ep_name: &str) -> Result<(), DaemonError> {
        tracing::info!(ep = %ep_name, "reloading ML models (execution provider changed)");
        self.inner = None;
        self.ep_name = ep_name.to_string();
        self.get_or_load().map(|_| ())
    }
}
