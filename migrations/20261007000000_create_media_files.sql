CREATE TABLE media_files (
    id                  TEXT PRIMARY KEY NOT NULL,
    directory           TEXT NOT NULL,
    file_name           TEXT NOT NULL,
    size_bytes          INTEGER NOT NULL,
    modified_at         INTEGER NOT NULL,
    origin              TEXT NOT NULL CHECK(origin IN ('tessora', 'external')),
    source_task_id      TEXT,
    content_hash        TEXT,
    state               TEXT NOT NULL DEFAULT 'present'
                        CHECK(state IN ('present', 'missing')),
    discovered_at       INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    last_seen_scan_id   TEXT,
    UNIQUE(directory, file_name),
    FOREIGN KEY(source_task_id) REFERENCES download_tasks(id) ON DELETE SET NULL
);

CREATE INDEX idx_media_files_state ON media_files(state);
CREATE INDEX idx_media_files_content_hash ON media_files(content_hash);
CREATE INDEX idx_media_files_source_task ON media_files(source_task_id);
