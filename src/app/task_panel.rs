use tessora::domain::{TaskEvent, TaskStatus};
use tokio::sync::broadcast;
use topcoat::{
    Result,
    context::Cx,
    runtime::{Event, Signal, connected},
    view::{View, attributes, component, emit, live, view},
};

use crate::components::{
    badge::{BadgeVariant, badge},
    button::{ButtonSize, ButtonVariant, button},
    card::{card, card_content, card_header, card_title},
    progress::progress,
};

use super::{
    actions::{cancel_download, delete_task, retry_download},
    format::status_label,
    manager,
};

#[component]
pub(crate) async fn task_panel_view(cx: &Cx, notice: Signal<String>) -> Result<impl View> {
    Ok(view! {
        card(
            card_header(card_title("下载任务"))
            card_content(
                attrs: attributes! { class="max-h-[60vh] overflow-y-auto" },
                (live! {
                    let task_manager = manager(cx);
                    let mut events = task_manager.subscribe();

                    loop {
                        let tasks = task_manager.list_tasks().await?;
                        let token = emit! {
                            if tasks.is_empty() {
                                <p class="text-sm text-muted-foreground">
                                    "当前没有下载任务"
                                </p>
                            } else {
                                <div class="grid grid-cols-1 gap-3">
                                    #[key(task.id.clone())]
                                    for task in tasks {
                                        let cancel_id = task.id.clone();
                                        let retry_id = task.id.clone();
                                        let delete_id = task.id.clone();

                                        <article
                                            id=(format!("task-{}", task.id))
                                            class="rounded-lg border border-border bg-background p-4"
                                        >
                                            <div class="flex items-start justify-between gap-4">
                                                <div class="min-w-0 flex-1">
                                                    <p class="truncate font-medium">(&task.file_name)</p>
                                                    <p class="truncate text-xs text-muted-foreground">
                                                        (&task.url)
                                                    </p>
                                                </div>

                                                if task.status == TaskStatus::Failed {
                                                    badge(
                                                        variant: BadgeVariant::Destructive,
                                                        (status_label(task.status))
                                                    )
                                                } else {
                                                    badge(
                                                        variant: BadgeVariant::Secondary,
                                                        (status_label(task.status))
                                                    )
                                                }
                                            </div>

                                            if let Some(task_progress) = &task.progress {
                                                <div class="mt-3 grid gap-1">
                                                    progress(
                                                        value: task_progress.percentage,
                                                        attrs: attributes! { aria-label="下载进度" }
                                                    )
                                                    <p class="text-xs text-muted-foreground">
                                                        (format!(
                                                            "{:.1}%",
                                                            task_progress.percentage.unwrap_or_default(),
                                                        ))
                                                    </p>
                                                </div>
                                            }

                                            if let Some(error) = &task.error {
                                                <p class="mt-3 text-sm text-destructive">(error)</p>
                                            }

                                            <div class="mt-4 flex flex-wrap gap-2">
                                                if task.status.is_active() {
                                                    button(
                                                        variant: ButtonVariant::Secondary,
                                                        size: ButtonSize::Sm,
                                                        attrs: attributes! {
                                                            @click=$(async |_event: Event| {
                                                                let result = cancel_download(cancel_id.clone()).await;
                                                                notice.set(result.1);
                                                            })
                                                        },
                                                        "取消"
                                                    )
                                                }

                                                if task.status.can_retry() {
                                                    button(
                                                        size: ButtonSize::Sm,
                                                        attrs: attributes! {
                                                            @click=$(async |_event: Event| {
                                                                let result = retry_download(retry_id.clone()).await;
                                                                notice.set(result.1);
                                                            })
                                                        },
                                                        "重新下载"
                                                    )
                                                }

                                                if !task.status.is_active() {
                                                    button(
                                                        variant: ButtonVariant::Ghost,
                                                        size: ButtonSize::Sm,
                                                        attrs: attributes! {
                                                            @click=$(async |_event: Event| {
                                                                let result = delete_task(delete_id.clone()).await;
                                                                notice.set(result.1);
                                                            })
                                                        },
                                                        "删除记录"
                                                    )
                                                }
                                            </div>
                                        </article>
                                    }
                                </div>
                            }
                        }?;

                        if !connected(cx) {
                            break Ok(token);
                        }
                        if !wait_for_task_change(&mut events).await {
                            break Ok(token);
                        }
                    }
                })
            )
        )
    })
}

async fn wait_for_task_change(events: &mut broadcast::Receiver<TaskEvent>) -> bool {
    loop {
        match events.recv().await {
            Ok(TaskEvent::Progress { .. } | TaskEvent::Changed { .. }) => return true,
            Ok(TaskEvent::MediaChanged) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => return true,
            Err(broadcast::error::RecvError::Closed) => return false,
        }
    }
}
