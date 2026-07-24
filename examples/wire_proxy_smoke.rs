//! Smoke test for the transparent wire proxy.
//!
//! Starts the proxy in-process (pointing at a running Brain), then drives an
//! encode + recall round-trip through it with the SDK's wire client — proving a
//! customer SDK can speak the wire protocol to the edge and have frames spliced
//! to Brain unchanged.
//!
//! Run against the `brain` container:
//!   BRAIN_ADDR=127.0.0.1:9090 BRAIN_KEY=brain_… \
//!     cargo run --example wire_proxy_smoke

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use brain_db_sdk::verbs::{EncodeBuilder, RecallBuilder};
use brain_db_sdk::{Auth, BrainClient};
use brain_edge::{wire_proxy, MeteringSink, NoopMeter, RateLimitConfig, WireProxyConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let brain_addr: SocketAddr = std::env::var("BRAIN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9090".into())
        .parse()?;
    let key = std::env::var("BRAIN_KEY").map_err(|_| "set BRAIN_KEY to a minted Brain key")?;
    let wire_addr: SocketAddr = "127.0.0.1:9190".parse()?;

    // Start the proxy in the background.
    let meter: Arc<dyn MeteringSink> = Arc::new(NoopMeter);
    let cfg = WireProxyConfig {
        wire_listen_addr: wire_addr,
        brain_addr,
        rate: RateLimitConfig {
            capacity: 0,
            refill_per_sec: 0,
        },
    };
    tokio::spawn(async move {
        if let Err(e) = wire_proxy::serve(cfg, meter).await {
            eprintln!("proxy exited: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Point the SDK at the PROXY, not Brain. Same key, same protocol.
    let client = BrainClient::connect(wire_addr, Auth::Token(key.into_bytes())).await?;
    println!(
        "handshake through proxy OK: namespace={:?}",
        client.connection()
    );

    let text = "Ada prefers oat milk in her coffee.";
    let enc = client.encode(&EncodeBuilder::new(text).build()).await?;
    println!("encode through proxy OK: memory_id={:?}", enc.memory_id);

    tokio::time::sleep(Duration::from_millis(2500)).await;

    let recalled = client
        .recall(&RecallBuilder::new("What milk does Ada like?").build())
        .await?;
    println!(
        "recall through proxy OK: kind={:?} hits={}",
        recalled.answer_kind,
        recalled.memories().len()
    );
    for m in recalled.memories() {
        println!("  - {m:?}");
    }

    println!("SMOKE OK");
    Ok(())
}
