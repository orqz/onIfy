//! The background runtime for networking and playback control.
//!
//! The GTK main thread never waits on the network: work is spawned here and
//! its result awaited from the UI's own executor.

use std::future::Future;
use std::sync::LazyLock;

use tokio::runtime::Runtime;

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("onify-net")
        .enable_all()
        .build()
        .expect("start tokio runtime")
});

pub fn handle() -> &'static Runtime {
    &RUNTIME
}

/// Runs `fut` on the background runtime; await the result from the UI thread.
pub fn spawn<T: Send + 'static>(
    fut: impl Future<Output = T> + Send + 'static,
) -> impl Future<Output = T> {
    let task = RUNTIME.spawn(fut);
    async move { task.await.expect("background task panicked") }
}
