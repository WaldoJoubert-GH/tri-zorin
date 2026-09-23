//! Throwaway probe: real pi model discovery through AcpHarness::pi().
use zeron_harness::{AcpHarness, Harness};

#[tokio::main]
async fn main() {
    let start = std::time::Instant::now();
    let harness = AcpHarness::pi();
    eprintln!("timeout: {:?}", harness.model_discovery_timeout());
    match harness.models().await {
        Ok(models) => {
            for m in models.iter().take(8) {
                eprintln!("{:40} {}", m.id, m.label);
            }
            eprintln!("--- {} models in {:?}", models.len(), start.elapsed());
        }
        Err(e) => {
            eprintln!("discovery failed: {e}");
            std::process::exit(1);
        }
    }
}
