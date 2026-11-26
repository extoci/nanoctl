use std::sync::Mutex;

use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Stored,
    Replaced,
}

/// A bounded asynchronous latest-value slot. It never queues older values: writing a
/// second value before the consumer takes the first replaces it in place.
pub struct LatestSlot<T> {
    state: Mutex<LatestSlotState<T>>,
    notify: Notify,
}

struct LatestSlotState<T> {
    value: Option<T>,
    closed: bool,
}

impl<T> LatestSlot<T> {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LatestSlotState {
                value: None,
                closed: false,
            }),
            notify: Notify::new(),
        }
    }

    pub fn put(&self, value: T) -> PutOutcome {
        let outcome = {
            let mut state = self.state.lock().expect("latest slot poisoned");
            if state.closed {
                return PutOutcome::Replaced;
            }
            let outcome = if state.value.is_some() {
                PutOutcome::Replaced
            } else {
                PutOutcome::Stored
            };
            state.value = Some(value);
            outcome
        };
        self.notify.notify_one();
        outcome
    }

    pub async fn recv(&self) -> Option<T> {
        loop {
            {
                let mut state = self.state.lock().expect("latest slot poisoned");
                if let Some(value) = state.value.take() {
                    return Some(value);
                }
                if state.closed {
                    return None;
                }
            }
            self.notify.notified().await;
        }
    }

    pub fn close(&self) {
        {
            let mut state = self.state.lock().expect("latest slot poisoned");
            state.closed = true;
            state.value = None;
        }
        self.notify.notify_one();
    }

    pub fn len(&self) -> usize {
        usize::from(
            self.state
                .lock()
                .expect("latest slot poisoned")
                .value
                .is_some(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{LatestSlot, PutOutcome};

    #[tokio::test]
    async fn latest_slot_replaces_instead_of_queueing() {
        let slot = LatestSlot::new();
        assert_eq!(slot.put(1), PutOutcome::Stored);
        assert_eq!(slot.put(2), PutOutcome::Replaced);
        assert_eq!(slot.put(3), PutOutcome::Replaced);
        assert_eq!(slot.len(), 1);
        assert_eq!(slot.recv().await, Some(3));
        assert!(slot.is_empty());
    }

    #[tokio::test]
    async fn close_discards_pending_value_and_wakes_consumer() {
        let slot = LatestSlot::new();
        assert_eq!(slot.put(1), PutOutcome::Stored);
        slot.close();
        assert_eq!(slot.recv().await, None);
    }
}
