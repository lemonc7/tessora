#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    pub directory: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_at: i64,
}

impl MediaFile {
    pub fn relative_path(&self) -> String {
        if self.directory.is_empty() {
            self.file_name.clone()
        } else {
            format!("{}/{}", self.directory, self.file_name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanReport {
    pub files: usize,
    pub skipped: usize,
}
