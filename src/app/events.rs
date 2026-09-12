use std::collections::HashMap;

use dioxus::prelude::*;

use crate::contracts::tasks::TaskProgress;

#[cfg(feature = "web")]
use crate::contracts::tasks::TaskEvent;

#[cfg(feature = "web")]
use crate::app::api;

#[derive(Clone, Copy)]
pub struct LiveTaskEvents {
    progress: Signal<HashMap<String, TaskProgress>>,
    connected: Signal<bool>,
}

impl LiveTaskEvents {
    pub fn progress(&self, id: &str) -> Option<TaskProgress> {
        self.progress.read().get(id).cloned()
    }

    pub fn connected(&self) -> bool {
        (self.connected)()
    }
}

pub fn use_task_events(refresh: Signal<u64>) -> LiveTaskEvents {
    let progress = use_signal(HashMap::new);
    let connected = use_signal(|| false);

    #[cfg(feature = "web")]
    let mut progress_task = progress;
    #[cfg(feature = "web")]
    let mut connected_task = connected;
    #[cfg(feature = "web")]
    let mut refresh = refresh;
    #[cfg(not(feature = "web"))]
    let _ = refresh;

    let _events_task = use_future(move || async move {
        #[cfg(feature = "web")]
        {
            let mut retry_ms = INITIAL_RETRY_MS;
            loop {
                if let Ok(mut events) = api::task_events().await {
                    retry_ms = INITIAL_RETRY_MS;
                    connected_task.set(true);
                    progress_task.write().clear();
                    *refresh.write() += 1;

                    while let Some(event) = events.recv().await {
                        match event {
                            Ok(TaskEvent::Progress { id, progress }) => {
                                progress_task.write().insert(id, progress);
                            }
                            Ok(TaskEvent::Changed { id }) => {
                                progress_task.write().remove(&id);
                                *refresh.write() += 1;
                            }
                            Err(_) => break,
                        }
                    }
                    connected_task.set(false);
                }

                gloo_timers::future::TimeoutFuture::new(retry_ms).await;
                retry_ms = retry_ms.saturating_mul(2).min(MAX_RETRY_MS);
            }
        }

        #[cfg(not(feature = "web"))]
        std::future::pending::<()>().await;
    });

    LiveTaskEvents {
        progress,
        connected,
    }
}

#[cfg(feature = "web")]
const INITIAL_RETRY_MS: u32 = 1_000;
#[cfg(feature = "web")]
const MAX_RETRY_MS: u32 = 30_000;
