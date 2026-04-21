pub mod alignment;
pub mod detection;
pub mod liveness;
pub mod quality;
pub mod recognition;

use ort::ep::ExecutionProviderDispatch;

/// Build execution provider dispatch from config string.
/// Falls back to CPU if the requested provider isn't available.
pub fn execution_providers(ep_name: &str) -> Vec<ExecutionProviderDispatch> {
    match ep_name {
        "rocm" => {
            tracing::info!("Using ROCm (AMD GPU) execution provider");
            vec![ort::ep::ROCm::default().build(), ort::ep::CPU::default().build()]
        }
        "cuda" => {
            tracing::info!("Using CUDA (NVIDIA GPU) execution provider");
            vec![ort::ep::CUDA::default().build(), ort::ep::CPU::default().build()]
        }
        "openvino" => {
            tracing::info!("Using OpenVINO execution provider");
            vec![ort::ep::OpenVINO::default().build(), ort::ep::CPU::default().build()]
        }
        "vitis" | "xdna" => {
            tracing::info!("Using Vitis AI (AMD XDNA/NPU) execution provider");
            vec![ort::ep::Vitis::default().build(), ort::ep::CPU::default().build()]
        }
        _ => {
            vec![ort::ep::CPU::default().build()]
        }
    }
}
