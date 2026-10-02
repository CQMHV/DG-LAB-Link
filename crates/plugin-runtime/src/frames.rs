use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dg_lab_link_plugin_sdk::{Binding, Frame, FrameNotification, MAX_BINDINGS};

#[derive(Default)]
struct Entry {
    source_id: String,
    generation: u64,
    active: bool,
    sequence: u64,
    frame: Option<Frame>,
    received: Option<Instant>,
}

/// Shared, short-lock, latest-only mailbox. Never performs device or process I/O.
#[derive(Clone, Default)]
pub struct LatestFrameStore(
    Arc<Mutex<HashMap<String, Entry>>>,
    Arc<Mutex<HashMap<String, String>>>,
);

impl LatestFrameStore {
    pub fn take(&self, binding_id: &str, generation: u64) -> Option<Frame> {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = entries.get_mut(binding_id)?;
        if entry.generation != generation || !entry.active {
            return None;
        }
        if entry
            .received
            .is_none_or(|received| received.elapsed() >= std::time::Duration::from_millis(500))
        {
            entry.frame = None;
            return None;
        }
        entry.frame.take()
    }

    pub fn last_received(&self, binding_id: &str, generation: u64) -> Option<Instant> {
        let entries = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = entries.get(binding_id)?;
        (entry.generation == generation && entry.active)
            .then_some(entry.received)
            .flatten()
    }

    pub fn invalidate(&self, binding_id: &str, generation: u64) {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = entries.get_mut(binding_id) {
            entry.generation = generation;
            entry.frame = None;
            entry.received = None;
            entry.sequence = 0;
            entry.active = false;
        }
    }

    pub(crate) fn replace_bindings(&self, source_id: &str, bindings: &[Binding]) {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|id, entry| {
            entry.source_id != source_id || bindings.iter().any(|binding| &binding.binding_id == id)
        });
        for binding in bindings {
            if !entries.contains_key(&binding.binding_id) && entries.len() >= MAX_BINDINGS {
                continue;
            }
            let entry = entries.entry(binding.binding_id.clone()).or_default();
            if entry.source_id != source_id
                || entry.generation != binding.generation
                || entry.active != binding.active
            {
                *entry = Entry {
                    source_id: source_id.to_owned(),
                    generation: binding.generation,
                    active: binding.active,
                    ..Entry::default()
                };
            }
        }
    }

    pub(crate) fn push(&self, source_id: &str, notification: FrameNotification) -> bool {
        if notification.frame.validate().is_err() {
            return false;
        }
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(entry) = entries.get_mut(&notification.binding_id) else {
            return false;
        };
        if entry.source_id != source_id
            || !entry.active
            || entry.generation != notification.generation
            || notification.sequence <= entry.sequence
        {
            return false;
        }
        entry.sequence = notification.sequence;
        entry.frame = Some(notification.frame);
        entry.received = Some(Instant::now());
        true
    }

    pub(crate) fn begin_source(&self, source_id: &str, lease: &str) {
        let mut leases = self
            .1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        leases.insert(source_id.to_owned(), lease.to_owned());
        self.clear_source(source_id);
    }

    pub(crate) fn push_leased(
        &self,
        source_id: &str,
        lease: &str,
        notification: FrameNotification,
    ) -> bool {
        let leases = self
            .1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if leases.get(source_id).is_none_or(|active| active != lease) {
            return false;
        }
        self.push(source_id, notification)
    }

    pub(crate) fn clear_leased(&self, source_id: &str, lease: &str) {
        let mut leases = self
            .1
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if leases.get(source_id).is_some_and(|active| active == lease) {
            leases.remove(source_id);
            self.clear_source(source_id);
        }
    }

    pub(crate) fn clear_source(&self, source_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|_, entry| entry.source_id != source_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_lab_link_plugin_sdk::Channel;
    use serde_json::json;

    #[test]
    fn does_not_replay_an_expired_frame_after_consumer_backpressure() {
        let store = LatestFrameStore::default();
        store.replace_bindings(
            "source",
            &[Binding {
                binding_id: "device/a".into(),
                control_id: "device".into(),
                channel: Channel::A,
                generation: 1,
                config: json!({}),
                active: true,
            }],
        );
        assert!(store.push(
            "source",
            FrameNotification {
                binding_id: "device/a".into(),
                generation: 1,
                sequence: 1,
                frame: Frame::silent()
            }
        ));
        store
            .0
            .lock()
            .unwrap()
            .get_mut("device/a")
            .unwrap()
            .received = Some(Instant::now() - std::time::Duration::from_millis(501));
        assert!(store.take("device/a", 1).is_none());
    }

    #[test]
    fn consumes_once_rejects_late_generation_and_other_source() {
        let store = LatestFrameStore::default();
        let mut binding = Binding {
            binding_id: "device/a".into(),
            control_id: "device".into(),
            channel: Channel::A,
            generation: 2,
            config: json!({}),
            active: true,
        };
        store.replace_bindings("source", &[binding.clone()]);
        let notification = FrameNotification {
            binding_id: binding.binding_id.clone(),
            generation: 2,
            sequence: 3,
            frame: Frame::silent(),
        };
        assert!(!store.push("other", notification.clone()));
        assert!(store.push("source", notification.clone()));
        assert!(store.take(&binding.binding_id, 2).is_some());
        assert!(store.take(&binding.binding_id, 2).is_none());
        assert!(!store.push("source", notification.clone()));
        binding.generation = 3;
        store.replace_bindings("source", &[binding.clone()]);
        assert!(!store.push("source", notification));
        assert!(store.last_received(&binding.binding_id, 3).is_none());
        let notification = FrameNotification {
            binding_id: binding.binding_id.clone(),
            generation: 3,
            sequence: 4,
            frame: Frame::silent(),
        };
        assert!(store.push("source", notification));
        binding.active = false;
        store.replace_bindings("source", &[binding.clone()]);
        assert!(store.take(&binding.binding_id, 3).is_none());
    }
}
