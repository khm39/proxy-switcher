use crate::models::TestStatus;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const TEST_URL: &str = "https://www.google.com";
const TIMEOUT_SECS: u64 = 10;

/// Spawn an async connection test through the given proxy URL.
/// Updates `status` with the result and requests a repaint via `ctx`.
pub fn run_test(
    rt: &tokio::runtime::Runtime,
    proxy_url: String,
    status: Arc<Mutex<TestStatus>>,
    ctx: egui::Context,
) {
    // Mark as testing
    {
        let mut s = status.lock().unwrap();
        *s = TestStatus::Testing;
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let result = do_test(&proxy_url).await;
        {
            let mut s = status.lock().unwrap();
            *s = result;
        }
        ctx.request_repaint();
    });
}

async fn do_test(proxy_url: &str) -> TestStatus {
    let proxy = match reqwest::Proxy::all(proxy_url) {
        Ok(p) => p,
        Err(e) => return TestStatus::Failed(format!("Invalid proxy: {e}")),
    };

    let client = match reqwest::Client::builder()
        .proxy(proxy)
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return TestStatus::Failed(format!("Client error: {e}")),
    };

    let start = Instant::now();
    match client.get(TEST_URL).send().await {
        Ok(resp) => {
            let ms = start.elapsed().as_millis() as u64;
            if resp.status().is_success() || resp.status().is_redirection() {
                TestStatus::Success(ms)
            } else if resp.status().as_u16() == 407 {
                TestStatus::Failed("Authentication required".to_string())
            } else {
                TestStatus::Failed(format!("HTTP {}", resp.status()))
            }
        }
        Err(e) => {
            if e.is_timeout() {
                TestStatus::Failed("Timeout".to_string())
            } else if e.is_connect() {
                TestStatus::Failed("Connection failed".to_string())
            } else {
                TestStatus::Failed(format!("{e}"))
            }
        }
    }
}
