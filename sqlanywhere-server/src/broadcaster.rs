use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap},
    mem,
    sync::Arc,
};

use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::broadcast::{self};
use tokio_stream::wrappers::BroadcastStream;

#[derive(Debug, Copy, Clone, Serialize, Default)]
pub struct BroadcastMsg {
    #[serde(skip_serializing_if = "is_zero")]
    pub unknown: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub delete: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub insert: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub update: u64,
}

#[inline(always)]
fn is_zero(num: &u64) -> bool {
    *num == 0
}

#[derive(Debug, Default)]
pub struct BroadcasterInner {
    state: Mutex<BTreeMap<String, BroadcastMsg>>,
    /// Changes whose commit has begun: the commit hook runs before the commit
    /// is done, so they are sent by `flush` once the statement has finished.
    committed: Mutex<BTreeMap<String, BroadcastMsg>>,
    senders: Mutex<HashMap<String, broadcast::Sender<BroadcastMsg>>>,
}

#[derive(Debug, Default, Clone)]
pub struct Broadcaster {
    inner: Arc<BroadcasterInner>,
}

impl Broadcaster {
    const BROADCAST_CAP: usize = 1024;

    pub fn notify(&self, table: &str, action: rusqlite::hooks::Action) {
        let mut state = self.inner.state.lock();
        if let Some(entry) = state.get_mut(table) {
            Self::increment(entry, action);
        } else {
            let mut entry = BroadcastMsg::default();
            Self::increment(&mut entry, action);
            state.insert(table.into(), entry);
        }
    }

    fn increment(value: &mut BroadcastMsg, action: rusqlite::hooks::Action) {
        match action {
            rusqlite::hooks::Action::SQLITE_DELETE => value.delete += 1,
            rusqlite::hooks::Action::SQLITE_INSERT => value.insert += 1,
            rusqlite::hooks::Action::SQLITE_UPDATE => value.update += 1,
            _ => value.unknown += 1,
        }
    }

    /// Called from the commit hook, before the commit is done: a listener told
    /// now could read the database before the commit lands. The changes wait
    /// for [`flush`](Self::flush).
    pub fn commit(&self) {
        let state = mem::take(&mut *self.inner.state.lock());
        let mut committed = self.inner.committed.lock();
        for (table, entry) in state {
            let pending = committed.entry(table).or_default();
            pending.unknown += entry.unknown;
            pending.delete += entry.delete;
            pending.insert += entry.insert;
            pending.update += entry.update;
        }
    }

    /// Send the committed changes, once the statement that committed them has
    /// finished. A commit that failed after its hook is sent too: a listener
    /// looks again and finds nothing new, which is better than missing one.
    pub fn flush(&self) {
        let committed = mem::take(&mut *self.inner.committed.lock());
        if committed.is_empty() {
            return;
        }
        let senders = self.inner.senders.lock();
        committed.into_iter().for_each(|(table, entry)| {
            if let Some(sender) = senders.get(&table) {
                let _ = sender.send(entry);
            }
        });
    }

    pub fn rollback(&self) {
        self.inner.state.lock().clear();
    }

    pub fn subscribe(&self, table: String) -> BroadcastStream<BroadcastMsg> {
        let receiver = match self.inner.senders.lock().entry(table) {
            Entry::Occupied(entry) => entry.get().subscribe(),
            Entry::Vacant(entry) => {
                let (sender, receiver) = broadcast::channel(Self::BROADCAST_CAP);
                entry.insert(sender);
                receiver
            }
        };

        BroadcastStream::new(receiver)
    }

    pub fn unsubscribe(&self, table: &String) -> bool {
        let mut tables = self.inner.senders.lock();
        if let Some(sender) = tables.get(table) {
            if sender.receiver_count() == 0 {
                tables.remove(table);
                if tables.is_empty() {
                    return false;
                }
            }
        }
        return true;
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::hooks::Action;
    use tokio_stream::StreamExt;

    use super::*;

    #[tokio::test]
    async fn changes_are_sent_when_flushed_not_when_committed() {
        let broadcaster = Broadcaster::default();
        let mut todo = broadcaster.subscribe("todo".into());

        broadcaster.notify("todo", Action::SQLITE_INSERT);
        broadcaster.notify("todo", Action::SQLITE_INSERT);
        // The commit hook: the commit is not done yet.
        broadcaster.commit();
        assert!(futures::FutureExt::now_or_never(todo.next()).is_none());

        // A rolled-back transaction leaves nothing behind.
        broadcaster.notify("todo", Action::SQLITE_DELETE);
        broadcaster.rollback();

        broadcaster.flush();
        let msg = todo.next().await.unwrap().unwrap();
        assert_eq!((msg.insert, msg.delete), (2, 0));
        broadcaster.flush();
        assert!(futures::FutureExt::now_or_never(todo.next()).is_none());
    }
}
