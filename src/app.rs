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

use download_form::download_form_view;
use media_panel::media_panel_view;
use task_panel::task_panel_view;

use crate::services::TaskManager;

pub struct AppState {
    pub manager: Arc<TaskManager>,
}

pub(crate) fn manager(cx: &Cx) -> &Arc<TaskManager> {
    &app_context::<AppState>(cx).manager
}

// 路由
pub fn router(state: AppState) -> Router {
    module_router!() // 创建当前模块的router
        .discover() // 发现通过宏注册的内容，例如#[page] #[layout] #[procedure] #[shard]
        .app_context(state) // 将全局状态放入请求上下文，类似axum Extension
        .assets(AssetBundle::load().expect("无法加载 Topcoat assets")) // 加载构建后的CSS，图标等静态资源
        .runtime() // 注册topcoat浏览器runtime所需的路由和脚本支持
        .build()
}

// HTML文档外壳，slot是具体页面内容
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
                topcoat::runtime::script()  // 插入浏览器runtime，负责连接服务器，监听DOM时间，调用Procedure，接受服务器局部更新等等
                topcoat::dev::script()  // 开发者模式相关支持
            </head>
            <body class="min-h-screen bg-background text-foreground">(slot)</body>
        </html>
    })
}

// 主页定义
#[topcoat::router::page]
async fn home(cx: &Cx) -> topcoat::Result<impl View> {
    let notice = signal(cx, String::new);

    Ok(view! {
        <main class="mx-auto flex max-w-7xl flex-col gap-4 p-4 md:p-6">
            // 标题栏
            <header>
                <h1 class="text-3xl font-semibold tracking-tight">"Tessora"</h1>
                <p class="mt-1 text-sm text-muted-foreground">
                    "以本地媒体目录为事实标准的下载管理器"
                </p>
            </header>

            // 消息通知栏
            <p class="text-sm text-primary empty:hidden" aria-live="polite">
                $(notice.get())
            </p>

            // 下载表格栏
            download_form_view(notice: notice.clone())

            // 下载任务与媒体文件栏
            <section class="grid gap-6 lg:grid-cols-2">
                task_panel_view(notice: notice.clone())
                media_panel_view(notice: notice.clone())
            </section>
        </main>
    })
}
