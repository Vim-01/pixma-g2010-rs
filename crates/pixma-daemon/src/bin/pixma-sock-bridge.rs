//! Restricted stdio <-> Unix domain socket bridge for `pixma-daemon`.
//!
//! Server mode (default, including `sshd` `ForceCommand` / `-c ...` invocations):
//!   Connects strictly to `/run/pixma/pixmad.sock` and bridges stdin/stdout <-> UnixStream.
//!   All non-`--listen` arguments are ignored so `ForceCommand` / restricted login shell
//!   usage cannot redirect traffic to any other socket, host, or shell.
//!
//! Client listener mode (`pixma-sock-bridge --listen <local_sock> -- <cmd> [args...]`):
//!   Listens on a private local Unix domain socket (mode 0600) and spawns `<cmd> [args...]`
//!   (typically ephemeral `ssh -T ...`) per accepted local connection, bridging stdio.

use std::env;
use std::fs::{self, Permissions};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;

const DEFAULT_DAEMON_SOCKET: &str = "/run/pixma/pixmad.sock";

enum Mode {
    Help,
    ServerBridge,
    ClientListen {
        listen_sock: PathBuf,
        command: Vec<String>,
    },
}

struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn parse_mode(args: &[String]) -> Result<Mode> {
    match args.get(1).map(String::as_str) {
        Some("--help" | "-h") => Ok(Mode::Help),
        Some("--listen") => {
            let sock = args
                .get(2)
                .filter(|s| !s.is_empty() && *s != "--")
                .context("--listen requires a socket path before `--`")?;
            let sep = args
                .iter()
                .position(|a| a == "--")
                .context("--listen requires `-- <command> [args...]`")?;
            if sep < 3 || sep + 1 >= args.len() {
                bail!("invalid --listen arguments; expected `--listen <sock> -- <cmd> [args...]`");
            }
            Ok(Mode::ClientListen {
                listen_sock: PathBuf::from(sock),
                command: args[sep + 1..].to_vec(),
            })
        }
        // Any other invocation (no args, or `<shell> -c <ForceCommand>` from sshd)
        // runs exclusively in hardcoded server bridge mode.
        _ => Ok(Mode::ServerBridge),
    }
}

async fn bridge_streams<R1, W1, R2, W2>(
    mut peer_read: R1,
    mut peer_write: W1,
    mut sock_read: R2,
    mut sock_write: W2,
) -> Result<()>
where
    R1: AsyncRead + Unpin,
    W1: AsyncWrite + Unpin,
    R2: AsyncRead + Unpin,
    W2: AsyncWrite + Unpin,
{
    let to_sock = async {
        tokio::io::copy(&mut peer_read, &mut sock_write).await?;
        sock_write.shutdown().await?;
        Ok::<(), std::io::Error>(())
    };
    let from_sock = async {
        tokio::io::copy(&mut sock_read, &mut peer_write).await?;
        peer_write.flush().await?;
        let _ = peer_write.shutdown().await;
        Ok::<(), std::io::Error>(())
    };

    tokio::pin!(to_sock);
    tokio::pin!(from_sock);
    let mut to_done = false;

    loop {
        tokio::select! {
            res = &mut to_sock, if !to_done => {
                to_done = true;
                match res {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                        // Allow `from_sock` to flush any trailing error frame from pixmad.
                    }
                    Err(e) => return Err(e).context("copy stdin/peer -> socket failed"),
                }
            }
            res = &mut from_sock => {
                res.context("copy socket -> stdout/peer failed")?;
                break;
            }
        }
    }
    Ok(())
}

async fn run_server_bridge(socket_path: &Path) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("failed to connect to {}", socket_path.display()))?;
    let (sock_read, sock_write) = stream.into_split();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    bridge_streams(stdin, stdout, sock_read, sock_write).await
}

async fn handle_local_client(local_stream: UnixStream, command: Vec<String>) -> Result<()> {
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn tunnel command `{}`", command[0]))?;
    let mut child_stdin = child.stdin.take().context("missing child stdin pipe")?;
    let mut child_stdout = child.stdout.take().context("missing child stdout pipe")?;
    let (mut local_read, mut local_write) = local_stream.into_split();

    let to_child = async move {
        let res = tokio::io::copy(&mut local_read, &mut child_stdin).await;
        let _ = child_stdin.flush().await;
        // Dropping `ChildStdin` closes the OS pipe fd so `ssh` sees EOF.
        drop(child_stdin);
        res
    };
    let from_child = async {
        tokio::io::copy(&mut child_stdout, &mut local_write).await?;
        local_write.shutdown().await?;
        Ok::<(), std::io::Error>(())
    };

    tokio::pin!(to_child);
    tokio::pin!(from_child);
    let mut to_done = false;
    let mut copy_err: Option<anyhow::Error> = None;

    loop {
        tokio::select! {
            res = &mut to_child, if !to_done => {
                to_done = true;
                if let Err(e) = res {
                    if e.kind() != ErrorKind::BrokenPipe {
                        copy_err = Some(anyhow::Error::new(e).context("local -> child pipe failed"));
                    }
                }
            }
            res = &mut from_child => {
                if let Err(e) = res {
                    if copy_err.is_none() {
                        copy_err = Some(anyhow::Error::new(e).context("child -> local pipe failed"));
                    }
                }
                break;
            }
        }
    }

    let status = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    if let Ok(Ok(st)) = status {
        if !st.success() {
            if let Some(err) = copy_err {
                return Err(err);
            }
            bail!("tunnel command exited with status {st}");
        }
    } else if status.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    if let Some(err) = copy_err {
        return Err(err);
    }
    Ok(())
}

async fn wait_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(ref mut s) = sigterm {
                    s.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn prepare_listener_parent(listen_sock: &Path) -> Result<()> {
    let Some(parent) = listen_sock.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    match fs::symlink_metadata(parent) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            pixma_daemon::ensure_verified_private_dir(parent).with_context(|| {
                format!("failed to create private socket dir {}", parent.display())
            })?;
        }
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                bail!(
                    "unsafe parent directory for listener socket: {}",
                    parent.display()
                );
            }
            let mode = meta.permissions().mode();
            // Reject world-writable directories (including sticky /tmp) so listener sockets require a private directory.
            if (mode & 0o002) != 0 {
                bail!(
                    "refusing to bind socket inside world-writable directory {}",
                    parent.display()
                );
            }
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to inspect {}", parent.display()))
        }
    }
    Ok(())
}

fn prepare_listener_socket_target(listen_sock: &Path) -> Result<()> {
    prepare_listener_parent(listen_sock)?;
    match fs::symlink_metadata(listen_sock) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                bail!(
                    "refusing to overwrite symlink listener socket {}",
                    listen_sock.display()
                );
            }
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                bail!(
                    "refusing to unlink non-socket path {}",
                    listen_sock.display()
                );
            }
            fs::remove_file(listen_sock)
                .with_context(|| format!("failed to clear {}", listen_sock.display()))?;
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("failed to inspect {}", listen_sock.display()))
        }
    }
    Ok(())
}

fn is_authorized_peer_stream(stream: &UnixStream, expected_uid: u32) -> bool {
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == expected_uid || cred.uid() == 0 => true,
        Ok(cred) => {
            eprintln!(
                "pixma-sock-bridge: rejected unauthorized peer uid {} (expected {expected_uid})",
                cred.uid()
            );
            false
        }
        Err(err) => {
            eprintln!(
                "pixma-sock-bridge: rejected peer without verifiable SO_PEERCRED credentials: {err}"
            );
            false
        }
    }
}

async fn run_client_listener(listen_sock: &Path, command: Vec<String>) -> Result<()> {
    prepare_listener_socket_target(listen_sock)?;

    let listener = UnixListener::bind(listen_sock)
        .with_context(|| format!("failed to bind {}", listen_sock.display()))?;
    fs::set_permissions(listen_sock, Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod 0600 {}", listen_sock.display()))?;
    let expected_uid = pixma_daemon::current_uid();
    let _cleanup = SocketCleanup(listen_sock.to_path_buf());

    let shutdown = wait_shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept failed")?;
                if !is_authorized_peer_stream(&stream, expected_uid) {
                    continue;
                }
                let cmd = command.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_local_client(stream, cmd).await {
                        eprintln!("pixma-sock-bridge client error: {err:#}");
                    }
                });
            }
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mode = match parse_mode(&args) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("pixma-sock-bridge: {err:#}");
            std::process::exit(2);
        }
    };

    if matches!(mode, Mode::Help) {
        println!(
            "Usage:\n  pixma-sock-bridge                                (server stdio -> {DEFAULT_DAEMON_SOCKET})\n  pixma-sock-bridge --listen <sock> -- <cmd> [...]  (local socket listener)"
        );
        return;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let res = match mode {
        Mode::Help => Ok(()),
        Mode::ServerBridge => rt.block_on(run_server_bridge(Path::new(DEFAULT_DAEMON_SOCKET))),
        Mode::ClientListen {
            listen_sock,
            command,
        } => rt.block_on(run_client_listener(&listen_sock, command)),
    };

    // Do not block on `tokio::io::stdin()`'s blocking reader thread if the Unix socket closed first.
    rt.shutdown_background();

    if let Err(err) = res {
        eprintln!("pixma-sock-bridge: {err:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn sshd_force_command_args_always_use_server_mode() {
        let mode = parse_mode(&[
            "/usr/bin/pixma-sock-bridge".into(),
            "-c".into(),
            "--listen /tmp/evil -- /bin/sh".into(),
        ])
        .unwrap();
        assert!(matches!(mode, Mode::ServerBridge));

        let bare = parse_mode(&["pixma-sock-bridge".into()]).unwrap();
        assert!(matches!(bare, Mode::ServerBridge));
    }

    #[test]
    fn listen_args_parse_correctly() {
        let mode = parse_mode(&[
            "pixma-sock-bridge".into(),
            "--listen".into(),
            "/tmp/test.sock".into(),
            "--".into(),
            "ssh".into(),
            "-T".into(),
            "host".into(),
        ])
        .unwrap();
        match mode {
            Mode::ClientListen {
                listen_sock,
                command,
            } => {
                assert_eq!(listen_sock, PathBuf::from("/tmp/test.sock"));
                assert_eq!(command, vec!["ssh", "-T", "host"]);
            }
            _ => panic!("expected ClientListen mode"),
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "pixma_bridge_test_{}_{tag}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn server_bridge_forwards_bidirectional_and_closes_on_peer_eof() {
        let tmp = TempDir::new("srv");
        let sock_path = tmp.0.join("pixmad.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"PING");
            s.write_all(b"PONG").await.unwrap();
            let mut extra = [0u8; 1];
            assert_eq!(s.read(&mut extra).await.unwrap(), 0);
        });

        let (peer_client, peer_bridge) = tokio::io::duplex(64);
        let (mut cli_read, mut cli_write) = tokio::io::split(peer_client);
        let (br_read, br_write) = tokio::io::split(peer_bridge);

        let sock_stream = UnixStream::connect(&sock_path).await.unwrap();
        let (sr, sw) = sock_stream.into_split();

        let bridge = tokio::spawn(async move { bridge_streams(br_read, br_write, sr, sw).await });

        cli_write.write_all(b"PING").await.unwrap();
        cli_write.shutdown().await.unwrap();

        let mut reply = Vec::new();
        cli_read.read_to_end(&mut reply).await.unwrap();
        assert_eq!(&reply, b"PONG");

        bridge.await.unwrap().unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn client_listener_mode_spawns_bridge_command() {
        let tmp = TempDir::new("cli");
        let local_sock = tmp.0.join("local.sock");
        let listener = UnixListener::bind(&local_sock).unwrap();

        let accept_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_local_client(stream, vec!["cat".to_string()])
                .await
                .unwrap();
        });

        let mut client = UnixStream::connect(&local_sock).await.unwrap();
        client.write_all(b"ECHO_OK").await.unwrap();
        client.shutdown().await.unwrap();
        let mut out = Vec::new();
        client.read_to_end(&mut out).await.unwrap();
        assert_eq!(&out, b"ECHO_OK");
        accept_task.await.unwrap();
    }

    #[test]
    fn client_listener_prepares_private_parent_0700_and_rejects_symlink_parent() {
        let tmp = TempDir::new("parent");
        let missing_dir = tmp.0.join("nested-private");
        let sock = missing_dir.join("remote.sock");
        prepare_listener_parent(&sock).expect("Missing parent should be created as 0700");
        assert_eq!(
            fs::metadata(&missing_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let sym_parent = tmp.0.join("sym-parent");
        std::os::unix::fs::symlink(&missing_dir, &sym_parent).unwrap();
        assert!(prepare_listener_parent(&sym_parent.join("remote.sock")).is_err());

        // Symlink or regular file at listener socket path itself must be refused before unlinking
        let real_target = tmp.0.join("target.sock");
        fs::write(&real_target, b"keep").unwrap();
        let sym_sock = tmp.0.join("sym.sock");
        std::os::unix::fs::symlink(&real_target, &sym_sock).unwrap();
        assert!(prepare_listener_socket_target(&sym_sock).is_err());
        assert!(prepare_listener_socket_target(&real_target).is_err());
        assert_eq!(fs::read(&real_target).unwrap(), b"keep");

        // World-writable directory like /tmp directly must be refused
        assert!(prepare_listener_parent(Path::new("/tmp/direct-world-writable.sock")).is_err());
    }

    #[tokio::test]
    async fn is_authorized_peer_stream_allows_self_and_rejects_other_uid() {
        let tmp = TempDir::new("peercred");
        let sock_path = tmp.0.join("cred.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let srv = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let self_uid = pixma_daemon::current_uid();
            assert!(is_authorized_peer_stream(&stream, self_uid));
            if self_uid != 0 {
                assert!(!is_authorized_peer_stream(&stream, self_uid.saturating_add(1000)));
            }
        });

        let _cli = UnixStream::connect(&sock_path).await.unwrap();
        srv.await.unwrap();
    }
}
