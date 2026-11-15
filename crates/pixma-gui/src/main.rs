//! Entry point for `pixma-studio`.

use pixma_gui::app::{AppLaunchInit, AppModel};
use pixma_gui::remote_bridge::maybe_start_auto_bridge;
use pixma_gui::state::run_self_test;
use relm4::RelmApp;

fn main() -> anyhow::Result<()> {
    if std::env::var("RUST_LOG").is_ok() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    let args: Vec<String> = std::env::args().collect();
    let is_self_test = args.iter().any(|a| a == "--self-test");
    let is_mock_mode =
        args.iter().any(|a| a == "--mock") || std::env::var("PIXMA_MOCK").as_deref() == Ok("1");
    let mut configured_socket = std::env::var("PIXMA_SOCKET")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let mut configured_remote_host = std::env::var("PIXMA_REMOTE_HOST")
        .ok()
        .filter(|s| !s.trim().is_empty());

    let _auto_bridge = if is_self_test {
        None
    } else {
        let bridge = maybe_start_auto_bridge(is_mock_mode, configured_socket.as_deref());
        if let Some(ref b) = bridge {
            let sock = b.socket_path().to_string_lossy().into_owned();
            let host = b.remote_host().to_string();
            if configured_remote_host.is_none() {
                configured_remote_host = Some(host);
            }
            configured_socket = Some(sock);
        }
        bridge
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        if is_self_test {
            println!("[pixma-studio] Running headless self-test suite...");
            run_self_test().await.map_err(|e| anyhow::anyhow!(e))?;
            println!("[pixma-studio] Headless self-test succeeded. Exiting 0.");
            return Ok(());
        }

        let client = if is_mock_mode {
            pixma_daemon::DaemonClient::connect_mock(None).ok()
        } else {
            None
        };

        let init = AppLaunchInit {
            client,
            is_mock_mode,
            configured_socket,
            configured_remote_host,
        };

        let app = RelmApp::new("org.pixma.G2010Studio");
        app.run::<AppModel>(init);

        Ok(())
    })
}
