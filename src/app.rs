mod actions;
mod download_form;
mod format;
mod media_panel;
mod task_panel;

use std::sync::Arc;

use topcoat::{
    asset::{AssetBundle, RouterBuilderAssetExt},
    context::{Cx, app_context},
    router::{Router, RouterBuilderDiscoverExt, Slot, layout, module_router},
    runtime::{RouterBuilderRuntimeExt, signal},
    view::{View, view},
};

use tessora::services::TaskManager;

use download_form::download_form_view;
use media_panel::media_panel_view;
use task_panel::task_panel_view;

pub struct AppState {
    pub manager: Arc<TaskManager>,
}

pub(crate) fn manager(cx: &Cx) -> &Arc<TaskManager> {
    &app_context::<AppState>(cx).manager
}

pub fn router(state: AppState) -> Router {
    module_router!()
        .discover()
        .app_context(state)
        .assets(AssetBundle::load().expect("无法加载 Topcoat assets"))
        .runtime()
        .build()
}

#[layout]
async fn document(slot: Slot<'_>) -> topcoat::Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="zh-CN" class="dark">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"Tessora"</title>
                <link rel="stylesheet" href=(topcoat::tailwind::stylesheet!())>
                topcoat::runtime::script()
                topcoat::dev::script()
            </head>
            <body class="min-h-screen bg-background text-foreground">(slot)</body>
        </html>
    })
}

#[topcoat::router::page]
async fn home(cx: &Cx) -> topcoat::Result<impl View> {
    let notice = signal(cx, String::new);

    Ok(view! {
        <main class="mx-auto flex max-w-7xl flex-col gap-6 p-4 md:p-8">
            <header>
                <h1 class="text-3xl font-semibold tracking-tight">"Tessora"</h1>
                <p class="mt-2 text-sm text-muted-foreground">
                    "以本地媒体目录为事实标准的下载管理器"
                </p>
            </header>

            <p class="min-h-6 text-sm text-primary" aria-live="polite">
                $(notice.get())
            </p>

            download_form_view(notice: notice.clone())

            <section class="grid gap-6 lg:grid-cols-2">
                task_panel_view(notice: notice.clone())
                media_panel_view(notice: notice.clone())
            </section>
        </main>
    })
}
