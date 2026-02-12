use std::{collections::HashMap, sync::Arc};

use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::debug;

use crate::{
    backend::{BackendError, InferenceBackend},
    models::{BackendChatResponse, BackendChunk, NormalizedChatRequest},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceOutcome {
    Leader,
    Joined,
}

#[derive(Debug, Default)]
pub struct InflightCoalescer {
    inflight: Mutex<HashMap<String, Vec<oneshot::Sender<Result<BackendChatResponse, String>>>>>,
    stream_inflight: Mutex<HashMap<String, Arc<Mutex<StreamEntry>>>>,
}

impl InflightCoalescer {
    pub async fn execute_or_join(
        &self,
        key: String,
        backend: Arc<dyn InferenceBackend>,
        request: NormalizedChatRequest,
    ) -> Result<(BackendChatResponse, CoalesceOutcome), BackendError> {
        let receiver = {
            let mut inflight = self.inflight.lock().await;
            if let Some(waiters) = inflight.get_mut(&key) {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                Some(rx)
            } else {
                inflight.insert(key.clone(), Vec::new());
                None
            }
        };

        if let Some(receiver) = receiver {
            debug!(fingerprint = %key, "joined inflight request");
            return match receiver.await {
                Ok(Ok(response)) => Ok((response, CoalesceOutcome::Joined)),
                Ok(Err(message)) => Err(BackendError::Unavailable(message)),
                Err(_) => Err(BackendError::Unavailable(
                    "leader request dropped before completion".to_owned(),
                )),
            };
        }

        debug!(fingerprint = %key, "leader executing request");
        let leader_result = backend.execute_chat(request).await;

        let follower_result = match &leader_result {
            Ok(response) => Ok(response.clone()),
            Err(error) => Err(error.to_string()),
        };

        let waiters = {
            let mut inflight = self.inflight.lock().await;
            inflight.remove(&key).unwrap_or_default()
        };

        for waiter in waiters {
            let _ = waiter.send(follower_result.clone());
        }

        leader_result.map(|response| (response, CoalesceOutcome::Leader))
    }

    pub async fn join_or_create_stream(&self, key: String) -> StreamJoin {
        let (entry, is_leader) = {
            let mut streams = self.stream_inflight.lock().await;
            if let Some(entry) = streams.get(&key) {
                (entry.clone(), false)
            } else {
                let entry = Arc::new(Mutex::new(StreamEntry::default()));
                streams.insert(key.clone(), entry.clone());
                (entry, true)
            }
        };

        let mut entry_guard = entry.lock().await;
        let (tx, rx) = mpsc::unbounded_channel();
        for item in &entry_guard.history {
            if tx.send(item.clone()).is_err() {
                break;
            }
        }
        if !entry_guard.done {
            entry_guard.subscribers.push(tx);
        }
        drop(entry_guard);

        StreamJoin { receiver: rx, is_leader }
    }

    pub async fn publish_stream_item(&self, key: &str, item: StreamItem) {
        let Some(entry) = self.stream_inflight.lock().await.get(key).cloned() else {
            return;
        };

        let mut entry_guard = entry.lock().await;
        if entry_guard.done {
            return;
        }

        entry_guard.history.push(item.clone());
        entry_guard
            .subscribers
            .retain(|subscriber| subscriber.send(item.clone()).is_ok());

        if is_terminal_item(&item) {
            entry_guard.done = true;
            entry_guard.subscribers.clear();
        }
        let should_remove = entry_guard.done;
        drop(entry_guard);

        if should_remove {
            self.stream_inflight.lock().await.remove(key);
        }
    }
}

#[derive(Debug)]
pub struct StreamJoin {
    pub receiver: mpsc::UnboundedReceiver<StreamItem>,
    pub is_leader: bool,
}

pub type StreamItem = Result<BackendChunk, String>;

#[derive(Debug, Default)]
struct StreamEntry {
    history: Vec<StreamItem>,
    subscribers: Vec<mpsc::UnboundedSender<StreamItem>>,
    done: bool,
}

fn is_terminal_item(item: &StreamItem) -> bool {
    match item {
        Ok(chunk) => chunk.done,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        backend::mock::MockBackend,
        models::{BackendChunk, GenerationParams, MessageRole, NormalizedChatRequest, NormalizedMessage},
    };

    use super::{CoalesceOutcome, InflightCoalescer};

    #[tokio::test]
    async fn coalesces_identical_one_shot_requests() {
        let coalescer = Arc::new(InflightCoalescer::default());
        let backend = Arc::new(MockBackend::default());

        let request = NormalizedChatRequest {
            request_id: "req_1".to_owned(),
            user_id: "user_1".to_owned(),
            model: "mock".to_owned(),
            messages: vec![NormalizedMessage {
                role: MessageRole::User,
                content: "hello".to_owned(),
            }],
            generation: GenerationParams {
                max_tokens: Some(20),
                temperature: None,
                top_p: None,
            },
            stream: false,
        };

        let key = "same".to_owned();
        let key_for_first = key.clone();
        let first = {
            let coalescer = Arc::clone(&coalescer);
            let backend = backend.clone();
            let request = request.clone();
            tokio::spawn(async move {
                coalescer
                    .execute_or_join(key_for_first, backend, request)
                    .await
            })
        };

        let second = {
            let coalescer = Arc::clone(&coalescer);
            let backend = backend.clone();
            tokio::spawn(async move { coalescer.execute_or_join(key, backend, request).await })
        };

        let first = first.await.expect("first task should run").expect("first result");
        let second = second.await.expect("second task should run").expect("second result");

        assert_ne!(first.1, second.1);
        assert!(
            (first.1 == CoalesceOutcome::Leader && second.1 == CoalesceOutcome::Joined)
                || (first.1 == CoalesceOutcome::Joined && second.1 == CoalesceOutcome::Leader)
        );
        assert_eq!(first.0.content, second.0.content);
    }

    #[tokio::test]
    async fn stream_joiner_receives_history_and_live_updates() {
        let coalescer = InflightCoalescer::default();
        let key = "stream-key".to_owned();

        let leader = coalescer.join_or_create_stream(key.clone()).await;
        assert!(leader.is_leader);

        coalescer
            .publish_stream_item(
                &key,
                Ok(BackendChunk {
                    delta: Some("hello ".to_owned()),
                    finish_reason: None,
                    usage: None,
                    done: false,
                }),
            )
            .await;

        let follower = coalescer.join_or_create_stream(key.clone()).await;
        assert!(!follower.is_leader);

        coalescer
            .publish_stream_item(
                &key,
                Ok(BackendChunk {
                    delta: Some("world".to_owned()),
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                    done: true,
                }),
            )
            .await;

        let mut follower_rx = follower.receiver;
        let first = follower_rx
            .recv()
            .await
            .expect("follower should receive first replayed chunk")
            .expect("first chunk should be ok");
        let second = follower_rx
            .recv()
            .await
            .expect("follower should receive second live chunk")
            .expect("second chunk should be ok");

        assert_eq!(first.delta.as_deref(), Some("hello "));
        assert_eq!(second.delta.as_deref(), Some("world"));
        assert!(second.done);
    }
}
