use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use leviculum_core::link::LinkId;

use crate::sync_ext::MutexRecover;

/// Only live LinkHandles own notifications; this node-local index holds
/// weak references and never retains a link or its payloads.
#[derive(Default)]
pub(super) struct SendWindowRegistry {
    windows: Mutex<HashMap<LinkId, (Weak<tokio::sync::Notify>, usize)>>,
}

impl SendWindowRegistry {
    pub(super) fn new() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn get_or_create(&self, link_id: LinkId) -> Arc<tokio::sync::Notify> {
        let mut windows = self.windows.lock_recover();
        if let Some((weak, handles)) = windows.get_mut(&link_id) {
            if let Some(window) = weak.upgrade() {
                *handles += 1;
                return window;
            }
        }
        let window = Arc::new(tokio::sync::Notify::new());
        windows.insert(link_id, (Arc::downgrade(&window), 1));
        window
    }

    pub(super) fn release(&self, link_id: LinkId, window: &Arc<tokio::sync::Notify>) {
        let mut windows = self.windows.lock_recover();
        if let Some((weak, handles)) = windows.get_mut(&link_id) {
            if weak.ptr_eq(&Arc::downgrade(window)) {
                // Count handles, not Arc references: a concurrent wake may
                // temporarily own an Arc when the final handle is dropped.
                *handles -= 1;
                if *handles == 0 {
                    windows.remove(&link_id);
                }
            }
        }
    }

    pub(super) fn wake(&self, link_id: LinkId) {
        let window = self
            .windows
            .lock_recover()
            .get(&link_id)
            .and_then(|(w, _)| w.upgrade());
        if let Some(window) = window {
            window.notify_waiters();
        }
    }

    pub(super) fn wake_all(&self) {
        let windows: Vec<_> = self
            .windows
            .lock_recover()
            .values()
            .filter_map(|(w, _)| w.upgrade())
            .collect();
        for window in windows {
            window.notify_waiters();
        }
    }

    #[cfg(test)]
    pub(super) fn contains(&self, link_id: &LinkId) -> bool {
        self.windows.lock_recover().contains_key(link_id)
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.windows.lock_recover().is_empty()
    }
}
