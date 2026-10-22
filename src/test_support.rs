//! Deterministic, path-scoped I/O gates. Never compiled into the gateway.
use std::{path::Path, sync::Mutex};
use tokio::sync::oneshot;

pub(crate) struct IoGates(Mutex<Vec<ArmedGate>>);
struct ArmedGate {
    prefix: String,
    skip: usize,
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

pub(crate) struct IoGate {
    entered: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
}
impl IoGate {
    pub(crate) async fn entered(&mut self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.entered.take().expect("wait for gate only once"),
        )
        .await
        .expect("I/O must reach the gate")
        .expect("gated task must not exit early");
    }
    pub(crate) fn release(&mut self) {
        if let Some(sender) = self.release.take() {
            let _ = sender.send(());
        }
    }
}
// Dropping a failed test's gate also releases the blocked writer. No global reset
// can erase another parallel test's injection; no timer can open the gate early.
impl Drop for IoGate {
    fn drop(&mut self) {
        self.release();
    }
}
impl IoGates {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }
    pub(crate) fn arm(&self, prefix: &str, skip: usize) -> IoGate {
        let (entered, reached) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let mut gates = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|gate| !gate.entered.is_closed());
        assert!(
            !gates.iter().any(|gate| gate.prefix == prefix),
            "duplicate I/O gate"
        );
        gates.push(ArmedGate {
            prefix: prefix.to_owned(),
            skip,
            entered,
            release: wait,
        });
        IoGate {
            entered: Some(reached),
            release: Some(release),
        }
    }
    pub(crate) async fn hold(&self, path: &Path) {
        let gate = {
            let mut gates = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(index) = gates.iter().position(|gate| path.starts_with(&gate.prefix)) else {
                return;
            };
            if gates[index].skip > 0 {
                gates[index].skip -= 1;
                return;
            }
            gates.remove(index)
        };
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
}

#[tokio::test]
async fn gates_are_one_shot_path_scoped_and_released_on_drop() {
    let gates = std::sync::Arc::new(IoGates::new());
    // A failed test may drop its handle before the writer reaches the gate.
    drop(gates.arm("/first", 0));
    let mut first = gates.arm("/first", 1);
    let mut second = gates.arm("/second", 0);
    gates.hold(Path::new("/first-unrelated/file")).await;
    gates.hold(Path::new("/first/skipped")).await;
    let writer = |path: &'static str| {
        let gates = gates.clone();
        tokio::spawn(async move { gates.hold(Path::new(path)).await })
    };
    let a = writer("/first/held");
    let b = writer("/second/held");
    first.entered().await;
    second.entered().await;
    assert!(!a.is_finished());
    assert!(!b.is_finished());
    first.release();
    a.await.unwrap();
    assert!(
        !b.is_finished(),
        "releasing one path must not release its peer"
    );
    drop(second);
    b.await.unwrap();
    gates.hold(Path::new("/first/again")).await;
}
