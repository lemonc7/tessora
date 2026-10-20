use tokio::sync::broadcast;

use crate::domain::{TaskEvent, TaskProgress};

const TASK_EVENT_CAPACITY: usize = 64;

#[derive(Debug, Clone)]
pub struct TaskEvents {
    events: broadcast::Sender<TaskEvent>,
}

impl Default for TaskEvents {
    fn default() -> Self {
        let (events, _) = broadcast::channel(TASK_EVENT_CAPACITY);
        Self { events }
    }
}

impl TaskEvents {
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.events.subscribe()
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

    pub fn media_changed(&self) {
        let _ = self.events.send(TaskEvent::MediaChanged);
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

        events.media_changed();
        assert_eq!(receiver.try_recv().unwrap(), TaskEvent::MediaChanged);
    }
}
