CREATE TABLE download_tasks (
    id                    TEXT PRIMARY KEY NOT NULL,
    url                   TEXT NOT NULL,
    directory             TEXT NOT NULL,
    file_name             TEXT NOT NULL,
    status                TEXT NOT NULL,
    error_message         TEXT,
    created_at            INTEGER NOT NULL,
    started_at            INTEGER,
    finished_at           INTEGER,
    UNIQUE(directory, file_name)
);

CREATE INDEX idx_download_tasks_created_at
    ON download_tasks(created_at DESC);
