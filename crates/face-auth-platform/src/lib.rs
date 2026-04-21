// Pluggable platform traits — Phase 3+ implementation
// Stub for workspace compilation

pub mod ir_emitter;

pub trait DisplayManagerBackend {
    fn dm_user(&self) -> &str;
    fn detect(&self) -> bool;
}

pub trait ModelStoreBackend {
    fn model_dir(&self) -> std::path::PathBuf;
}
