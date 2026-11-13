//! A TCP relay between TernaryGrid and a database server whose connections the test
//! can black-hole (they stay open and nothing goes through: a connection that died
//! while the computer slept, half-open) or cut (as a server restart). New connections
//! still go through. Plugged in as the connection's "tunnel".

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use ternarygrid_app::conn::{Credentials, NetConnector, TunnelGuard};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub struct Relay {
    pub addr: SocketAddr,
    next_id: Arc<AtomicU64>,
    freeze_below: watch::Sender<u64>,
    cut_below: watch::Sender<u64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Resolves when `rx` passes `id`.
async fn passed(mut rx: watch::Receiver<u64>, id: u64) {
    loop {
        if *rx.borrow() > id {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Relay {
    /// Must run inside a tokio runtime.
    pub async fn start(target: SocketAddr) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let next_id = Arc::new(AtomicU64::new(0));
        let (freeze_below, freeze_rx) = watch::channel(0u64);
        let (cut_below, cut_rx) = watch::channel(0u64);
        let ids = next_id.clone();
        let task = tokio::spawn(async move {
            while let Ok((down, _)) = listener.accept().await {
                let id = ids.fetch_add(1, Ordering::SeqCst);
                let (freeze, cut) = (freeze_rx.clone(), cut_rx.clone());
                tokio::spawn(async move {
                    let Ok(up) = TcpStream::connect(target).await else { return };
                    let _ = down.set_nodelay(true);
                    let _ = up.set_nodelay(true);
                    let (mut dr, mut dw) = down.into_split();
                    let (mut ur, mut uw) = up.into_split();
                    let copy = async {
                        let _ = tokio::join!(tokio::io::copy(&mut dr, &mut uw), tokio::io::copy(&mut ur, &mut dw));
                    };
                    let frozen = tokio::select! {
                        _ = copy => false,
                        _ = passed(freeze, id) => true,
                        // Cut: both sockets are dropped (closed).
                        _ = passed(cut.clone(), id) => false,
                    };
                    if frozen {
                        // Both sockets stay open and nothing goes through any more, until cut.
                        passed(cut, id).await;
                    }
                    drop((dr, dw, ur, uw));
                });
            }
        });
        Relay { addr, next_id, freeze_below, cut_below, task }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Black-holes every connection made so far.
    pub fn freeze_existing(&self) {
        let _ = self.freeze_below.send(self.next_id.load(Ordering::SeqCst));
    }

    /// Closes every connection made so far.
    pub fn cut_existing(&self) {
        let _ = self.cut_below.send(self.next_id.load(Ordering::SeqCst));
    }
}

/// Connects through one [`Relay`] to the server, made on the first connect.
#[derive(Default)]
pub struct RelayNet {
    pub relay: tokio::sync::Mutex<Option<Arc<Relay>>>,
}

impl RelayNet {
    pub fn relay(&self) -> Arc<Relay> {
        self.relay.try_lock().expect("not connecting").clone().expect("connected once")
    }
}

#[async_trait::async_trait]
impl NetConnector for RelayNet {
    async fn open(&self, _cfg: &ternarygrid_core::ConnectionConfig, _creds: &Credentials, host: &str, port: u16) -> ternarygrid_core::DbResult<Option<TunnelGuard>> {
        let mut slot = self.relay.lock().await;
        if slot.is_none() {
            let target = tokio::net::lookup_host((host, port)).await.unwrap().next().unwrap();
            *slot = Some(Arc::new(Relay::start(target).await));
        }
        let relay = slot.as_ref().unwrap();
        Ok(Some(TunnelGuard { local_host: "127.0.0.1".into(), local_port: relay.port(), alive: Box::new(|| true), guard: Box::new(()), check: None }))
    }
}
