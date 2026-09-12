use std::path::{Path, PathBuf};

pub fn temporary_output(final_output: &Path, task_id: &str) -> PathBuf {
    let file_name = final_output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    final_output.with_file_name(format!(".{file_name}.{task_id}.part.mp4"))
}

pub fn cleanup_stale_temporary_files(root: &Path) -> std::io::Result<usize> {
    cleanup_directory(root, 0)
}

fn cleanup_directory(directory: &Path, depth: usize) -> std::io::Result<usize> {
    if depth >= 32 {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            removed += cleanup_directory(&entry.path(), depth + 1)?;
        } else if file_type.is_file() && is_tessora_temporary_file(&entry.path()) {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn is_tessora_temporary_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(prefix) = file_name.strip_suffix(".part.mp4") else {
        return false;
    };
    let Some((_, task_id)) = prefix.rsplit_once('.') else {
        return false;
    };
    file_name.starts_with('.') && uuid::Uuid::parse_str(task_id).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_owned_temporary_files() {
        assert!(is_tessora_temporary_file(Path::new(
            ".episode.550e8400-e29b-41d4-a716-446655440000.part.mp4"
        )));
        assert!(!is_tessora_temporary_file(Path::new("episode.part.mp4")));
        assert!(!is_tessora_temporary_file(Path::new(
            ".episode.not-a-uuid.part.mp4"
        )));
        assert!(!is_tessora_temporary_file(Path::new(
            ".episode.550e8400-e29b-41d4-a716-446655440000.mp4"
        )));
    }

    #[test]
    fn removes_owned_temporary_files_only() {
        let root =
            std::env::temp_dir().join(format!("tessora-cleanup-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let owned = root.join(".episode.550e8400-e29b-41d4-a716-446655440000.part.mp4");
        let unrelated = root.join("keep.part.mp4");
        std::fs::write(&owned, b"partial").unwrap();
        std::fs::write(&unrelated, b"keep").unwrap();

        assert_eq!(cleanup_stale_temporary_files(&root).unwrap(), 1);
        assert!(!owned.exists());
        assert!(unrelated.exists());

        std::fs::remove_file(unrelated).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
