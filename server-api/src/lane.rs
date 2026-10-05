// Requests on one connection run concurrently (tcp.rs, ws.rs), so they can
// finish in any order. A chunked upload's parts can't: each UploadChunk
// must land right where the previous one ended. Instead of making clients
// wait for every chunk's answer before sending the next, a connection's
// upload requests go through one lane that runs them in arrival order,
// while the client already sends the next chunk.

use std::future::Future;
use std::pin::Pin;

use lestobackupper_schema::protocol::RequestBody;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Whether `body`'s effect depends on the requests before it on the same connection.
pub fn must_run_in_order(body: &RequestBody) -> bool {
    matches!(body, RequestBody::BeginUploadRequest(_) | RequestBody::UploadChunkRequest(_) | RequestBody::CommitUploadRequest(_))
}

/// Runs jobs one at a time, in the order they were pushed.
pub struct OrderedLane {
    tx: mpsc::UnboundedSender<Job>,
    task: JoinHandle<()>,
}

impl OrderedLane {
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let task = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                job.await;
            }
        });
        OrderedLane { tx, task }
    }

    pub fn push(&self, job: impl Future<Output = ()> + Send + 'static) {
        // Only fails once the task is gone, which only `finish` does.
        let _ = self.tx.send(Box::pin(job));
    }

    /// Waits for every pushed job to finish.
    pub async fn finish(self) {
        let OrderedLane { tx, task } = self;
        {
            let _closing = tx;
        }
        let _ = task.await;
    }
}

impl Default for OrderedLane {
    fn default() -> Self {
        Self::new()
    }
}
