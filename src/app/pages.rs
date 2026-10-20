use dioxus::prelude::*;

use crate::app::{
    api::{list_directories, list_media_files, list_tasks},
    components::{CreateTaskPanel, MediaLibrary, TaskList},
    events::use_task_events,
};

#[component]
pub fn Home() -> Element {
    let mut task_refresh = use_signal(|| 0_u64);
    let media_refresh = use_signal(|| 0_u64);
    let tasks = use_resource(move || {
        let _ = task_refresh();
        list_tasks()
    });
    let directories = use_resource(list_directories);
    let media_files = use_resource(move || {
        let _ = task_refresh();
        let _ = media_refresh();
        list_media_files(true)
    });
    let live_events = use_task_events(task_refresh);

    let directory_values = match directories.read().as_ref() {
        Some(Ok(values)) => values.clone(),
        _ => Vec::new(),
    };
    let live_connected = live_events.connected();

    // Keep the page width arithmetic out of a Tailwind arbitrary-value class. The production CSS
    // optimizer can remove calc() here and turn `min(..., calc(...))` into an invalid declaration.
    rsx! {
        div { class: "relative min-h-screen w-full overflow-hidden bg-[#090b10] font-sans text-indigo-50 antialiased",
            div { class: "pointer-events-none absolute -left-40 -top-52 size-[38rem] rounded-full bg-violet-700/20 blur-[110px]" }
            div { class: "pointer-events-none absolute -right-40 top-10 size-[30rem] rounded-full bg-emerald-600/10 blur-[110px]" }

            div {
                class: "relative mx-auto w-full",
                style: "max-width: 1212px; padding-inline: 1rem;",
                header { class: "flex items-center gap-4 py-9 md:py-11",
                    div { class: "grid size-14 shrink-0 place-items-center rounded-2xl border border-white/20 bg-gradient-to-br from-violet-500 to-indigo-700 font-serif text-3xl italic shadow-2xl shadow-indigo-900/40",
                        "T"
                    }
                    div { class: "min-w-0",
                        p { class: "mb-1 text-[10px] font-extrabold tracking-[0.16em] text-slate-500", "SELF-HOSTED MEDIA TOOL" }
                        h1 { class: "text-3xl font-bold tracking-tight", "Tessora" }
                        p { class: "mt-1 hidden text-sm text-slate-400 sm:block", "把 M3U8 资源安静地收进你的媒体库。" }
                    }
                    div { class: "ml-auto hidden items-center gap-2 rounded-full border border-slate-800 bg-slate-900/70 px-3 py-2 text-xs text-slate-400 sm:flex",
                        span {
                            class: if live_connected { "size-2 rounded-full bg-emerald-400 shadow-[0_0_12px_#4ade80]" } else { "size-2 animate-pulse rounded-full bg-amber-400" }
                        }
                        if live_connected { "实时连接" } else { "正在连接" }
                    }
                }

                main { class: "grid min-w-0 gap-6",
                    CreateTaskPanel {
                        directories: directory_values,
                        task_refresh,
                        events_connected: live_connected,
                    }

                    section { class: "min-w-0 rounded-[1.4rem] border border-slate-800/90 bg-slate-950/60 p-3 shadow-2xl shadow-black/10 sm:p-6",
                        div { class: "mb-5 flex items-center justify-between",
                            div {
                                p { class: "mb-1 text-[10px] font-extrabold tracking-[0.16em] text-slate-500", "DOWNLOAD QUEUE" }
                                h2 { class: "text-2xl font-bold tracking-tight", "下载任务" }
                            }
                            button {
                                class: "min-h-9 rounded-lg border border-slate-700 px-3 text-xs font-bold text-slate-400 transition hover:-translate-y-px hover:border-slate-600 hover:text-slate-200",
                                onclick: move |_| *task_refresh.write() += 1,
                                "刷新"
                            }
                        }

                        match tasks.read().as_ref() {
                            None => rsx! { div { class: "rounded-xl border border-dashed border-slate-700 px-5 py-11 text-center text-sm text-slate-500", "正在加载任务…" } },
                            Some(Err(error)) => rsx! {
                                div { class: "rounded-xl border border-dashed border-rose-900/70 bg-rose-950/20 px-5 py-11 text-center text-sm text-rose-400", "无法读取任务：{error}" }
                            },
                            Some(Ok(items)) => rsx! {
                                TaskList {
                                    tasks: items.iter().cloned().map(|mut task| {
                                        if let Some(progress) = live_events.progress(&task.id) {
                                            task.progress = Some(progress);
                                        }
                                        task
                                    }).collect::<Vec<_>>(),
                                    refresh: task_refresh,
                                    events_connected: live_connected,
                                }
                            },
                        }
                    }


                    section { class: "min-w-0 rounded-[1.4rem] border border-slate-800/90 bg-slate-950/60 p-3 shadow-2xl shadow-black/10 sm:p-6",
                        match media_files.read().as_ref() {
                            None => rsx! { div { class: "rounded-xl border border-dashed border-slate-700 px-5 py-11 text-center text-sm text-slate-500", "正在加载媒体库…" } },
                            Some(Err(error)) => rsx! {
                                div { class: "rounded-xl border border-dashed border-rose-900/70 bg-rose-950/20 px-5 py-11 text-center text-sm text-rose-400", "无法读取媒体库：{error}" }
                            },
                            Some(Ok(items)) => rsx! {
                                MediaLibrary { files: items.clone(), refresh: media_refresh }
                            },
                        }
                    }
                }

                footer { class: "px-3 py-8 text-center text-[11px] text-slate-600",
                    "Tessora 仅处理你有权访问和保存的非 DRM 媒体资源。"
                }
            }
        }
    }
}
