//! A local server belongs to one host session. Stopping it always waits for the
//! server's final save; dropping this owner is the fallback for application exit.
use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::PathBuf,
    thread::{self, JoinHandle},
};
use tokio::sync::oneshot;

pub struct HostedServer {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), String>>>,
    result: Option<Result<(), String>>,
}

impl HostedServer {
    /// Binding happens before returning, so the game can connect immediately.
    /// The existing server validates and loads the world on its own thread;
    /// startup or save failures are surfaced by `poll` or `shutdown_blocking`.
    pub fn start(world_path: PathBuf, lan: bool) -> Result<Self, String> {
        let world_lock = crate::world_saves::lock_world(&world_path)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| format!("Could not start the local server: {error}"))?;
        let ip = if lan {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        // The OS chooses a free port. Repeated host sessions and other running
        // instances never require taking over another server's listener.
        let listener = TcpListener::bind((ip, 0))
            .map_err(|error| format!("Could not listen for players: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let (shutdown, stopped) = oneshot::channel();
        let config = sauerteig_server::Config {
            world_path,
            ..Default::default()
        };
        let thread = thread::Builder::new()
            .name("sauerteig-host".into())
            .spawn(move || {
                let _world_lock = world_lock;
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .map_err(|error| error.to_string())?;
                    sauerteig_server::serve(listener, config, async move {
                        // A dropped sender also means the owning session ended.
                        let _ = stopped.await;
                    })
                    .await
                })
            })
            .map_err(|error| format!("Could not start the host thread: {error}"))?;
        Ok(Self {
            address,
            shutdown: Some(shutdown),
            thread: Some(thread),
            result: None,
        })
    }

    /// The host always joins its own server over loopback, including LAN rooms.
    pub fn endpoint(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.address.port())
    }

    /// This is the listening address. In LAN mode 0.0.0.0 means all interfaces;
    /// another device must use this computer's actual LAN address and this port.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// Report completion once, without blocking the UI while a save is pending.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            Some(self.finish())
        } else {
            None
        }
    }

    /// Use on final application shutdown. There is deliberately no timeout that
    /// abandons the worker: a successful return means the final save completed.
    pub fn shutdown_blocking(&mut self) -> Result<(), String> {
        self.stop();
        self.finish()
    }

    fn finish(&mut self) -> Result<(), String> {
        if let Some(thread) = self.thread.take() {
            self.result = Some(thread.join().unwrap_or_else(|_| {
                Err("The local server stopped unexpectedly before confirming its save.".into())
            }));
            self.shutdown = None;
        }
        self.result.clone().unwrap_or(Ok(()))
    }
}

impl Drop for HostedServer {
    fn drop(&mut self) {
        if self.thread.is_some() {
            if let Err(error) = self.shutdown_blocking() {
                log::error!("Local server shutdown failed: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::testing::Client;
    use std::{
        fs,
        io::{Read, Write},
        net::{IpAddr, TcpStream},
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "sauerteig-host-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn world(&self) -> PathBuf {
            self.0.join("world.json")
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wait_for_completion(host: &mut HostedServer) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(result) = host.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "host did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn open_websocket(host: &HostedServer) -> TcpStream {
        let mut socket = TcpStream::connect(host.address()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket
            .write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
            assert!(header.len() < 8192);
        }
        assert!(String::from_utf8(header)
            .unwrap()
            .starts_with("HTTP/1.1 101"));
        socket
    }

    #[test]
    fn repeated_host_sessions_save_and_release_the_listener() {
        let directory = TestDirectory::new();
        for _ in 0..3 {
            let mut host = HostedServer::start(directory.world(), false).unwrap();
            let address = host.address();
            assert_eq!(address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
            assert_eq!(host.endpoint(), format!("ws://{address}/ws"));
            host.stop();
            host.stop();
            wait_for_completion(&mut host).unwrap();
            assert!(host.poll().is_none());
            host.shutdown_blocking().unwrap();
            let saved = fs::read(directory.world()).unwrap();
            assert!(!saved.is_empty(), "shutdown must finish the first save");
            let rebound = TcpListener::bind(address).unwrap();
            drop(rebound);
        }
    }

    #[test]
    fn corrupt_world_is_reported_without_overwriting_it() {
        let directory = TestDirectory::new();
        let original = b"not a saved world";
        fs::write(directory.world(), original).unwrap();
        let mut host = HostedServer::start(directory.world(), false).unwrap();
        let address = host.address();
        assert!(wait_for_completion(&mut host).is_err());
        assert!(host.shutdown_blocking().is_err());
        assert_eq!(fs::read(directory.world()).unwrap(), original);
        assert!(TcpListener::bind(address).is_ok());
    }

    #[test]
    fn a_world_cannot_be_hosted_twice_and_unlocks_after_shutdown() {
        let directory = TestDirectory::new();
        let mut first = HostedServer::start(directory.world(), false).unwrap();
        assert!(HostedServer::start(directory.world(), false).is_err());
        first.shutdown_blocking().unwrap();
        let mut second = HostedServer::start(directory.world(), false).unwrap();
        second.shutdown_blocking().unwrap();
    }

    #[test]
    fn graceful_stop_finishes_with_an_open_websocket() {
        let directory = TestDirectory::new();
        fs::write(
            directory.world(),
            br#"{"revision":0,"cells":[],"objects":[]}"#,
        )
        .unwrap();
        let mut host = HostedServer::start(directory.world(), false).unwrap();
        let socket = open_websocket(&host);
        host.stop();
        wait_for_completion(&mut host).unwrap();
        assert!(!fs::read(directory.world()).unwrap().is_empty());
        drop(socket);
    }

    /// Connects as a player, turns editing on and places a light, returning
    /// once the server has applied it. Dropping the client leaves.
    fn place_light(host: &HostedServer, position: sauerteig_core::Vec3) -> Client {
        use sauerteig_core::{ClientMessage, ObjectKind, ServerMessage};
        let (mut client, _) = Client::join(&host.endpoint());
        client.send(ClientMessage::SetMode {
            request: 1,
            editing: true,
        });
        client.message(|message| {
            matches!(
                message,
                ServerMessage::ModeChanged {
                    request: 1,
                    editing: true
                }
            )
        });
        client.send(ClientMessage::PlaceObject {
            kind: ObjectKind::Light,
            position,
            rotation: 0.0,
        });
        client.message(
            |message| matches!(message, ServerMessage::Objects { objects, .. } if objects.iter().any(|object| object.position == position)),
        );
        client
    }

    fn wait_for_file(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.is_file() {
            assert!(Instant::now() < deadline, "the first save never came");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn an_edit_survives_stopping_and_reopening_the_host() {
        use sauerteig_core::{ObjectKind, Vec3};
        let directory = TestDirectory::new();
        let mut host = HostedServer::start(directory.world(), false).unwrap();
        let position = Vec3::new(0.0, 40.0, 0.0);
        let client = place_light(&host, position);
        // Stop immediately after the edit is acknowledged, before waiting for
        // autosave. Reopening through the public protocol proves final-save use.
        drop(client);
        host.shutdown_blocking().unwrap();
        let mut reopened = HostedServer::start(directory.world(), false).unwrap();
        let (client, welcome) = Client::join(&reopened.endpoint());
        let objects = welcome.objects;
        assert!(objects
            .iter()
            .any(|object| object.position == position && object.kind == ObjectKind::Light));
        drop(client);
        reopened.shutdown_blocking().unwrap();
    }

    #[test]
    fn dropping_host_waits_for_save_and_lan_requires_explicit_opt_in() {
        let directory = TestDirectory::new();
        let host = HostedServer::start(directory.world(), true).unwrap();
        assert_eq!(host.address().ip(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert!(host.endpoint().starts_with("ws://127.0.0.1:"));
        drop(host);
        assert!(directory.world().is_file());
    }

    #[test]
    fn final_save_failure_is_returned_to_the_session() {
        use sauerteig_core::Vec3;
        let directory = TestDirectory::new();
        let mut host = HostedServer::start(directory.world(), false).unwrap();
        // A new world is saved when the server starts. After that, only a
        // change makes the final save write again.
        wait_for_file(&directory.world());
        let client = place_light(&host, Vec3::new(0.0, 40.0, 0.0));
        // The running server has loaded a world, then its save destination
        // becomes a directory, so the final atomic replacement must fail.
        fs::remove_file(directory.world()).unwrap();
        fs::create_dir(directory.world()).unwrap();
        host.stop();
        assert!(wait_for_completion(&mut host).is_err());
        drop(client);
    }

    #[test]
    fn a_session_without_changes_does_not_rewrite_the_world() {
        let directory = TestDirectory::new();
        let mut first = HostedServer::start(directory.world(), false).unwrap();
        wait_for_file(&directory.world());
        first.shutdown_blocking().unwrap();
        let (bytes, stamp) = (
            fs::read(directory.world()).unwrap(),
            fs::metadata(directory.world()).unwrap().modified().unwrap(),
        );
        thread::sleep(Duration::from_millis(50));
        let mut second = HostedServer::start(directory.world(), false).unwrap();
        thread::sleep(Duration::from_millis(1200));
        second.shutdown_blocking().unwrap();
        assert_eq!(fs::read(directory.world()).unwrap(), bytes);
        assert_eq!(
            fs::metadata(directory.world()).unwrap().modified().unwrap(),
            stamp
        );
    }
}
