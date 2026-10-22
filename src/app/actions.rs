use topcoat::{Result, context::Cx, runtime::procedure};

use crate::domain::CreateTask;

use super::manager;

type ActionResult = (bool, String);

// procedure 是前端可以调用的服务器函数
#[procedure("/api/tasks/create")]
pub(crate) async fn create_download(
    cx: &Cx,
    url: String,
    directory: String,
    file_name: String,
) -> Result<ActionResult> {
    let result = manager(cx)
        .create_task(CreateTask {
            url,
            directory,
            file_name,
        })
        .await;

    Ok(match result {
        Ok(task) => (true, format!("任务已创建：{}", task.file_name)),
        Err(error) => (false, error.to_string()),
    })
}

#[procedure("/api/tasks/cancel")]
pub(crate) async fn cancel_download(cx: &Cx, id: String) -> Result<ActionResult> {
    Ok(match manager(cx).cancel_task(&id).await {
        Ok(()) => (true, "已请求取消任务".to_owned()),
        Err(error) => (false, error.to_string()),
    })
}

#[procedure("/api/tasks/retry")]
pub(crate) async fn retry_download(cx: &Cx, id: String) -> Result<ActionResult> {
    Ok(match manager(cx).retry_task(&id).await {
        Ok(()) => (true, "任务已重新加入队列".to_owned()),
        Err(error) => (false, error.to_string()),
    })
}

#[procedure("/api/tasks/delete")]
pub(crate) async fn delete_task(cx: &Cx, id: String) -> Result<ActionResult> {
    Ok(match manager(cx).delete_task(&id).await {
        Ok(()) => (true, "任务记录已删除".to_owned()),
        Err(error) => (false, error.to_string()),
    })
}

#[procedure("/api/media/scan")]
pub(crate) async fn scan_media(cx: &Cx) -> Result<ActionResult> {
    Ok(match manager(cx).scan_media_files().await {
        Ok(report) => (
            true,
            format!(
                "扫描完成：发现 {} 个文件，跳过 {} 项",
                report.files, report.skipped
            ),
        ),
        Err(error) => (false, error.to_string()),
    })
}

#[procedure("/api/media/delete")]
pub(crate) async fn delete_media(
    cx: &Cx,
    directory: String,
    file_name: String,
) -> Result<ActionResult> {
    Ok(
        match manager(cx).delete_media_file(&directory, &file_name).await {
            Ok(()) => (true, "媒体文件已删除".to_owned()),
            Err(error) => (false, error.to_string()),
        },
    )
}
