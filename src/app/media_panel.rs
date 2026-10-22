use tokio::sync::broadcast;
use topcoat::{
    Error, Result,
    context::Cx,
    runtime::{Event, Signal, connected, shard, signal},
    view::{View, attributes, component, emit, live, view},
};

use crate::{
    components::{
        button::{ButtonSize, ButtonVariant, button},
        card::{card, card_content, card_header, card_title},
        dialog::{
            dialog, dialog_content, dialog_description, dialog_footer, dialog_header, dialog_title,
        },
        input::input,
    },
    domain::TaskEvent,
};

use super::{
    actions::{delete_media, scan_media},
    format::{format_size, stable_dom_id},
    manager,
};

#[component]
pub(crate) async fn media_panel_view(cx: &Cx, notice: Signal<String>) -> Result<impl View> {
    let query = signal(cx, String::new);
    let submitted_query = signal(cx, String::new);
    let confirming = signal(cx, || false);
    let pending_directory = signal(cx, String::new);
    let pending_file_name = signal(cx, String::new);
    let pending_path = signal(cx, String::new);

    Ok(view! {
        card(
            card_header(
                <div class="flex items-center justify-between gap-4">
                    card_title("媒体文件")
                    button(
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        attrs: attributes! {
                            @click=$(async |_event: Event| {
                                let result = scan_media().await;
                                notice.set(result.1);
                            })
                        },
                        "重新扫描"
                    )
                </div>
            )
            card_content(
                <div class="grid gap-4">
                    <div class="flex gap-2">
                        input(
                            attrs: attributes! {
                                type="search"
                                aria-label="媒体搜索关键词"
                                placeholder="输入文件名"
                                :value=$(query.get())
                                @input=$(|event: Event| query.set(event.target.value))
                            }
                        )
                        button(
                            attrs: attributes! {
                                type="button"
                                :disabled=$(query.get().trim().is_empty())
                                @click=$(|_event: Event| {
                                    let normalized = query.get().trim().to_owned();
                                    if normalized.is_empty() {
                                        notice.set("请输入搜索关键词".to_owned());
                                    } else {
                                        submitted_query.set(normalized);
                                    }
                                })
                            },
                            "搜索"
                        )
                    </div>

                    <div class="max-h-[60vh] overflow-y-auto">
                        media_results(
                            query: $(submitted_query.get()),
                            confirming: confirming.clone(),
                            pending_directory: pending_directory.clone(),
                            pending_file_name: pending_file_name.clone(),
                            pending_path: pending_path.clone()
                        )
                    </div>
                </div>
            )
        )

        dialog(
            open: $(confirming.get()),
            attrs: attributes! { aria-label="确认删除媒体文件" },
            dialog_content(
                dialog_header(
                    dialog_title("删除媒体文件？")
                    dialog_description(
                        "这个操作会删除磁盘上的 MP4 文件，无法通过 Tessora 恢复。"
                    )
                )
                <p class="break-all rounded-md bg-foreground/5 p-3 text-sm">
                    $(pending_path.get())
                </p>
                dialog_footer(
                    button(
                        variant: ButtonVariant::Ghost,
                        attrs: attributes! { @click=$(|_event: Event| confirming.set(false)) },
                        "取消"
                    )
                    button(
                        variant: ButtonVariant::Destructive,
                        attrs: attributes! {
                            @click=$(async |_event: Event| {
                                let result = delete_media(
                                    pending_directory.get(),
                                    pending_file_name.get(),
                                ).await;
                                confirming.set(false);
                                notice.set(result.1);
                            })
                        },
                        "确认删除"
                    )
                )
            )
        )
    })
}

// shard是局部页面单元，有独立更新边界和地址的服务端局部视图
#[shard("/_ui/media-results")]
async fn media_results(
    cx: &Cx,
    query: String,
    confirming: Signal<bool>,
    pending_directory: Signal<String>,
    pending_file_name: Signal<String>,
    pending_path: Signal<String>,
) -> Result<impl View> {
    if query.chars().count() > 200 {
        return Err(Error::msg("搜索内容不能超过 200 个字符"));
    }

    Ok(live! {
        let task_manager = manager(cx);
        let mut events = task_manager.subscribe();

        loop {
            let files = if query.is_empty() {
                Vec::new()
            } else {
                task_manager.search_media_files(&query).await
            };
            let token = emit! {
                if query.is_empty() {
                    <p class="text-sm text-muted-foreground">
                        "输入关键词并搜索以检查匹配的媒体文件"
                    </p>
                } else if files.is_empty() {
                    <p class="text-sm text-muted-foreground">
                        "没有找到匹配的媒体文件"
                    </p>
                } else {
                    <div class="grid gap-2">
                        <p class="text-xs text-muted-foreground">
                            "搜索结果最多显示 200 条；匹配项较多时，结果可能已截断。"
                        </p>
                        <div class="grid grid-cols-1 gap-2">
                            #[key(file.relative_path())]
                            for file in files {
                                let directory = file.directory.clone();
                                let file_name = file.file_name.clone();
                                let relative_path = file.relative_path();
                                let subtitle = if directory.is_empty() {
                                    format_size(file.size_bytes)
                                } else {
                                    format!("{} · {}", directory, format_size(file.size_bytes))
                                };

                                <article
                                    id=(stable_dom_id("media", &relative_path))
                                    class="flex items-center justify-between gap-4 rounded-lg border border-border bg-background p-3"
                                >
                                    <div class="min-w-0 flex-1">
                                        <p class="truncate text-sm font-medium">(&file_name)</p>
                                        <p class="truncate text-xs text-muted-foreground">
                                            (&subtitle)
                                        </p>
                                    </div>

                                    button(
                                        variant: ButtonVariant::Destructive,
                                        size: ButtonSize::Sm,
                                        attrs: attributes! {
                                            @click=$(|_event: Event| {
                                                pending_directory.set(directory.clone());
                                                pending_file_name.set(file_name.clone());
                                                pending_path.set(relative_path.clone());
                                                confirming.set(true);
                                            })
                                        },
                                        "删除"
                                    )
                                </article>
                            }
                        </div>
                    </div>
                }
            }?;

            if !connected(cx) {
                break Ok(token);
            }
            if !wait_for_media_change(&mut events).await {
                break Ok(token);
            }
        }
    })
}

async fn wait_for_media_change(events: &mut broadcast::Receiver<TaskEvent>) -> bool {
    loop {
        match events.recv().await {
            Ok(TaskEvent::MediaChanged) => return true,
            Ok(TaskEvent::Progress { .. } | TaskEvent::Changed { .. }) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => return true,
            Err(broadcast::error::RecvError::Closed) => return false,
        }
    }
}
