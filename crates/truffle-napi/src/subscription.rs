//! Disposable handle for JavaScript callback subscriptions.

use napi_derive::napi;
use tokio::task::{AbortHandle, JoinHandle};

/// A live native callback subscription.
///
/// Call `close()` when the listener is no longer needed. Closing is
/// idempotent. For backwards compatibility, ignoring the returned handle
/// leaves the listener active until its owning node/store stops.
#[napi]
pub struct NapiSubscription {
    abort_handle: AbortHandle,
}

impl NapiSubscription {
    pub(crate) fn from_task(handle: JoinHandle<()>) -> Self {
        Self {
            abort_handle: handle.abort_handle(),
        }
    }

    pub(crate) fn abort_handle(&self) -> AbortHandle {
        self.abort_handle.clone()
    }
}

#[napi]
impl NapiSubscription {
    /// Stop forwarding events to the callback. Idempotent.
    #[napi]
    pub fn close(&self) {
        self.abort_handle.abort();
    }
}
