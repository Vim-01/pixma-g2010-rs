//! Entry point for `pixma-studio`.

use pixma_gui::app::AppModel;
use pixma_gui::state::run_self_test;
use relm4::RelmApp;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--self-test") {
        println!("[pixma-studio] Running headless self-test suite...");
        run_self_test().await.map_err(|e| anyhow::anyhow!(e))?;
        println!("[pixma-studio] Headless self-test succeeded. Exiting 0.");
        return Ok(());
    }

    let client = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        pixma_daemon::DaemonClient::connect_or_fallback(None, None),
    )
    .await
    .ok()
    .and_then(|r| r.ok());

    let app = RelmApp::new("io.github.vim01.pixma_studio");
    app.run::<AppModel>(client);

    Ok(())
}
