//! Bounded upload admission and FIFO, round-robin scheduling between peers.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Display,
    future::Future,
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    sync::{mpsc, OwnedSemaphorePermit, Semaphore},
    task::{Id, JoinHandle, JoinSet},
};

const MAX_ACTIVE_UPLOADS: usize = 4;
const MAX_QUEUED_UPLOADS: usize = 64;
const MAX_QUEUED_UPLOADS_PER_PEER: usize = 8;

struct Admission {
    global: Arc<Semaphore>,
    peers: Mutex<HashMap<String, Weak<Semaphore>>>,
}

pub(super) struct UploadSender<T> {
    tx: mpsc::Sender<Queued<T>>,
    admission: Arc<Admission>,
}

impl<T> Clone for UploadSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            admission: self.admission.clone(),
        }
    }
}

impl<T> UploadSender<T> {
    pub(super) async fn send(&self, peer: String, item: T) -> Result<(), ()> {
        let per_peer = {
            let mut peers = self.admission.peers.lock().unwrap();
            peers.retain(|_, semaphore| semaphore.strong_count() > 0);
            match peers.get(&peer).and_then(Weak::upgrade) {
                Some(semaphore) => semaphore,
                None => {
                    let semaphore = Arc::new(Semaphore::new(MAX_QUEUED_UPLOADS_PER_PEER));
                    peers.insert(peer.clone(), Arc::downgrade(&semaphore));
                    semaphore
                }
            }
        };
        // Peer admission comes first so one peer cannot reserve the global queue.
        let admit = async {
            let peer_permit = per_peer.acquire_owned().await.map_err(|_| ())?;
            let global_permit = self
                .admission
                .global
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ())?;
            self.tx
                .send(Queued {
                    peer,
                    item,
                    _permits: [peer_permit, global_permit],
                })
                .await
                .map_err(|_| ())
        };
        tokio::select! {
            result = admit => result,
            _ = self.tx.closed() => Err(()),
        }
    }
}

struct Queued<T> {
    peer: String,
    item: T,
    // Held across both the channel and scheduler queue; released on dispatch/drop.
    _permits: [OwnedSemaphorePermit; 2],
}

struct FairQueue<T> {
    pending: HashMap<String, VecDeque<Queued<T>>>,
    ready: VecDeque<String>,
    active: HashSet<String>,
}

impl<T> FairQueue<T> {
    fn new() -> Self {
        Self {
            pending: HashMap::new(),
            ready: VecDeque::new(),
            active: HashSet::new(),
        }
    }

    fn push(&mut self, request: Queued<T>) {
        let peer = request.peer.clone();
        let queue = self.pending.entry(peer.clone()).or_default();
        if queue.is_empty() && !self.active.contains(&peer) {
            self.ready.push_back(peer);
        }
        queue.push_back(request);
    }

    fn pop(&mut self) -> Option<Queued<T>> {
        if self.active.len() >= MAX_ACTIVE_UPLOADS {
            return None;
        }
        let peer = self.ready.pop_front()?;
        let queue = self.pending.get_mut(&peer).unwrap();
        let request = queue.pop_front().unwrap();
        if queue.is_empty() {
            self.pending.remove(&peer);
        }
        self.active.insert(peer);
        Some(request)
    }

    fn completed(&mut self, peer: &str) {
        self.active.remove(peer);
        if self.pending.contains_key(peer) {
            self.ready.push_back(peer.to_owned());
        }
    }
}

pub(super) fn spawn<T, F, Fut, E>(worker: F) -> (UploadSender<T>, JoinHandle<()>)
where
    T: Send + 'static,
    F: Fn(T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), E>> + Send + 'static,
    E: Display + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<Queued<T>>(MAX_QUEUED_UPLOADS);
    let sender = UploadSender {
        tx,
        admission: Arc::new(Admission {
            global: Arc::new(Semaphore::new(MAX_QUEUED_UPLOADS)),
            peers: Mutex::new(HashMap::new()),
        }),
    };
    let worker = Arc::new(worker);
    let handle = tokio::spawn(async move {
        let mut queue = FairQueue::new();
        let mut tasks = JoinSet::new();
        let mut task_peers: HashMap<Id, String> = HashMap::new();
        let mut accepting = true;
        loop {
            // Consider all admitted peers before assigning the next free slots.
            while let Ok(request) = rx.try_recv() {
                queue.push(request);
            }
            while let Some(Queued {
                peer,
                item,
                _permits,
            }) = queue.pop()
            {
                drop(_permits);
                // Invoke the worker inside the task so a panic is isolated too.
                let worker = worker.clone();
                let task = tasks.spawn(async move { worker(item).await });
                task_peers.insert(task.id(), peer);
            }
            if !accepting && tasks.is_empty() {
                break;
            }
            tokio::select! {
                request = rx.recv(), if accepting => match request {
                    Some(request) => queue.push(request),
                    None => accepting = false,
                },
                Some(result) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    let id = match result {
                        Ok((id, result)) => {
                            if let Err(error) = result {
                                log::warn!("Error uploading: {error}");
                            }
                            id
                        }
                        Err(error) => {
                            log::warn!("Upload task failed: {error}");
                            error.id()
                        }
                    };
                    let peer = task_peers.remove(&id).unwrap();
                    queue.completed(&peer);
                }
            }
        }
    });
    (sender, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use tokio::sync::oneshot;

    fn queued(peer: &str, item: usize) -> Queued<usize> {
        Queued {
            peer: peer.into(),
            item,
            _permits: [
                Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
                Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap(),
            ],
        }
    }

    #[test]
    fn fifo_round_robin_and_active_limits() {
        let mut queue = FairQueue::new();
        queue.push(queued("a", 1));
        queue.push(queued("a", 2));
        queue.push(queued("a", 3));
        for peer in ["b", "c", "d", "e"] {
            queue.push(queued(peer, 0));
        }
        for peer in ["a", "b", "c", "d"] {
            assert_eq!(queue.pop().unwrap().peer, peer);
        }
        assert!(queue.pop().is_none(), "global active limit must hold");
        queue.completed("a");
        assert_eq!(queue.pop().unwrap().peer, "e", "waiting peer goes first");
        queue.completed("b");
        let next = queue.pop().unwrap();
        assert_eq!((next.peer.as_str(), next.item), ("a", 2));
        assert!(queue.pop().is_none());
        queue.completed("c");
        assert!(queue.pop().is_none(), "a cannot have two active uploads");
        queue.completed("a");
        assert_eq!(queue.pop().unwrap().item, 3);
    }

    struct Job {
        started: oneshot::Sender<()>,
        finish: oneshot::Receiver<Result<(), &'static str>>,
        panic: bool,
    }

    fn job() -> (
        Job,
        oneshot::Receiver<()>,
        oneshot::Sender<Result<(), &'static str>>,
    ) {
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        (
            Job {
                started: started_tx,
                finish: finish_rx,
                panic: false,
            },
            started_rx,
            finish_tx,
        )
    }

    fn scheduler() -> (UploadSender<Job>, JoinHandle<()>) {
        spawn(|job: Job| async move {
            let _ = job.started.send(());
            let result = job.finish.await.unwrap_or(Ok(()));
            assert!(!job.panic, "simulated upload panic");
            result
        })
    }

    #[tokio::test]
    async fn other_peer_runs_and_errors_and_panics_release_the_peer() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (sender, handle) = scheduler();
            let (first, started, finish) = job();
            sender.send("a".into(), first).await.unwrap();
            started.await.unwrap();
            let (mut second, mut second_started, second_finish) = job();
            second.panic = true;
            sender.send("a".into(), second).await.unwrap();
            let (third, third_started, third_finish) = job();
            sender.send("b".into(), third).await.unwrap();
            third_started.await.unwrap();
            assert!((&mut second_started).now_or_never().is_none());
            third_finish.send(Ok(())).unwrap();
            finish.send(Err("simulated read error")).unwrap();
            second_started.await.unwrap();
            let (fourth, fourth_started, fourth_finish) = job();
            sender.send("a".into(), fourth).await.unwrap();
            second_finish.send(Ok(())).unwrap();
            fourth_started.await.unwrap();
            // Closing submission still drains admitted work.
            drop(sender);
            fourth_finish.send(Ok(())).unwrap();
            handle.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn queue_limits_cover_channel_and_scheduler_and_canceled_admission() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (sender, handle) = scheduler();
            let mut finishes = Vec::new();
            for peer in ["a", "b", "c", "d"] {
                let (job, started, finish) = job();
                sender.send(peer.into(), job).await.unwrap();
                started.await.unwrap();
                finishes.push(finish);
            }
            for peer in ["a", "b", "c", "d", "e", "f", "g", "h"] {
                for _ in 0..MAX_QUEUED_UPLOADS_PER_PEER {
                    let (job, _, _) = job();
                    sender.send(peer.into(), job).await.unwrap();
                }
            }
            assert_eq!(sender.admission.global.available_permits(), 0);
            let (extra, _, _) = job();
            let mut blocked = Box::pin(sender.send("a".into(), extra));
            assert!(blocked.as_mut().now_or_never().is_none());
            drop(blocked);
            let (extra, _, _) = job();
            let mut blocked = Box::pin(sender.send("new".into(), extra));
            assert!(blocked.as_mut().now_or_never().is_none());
            drop(blocked);
            // Abandoning global admission also releases its peer reservation.
            assert!(sender
                .admission
                .peers
                .lock()
                .unwrap()
                .get("new")
                .unwrap()
                .upgrade()
                .is_none());
            finishes.remove(0).send(Ok(())).unwrap();
            // Waiting admission resumes as soon as a queued request is dispatched.
            let (extra, _, _) = job();
            sender.send("new".into(), extra).await.unwrap();
            drop(finishes);
            drop(sender);
            handle.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn scheduler_abort_drops_active_work_and_releases_waiting_admission() {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (sender, handle) = scheduler();
            let (first, started, mut finish) = job();
            sender.send("a".into(), first).await.unwrap();
            started.await.unwrap();
            for _ in 0..MAX_QUEUED_UPLOADS_PER_PEER {
                let (job, _, _) = job();
                sender.send("a".into(), job).await.unwrap();
            }
            let (extra, _, _) = job();
            let mut blocked = Box::pin(sender.send("a".into(), extra));
            assert!(blocked.as_mut().now_or_never().is_none());
            handle.abort();
            assert!(handle.await.unwrap_err().is_cancelled());
            assert!(blocked.await.is_err());
            finish.closed().await;
            assert_eq!(
                sender.admission.global.available_permits(),
                MAX_QUEUED_UPLOADS
            );
        })
        .await
        .unwrap();
    }
}
