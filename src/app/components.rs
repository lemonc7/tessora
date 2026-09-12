use dioxus::prelude::*;

use crate::{
    app::api::{cancel_task, create_task, delete_task, retry_task},
    contracts::{
        api::{CreateTaskRequest, DirectoryEntry},
        tasks::{DownloadTask, TaskStatus},
    },
};

const FIELD: &str = "h-11 w-full rounded-xl border border-slate-700 bg-slate-950/80 px-3 text-sm text-slate-100 outline-none transition placeholder:text-slate-600 focus:border-violet-500 focus:ring-4 focus:ring-violet-500/10";
const LABEL: &str = "mb-2 block text-xs font-semibold text-slate-300";
const SMALL_BUTTON: &str = "min-h-9 rounded-lg border px-3 text-xs font-bold transition hover:-translate-y-px hover:brightness-110 disabled:cursor-wait disabled:opacity-50";

#[component]
pub fn CreateTaskPanel(
    directories: Vec<DirectoryEntry>,
    mut task_refresh: Signal<u64>,
    events_connected: bool,
) -> Element {
    let mut url = use_signal(String::new);
    let mut directory = use_signal(String::new);
    let mut file_name = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut message = use_signal(|| None::<(bool, String)>);

    rsx! {
        section { class: "grid overflow-hidden rounded-[1.4rem] border border-white/10 bg-slate-900/80 shadow-2xl shadow-black/20 md:grid-cols-[minmax(220px,.75fr)_minmax(480px,1.8fr)]",
            div { class: "bg-gradient-to-br from-violet-600 to-indigo-800 p-6 sm:p-8",
                p { class: "mb-1 text-[10px] font-extrabold tracking-[0.16em] text-white/60", "NEW DOWNLOAD" }
                h2 { class: "text-2xl font-bold tracking-tight", "添加下载" }
                p { class: "mt-3 text-sm leading-7 text-white/70", "粘贴一个 M3U8 地址，选择服务器目录，剩下的交给 FFmpeg。" }
            }

            form {
                class: "grid gap-4 p-5 sm:p-7",
                onsubmit: move |event| {
                    event.prevent_default();
                    if submitting() {
                        return;
                    }
                    submitting.set(true);
                    message.set(None);
                    let request = CreateTaskRequest {
                        url: url(),
                        directory: directory(),
                        file_name: file_name(),
                    };
                    spawn(async move {
                        match create_task(request).await {
                            Ok(_) => {
                                url.set(String::new());
                                file_name.set(String::new());
                                message.set(Some((true, "任务已加入下载队列".into())));
                                if !events_connected {
                                    *task_refresh.write() += 1;
                                }
                            }
                            Err(error) => message.set(Some((false, error.to_string()))),
                        }
                        submitting.set(false);
                    });
                },

                label {
                    span { class: LABEL, "M3U8 链接" }
                    input {
                        class: FIELD,
                        r#type: "url",
                        placeholder: "https://example.com/master.m3u8",
                        value: url,
                        required: true,
                        oninput: move |event| url.set(event.value()),
                    }
                }

                div { class: "grid gap-4 sm:grid-cols-2",
                    label {
                        span { class: LABEL, "下载目录" }
                        select {
                            class: FIELD,
                            value: directory,
                            onchange: move |event| directory.set(event.value()),
                            if directories.is_empty() {
                                option { value: "", "根目录" }
                            }
                            for entry in directories.iter() {
                                option { value: "{entry.value}", "{entry.label}" }
                            }
                        }
                    }
                    label {
                        span { class: LABEL, "文件名（可选）" }
                        div { class: "flex h-11 items-center rounded-xl border border-slate-700 bg-slate-950/80 transition focus-within:border-violet-500 focus-within:ring-4 focus-within:ring-violet-500/10",
                            input {
                                class: "h-full min-w-0 flex-1 border-0 bg-transparent px-3 text-sm text-slate-100 outline-none placeholder:text-slate-600",
                                r#type: "text",
                                placeholder: "例如：第一集（留空自动命名）",
                                value: file_name,
                                maxlength: 180,
                                oninput: move |event| file_name.set(event.value()),
                            }
                            b { class: "pr-3 text-xs text-slate-500", ".mp4" }
                        }
                    }
                }

                if let Some((success, text)) = message() {
                    div {
                        class: "rounded-lg px-3 py-2.5 text-xs",
                        class: if success { "bg-emerald-500/10 text-emerald-300" } else { "bg-rose-500/10 text-rose-300" },
                        "{text}"
                    }
                }

                button {
                    class: "min-h-11 rounded-xl bg-gradient-to-r from-violet-600 to-indigo-600 px-5 text-sm font-bold text-white shadow-lg shadow-indigo-950/30 transition hover:-translate-y-px hover:brightness-110 disabled:cursor-wait disabled:opacity-50",
                    r#type: "submit",
                    disabled: submitting,
                    if submitting() { "正在创建…" } else { "开始下载" }
                }
            }
        }
    }
}

#[component]
pub fn TaskList(tasks: Vec<DownloadTask>, refresh: Signal<u64>, events_connected: bool) -> Element {
    if tasks.is_empty() {
        return rsx! {
            div { class: "rounded-xl border border-dashed border-slate-700 px-5 py-11 text-center text-slate-500",
                div { class: "text-3xl", "↓" }
                h3 { class: "mt-2 font-bold text-slate-300", "还没有下载任务" }
                p { class: "mt-1 text-sm", "从上方添加一个 M3U8 链接开始。" }
            }
        };
    }

    rsx! {
        div { class: "grid gap-2.5",
            for task in tasks {
                TaskCard { key: "{task.id}", task, refresh, events_connected }
            }
        }
    }
}

#[component]
fn TaskCard(task: DownloadTask, mut refresh: Signal<u64>, events_connected: bool) -> Element {
    let mut action_error = use_signal(|| None::<String>);
    let location = if task.directory.is_empty() {
        task.file_name.clone()
    } else {
        format!("{}/{}", task.directory, task.file_name)
    };
    let directory_label = if task.directory.is_empty() {
        "根目录".to_owned()
    } else {
        task.directory.clone()
    };
    let task_id_for_cancel = task.id.clone();
    let task_id_for_retry = task.id.clone();
    let task_id_for_delete = task.id.clone();
    let task_id_for_delete_file = task.id.clone();

    rsx! {
        article { class: "flex min-h-24 flex-col items-start justify-between gap-4 rounded-2xl border border-slate-800 bg-slate-900/80 p-4 sm:flex-row sm:items-center",
            div { class: "flex min-w-0 flex-1 items-center gap-3.5",
                div { class: "grid size-12 shrink-0 place-items-center rounded-xl bg-violet-500/15 text-[10px] font-black text-violet-300", "MP4" }
                div { class: "min-w-0 flex-1",
                    div { class: "flex items-center gap-2.5",
                        h3 { class: "truncate text-sm font-bold text-slate-100", title: "{location}", "{task.file_name}" }
                        span { class: status_classes(task.status), "{task.status.label()}" }
                    }
                    p { class: "mt-1.5 max-w-2xl truncate text-xs text-slate-500", "{directory_label}" }
                    if task.status == TaskStatus::Downloading {
                        if let Some(progress) = &task.progress {
                            div { class: "mt-2 max-w-xl",
                                div { class: "mb-1.5 flex items-center justify-between text-[11px] text-slate-400",
                                    if let Some(total) = progress.total_duration_ms {
                                        span { "{format_duration(progress.processed_duration_ms)} / {format_duration(total)}" }
                                    }
                                    if let Some(percentage) = progress.percentage {
                                        span { class: "font-bold text-violet-300", "{percentage:.0}%" }
                                    }
                                }
                                div { class: "h-1.5 overflow-hidden rounded-full bg-slate-800",
                                    if let Some(percentage) = progress.percentage {
                                        div {
                                            class: "h-full rounded-full bg-gradient-to-r from-violet-500 to-indigo-400 transition-[width] duration-500",
                                            style: "width: {percentage}%",
                                        }
                                    } else {
                                        div { class: "h-full w-1/3 animate-pulse rounded-full bg-violet-500/70" }
                                    }
                                }
                            }
                        } else {
                            div { class: "mt-2 flex items-center gap-2 text-[11px] text-slate-400",
                                span { class: "size-1.5 animate-pulse rounded-full bg-violet-400" }
                                "正在准备下载…"
                            }
                        }
                    }
                    if let Some(error) = &task.error {
                        p { class: "mt-2 max-w-2xl truncate text-[11px] text-rose-400", title: "{error}", "{error}" }
                    }
                    if let Some(error) = action_error() {
                        p { class: "mt-2 max-w-2xl truncate text-[11px] text-rose-400", "{error}" }
                    }
                }
            }

            div { class: "flex w-full shrink-0 items-center justify-end gap-2 sm:w-auto",
                if task.status.is_active() {
                    button {
                        class: "min-h-9 rounded-lg border border-rose-500/30 bg-rose-500/10 px-3 text-xs font-bold text-rose-300 transition hover:-translate-y-px hover:brightness-110",
                        onclick: move |_| {
                            let id = task_id_for_cancel.clone();
                            spawn(async move {
                                match cancel_task(id).await {
                                    Ok(()) if !events_connected => *refresh.write() += 1,
                                    Ok(()) => {}
                                    Err(error) => action_error.set(Some(error.to_string())),
                                }
                            });
                        },
                        "取消"
                    }
                }
                if task.status.can_retry() {
                    button {
                        class: "{SMALL_BUTTON} border-slate-600 bg-slate-800 text-slate-200",
                        onclick: move |_| {
                            let id = task_id_for_retry.clone();
                            spawn(async move {
                                match retry_task(id).await {
                                    Ok(()) if !events_connected => *refresh.write() += 1,
                                    Ok(()) => {}
                                    Err(error) => action_error.set(Some(error.to_string())),
                                }
                            });
                        },
                        "重试"
                    }
                }
                if !task.status.is_active() {
                    button {
                        class: "{SMALL_BUTTON} border-slate-700 bg-transparent text-slate-400",
                        onclick: move |_| {
                            let id = task_id_for_delete.clone();
                            spawn(async move {
                                match delete_task(id, false).await {
                                    Ok(()) if !events_connected => *refresh.write() += 1,
                                    Ok(()) => {}
                                    Err(error) => action_error.set(Some(error.to_string())),
                                }
                            });
                        },
                        "删记录"
                    }
                }
                if task.status == TaskStatus::Completed {
                    button {
                        class: "{SMALL_BUTTON} border-rose-500/30 bg-transparent text-rose-300",
                        onclick: move |_| {
                            let id = task_id_for_delete_file.clone();
                            spawn(async move {
                                match delete_task(id, true).await {
                                    Ok(()) if !events_connected => *refresh.write() += 1,
                                    Ok(()) => {}
                                    Err(error) => action_error.set(Some(error.to_string())),
                                }
                            });
                        },
                        "删文件"
                    }
                }
            }
        }
    }
}

fn status_classes(status: TaskStatus) -> &'static str {
    let base =
        "shrink-0 rounded-md bg-slate-800 px-2 py-1 text-[10px] font-extrabold text-slate-400";
    match status {
        TaskStatus::Queued => {
            "shrink-0 rounded-md bg-amber-400/10 px-2 py-1 text-[10px] font-extrabold text-amber-300"
        }
        TaskStatus::Downloading => {
            "shrink-0 rounded-md bg-violet-500/15 px-2 py-1 text-[10px] font-extrabold text-violet-300"
        }
        TaskStatus::Completed => {
            "shrink-0 rounded-md bg-emerald-500/10 px-2 py-1 text-[10px] font-extrabold text-emerald-300"
        }
        TaskStatus::Failed => {
            "shrink-0 rounded-md bg-rose-500/10 px-2 py-1 text-[10px] font-extrabold text-rose-300"
        }
        TaskStatus::Cancelled | TaskStatus::Interrupted => base,
    }
}

fn format_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}
