use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::contracts::tasks::{TaskEvent, TaskProgress};

const TASK_EVENT_CAPACITY: usize = 64;

#[derive(Debug, Clone)]
pub struct TaskEvents {
    events: broadcast::Sender<TaskEvent>,
    shutdown: CancellationToken,
}

impl Default for TaskEvents {
    fn default() -> Self {
        let (events, _) = broadcast::channel(TASK_EVENT_CAPACITY);
        Self {
            events,
            shutdown: CancellationToken::new(),
        }
    }
}

impl TaskEvents {
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn progress(&self, id: &str, progress: TaskProgress) {
        let _ = self.events.send(TaskEvent::Progress {
            id: id.to_owned(),
            progress,
        });
    }

    pub fn changed(&self, id: &str) {
        let _ = self.events.send(TaskEvent::Changed { id: id.to_owned() });
    }

    pub fn close_streams(&self) {
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_progress_and_change_events() {
        let events = TaskEvents::default();
        let mut receiver = events.subscribe();
        let progress = TaskProgress {
            processed_duration_ms: 2_000,
            total_duration_ms: Some(10_000),
            percentage: Some(20.0),
        };

        events.progress("task-1", progress.clone());
        events.changed("task-1");

        assert_eq!(
            receiver.try_recv().unwrap(),
            TaskEvent::Progress {
                id: "task-1".into(),
                progress,
            }
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            TaskEvent::Changed {
                id: "task-1".into(),
            }
        );
    }

    #[test]
    fn late_shutdown_subscribers_are_also_cancelled() {
        let events = TaskEvents::default();
        events.close_streams();

        assert!(events.shutdown_token().is_cancelled());
    }
}
