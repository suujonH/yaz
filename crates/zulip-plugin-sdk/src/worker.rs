use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, sync_channel};

use crate::abi;
use crate::host;

const QUEUE: usize = 50;

type Job = Box<dyn FnOnce() + Send>;

static WORKER: OnceLock<SyncSender<Job>> = OnceLock::new();

/// Runs on the worker thread (in order; the thread is created on the first call); returns false
/// when too many tasks are queued.
pub fn post(job: impl FnOnce() + Send + 'static) -> bool {
    let sender = WORKER.get_or_init(|| {
        let (tx, rx) = sync_channel::<Job>(QUEUE);
        std::thread::spawn(move || {
            for job in rx {
                if catch_unwind(AssertUnwindSafe(job)).is_err() {
                    host::log(abi::LOG_ERROR, "工作线程上的任务发生 panic");
                }
            }
        });
        tx
    });
    sender.try_send(Box::new(job)).is_ok()
}
