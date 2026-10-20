use std::fmt::Write;

use tessora::domain::TaskStatus;

pub(crate) fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "排队中",
        TaskStatus::Downloading => "下载中",
        TaskStatus::Completed => "已完成",
        TaskStatus::Failed => "失败",
        TaskStatus::Cancelled => "已取消",
    }
}

pub(crate) fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.2} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

pub(crate) fn stable_dom_id(prefix: &str, value: &str) -> String {
    let mut result = String::with_capacity(prefix.len() + value.len() * 2 + 1);
    result.push_str(prefix);
    result.push('-');

    for byte in value.as_bytes() {
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }

    result
}
