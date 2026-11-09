CREATE TABLE IF NOT EXISTS sessions (
 id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT 'New conversation', created_at REAL NOT NULL,
 updated_at REAL NOT NULL, compressed_summary TEXT DEFAULT '', summary_up_to_msg_id INTEGER DEFAULT 0,
 preferences_json TEXT DEFAULT '{}', deleted_at REAL
);
CREATE TABLE IF NOT EXISTS messages (
 id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 role TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', capability TEXT DEFAULT '', events_json TEXT DEFAULT '',
 attachments_json TEXT DEFAULT '', metadata_json TEXT DEFAULT '{}', created_at REAL NOT NULL, parent_message_id INTEGER
);
CREATE TABLE IF NOT EXISTS turns (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 capability TEXT DEFAULT '', status TEXT NOT NULL DEFAULT 'running', error TEXT DEFAULT '',
 created_at REAL NOT NULL, updated_at REAL NOT NULL, finished_at REAL, owner_id TEXT DEFAULT '',
 fencing_token INTEGER NOT NULL DEFAULT 0, state_version INTEGER NOT NULL DEFAULT 1,
 failure_code TEXT DEFAULT '', retryable INTEGER NOT NULL DEFAULT 0, assistant_message_id INTEGER
);
CREATE TABLE IF NOT EXISTS turn_submissions (
 client_submission_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL DEFAULT '', request_digest TEXT NOT NULL,
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL, superseded_by TEXT
);
CREATE TABLE IF NOT EXISTS turn_events (
 id INTEGER PRIMARY KEY AUTOINCREMENT, turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
 seq INTEGER NOT NULL, type TEXT NOT NULL, source TEXT DEFAULT '', stage TEXT DEFAULT '', content TEXT DEFAULT '',
 metadata_json TEXT DEFAULT '', timestamp REAL NOT NULL, created_at REAL NOT NULL, UNIQUE(turn_id,seq)
);
CREATE TABLE IF NOT EXISTS native_command_receipts (
 command_id TEXT PRIMARY KEY, turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
 command_type TEXT NOT NULL, response_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session_created ON messages(session_id,created_at,id);
CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_turns_session_updated ON turns(session_id,updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_turns_session_status ON turns(session_id,status,updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_turn_events_turn_seq ON turn_events(turn_id,seq);
CREATE TABLE IF NOT EXISTS native_tool_invocations (
 invocation_id TEXT PRIMARY KEY, turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
 call_id TEXT NOT NULL, name TEXT NOT NULL, arguments_json TEXT NOT NULL,
 status TEXT NOT NULL, result_json TEXT, created_at REAL NOT NULL, finished_at REAL,
 UNIQUE(turn_id,call_id)
);
CREATE INDEX IF NOT EXISTS idx_native_tool_turn_status ON native_tool_invocations(turn_id,status);
CREATE TABLE IF NOT EXISTS native_video_artifacts (
 id TEXT PRIMARY KEY REFERENCES native_tool_invocations(invocation_id) ON DELETE CASCADE,
 workspace_id TEXT NOT NULL, mime TEXT NOT NULL, content BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS native_turn_checkpoints (
 turn_id TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE,
 phase TEXT NOT NULL, payload_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS native_reply_commands (
 command_id TEXT PRIMARY KEY REFERENCES native_command_receipts(command_id) ON DELETE CASCADE,
 request_digest TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS native_notebooks (
 workspace_id TEXT NOT NULL, id TEXT NOT NULL, payload_json TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1, deleted INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(workspace_id,id)
);
CREATE TABLE IF NOT EXISTS native_memory_docs (
 workspace_id TEXT NOT NULL, layer TEXT NOT NULL, key TEXT NOT NULL,
 content TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, updated_at REAL NOT NULL,
 PRIMARY KEY(workspace_id,layer,key)
);
CREATE TABLE IF NOT EXISTS native_memory_versions (
 id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id TEXT NOT NULL,
 layer TEXT NOT NULL, key TEXT NOT NULL, content TEXT NOT NULL, created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS native_learning_imports(workspace_id TEXT PRIMARY KEY);
CREATE INDEX IF NOT EXISTS native_memory_versions_scope ON native_memory_versions(workspace_id,layer,key,id);
CREATE TABLE IF NOT EXISTS native_knowledge_bases (
 workspace_id TEXT NOT NULL, name TEXT NOT NULL, created_at REAL NOT NULL,
 PRIMARY KEY(workspace_id,name)
);
CREATE TABLE IF NOT EXISTS native_knowledge_files (
 workspace_id TEXT NOT NULL, kb TEXT NOT NULL, path TEXT NOT NULL,
 content TEXT NOT NULL, sha256 TEXT NOT NULL, updated_at REAL NOT NULL,
 PRIMARY KEY(workspace_id,kb,path)
);
CREATE TABLE IF NOT EXISTS native_knowledge_chunks (
 id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL,kb TEXT NOT NULL,path TEXT NOT NULL,
 start_line INTEGER NOT NULL,end_line INTEGER NOT NULL,start_byte INTEGER NOT NULL,end_byte INTEGER NOT NULL,content TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS native_knowledge_chunks_scope ON native_knowledge_chunks(workspace_id,kb,path);
CREATE VIRTUAL TABLE IF NOT EXISTS native_knowledge_fts USING fts5(id UNINDEXED,terms,tokenize='unicode61');
CREATE TABLE IF NOT EXISTS native_knowledge_sources (
 workspace_id TEXT NOT NULL,kb TEXT NOT NULL,path TEXT NOT NULL,sha256 TEXT NOT NULL,
 content TEXT NOT NULL,created_at REAL NOT NULL,
 PRIMARY KEY(workspace_id,kb,path,sha256)
);
CREATE TABLE IF NOT EXISTS native_knowledge_originals (
 workspace_id TEXT NOT NULL,kb TEXT NOT NULL,path TEXT NOT NULL,sha256 TEXT NOT NULL,original BLOB NOT NULL,
 PRIMARY KEY(workspace_id,kb,path,sha256)
);
CREATE TABLE IF NOT EXISTS native_study_artifacts (
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,payload_json TEXT NOT NULL,PRIMARY KEY(workspace_id,id)
);
CREATE TABLE IF NOT EXISTS native_study_attempts (
 workspace_id TEXT NOT NULL,command_id TEXT NOT NULL,payload_json TEXT NOT NULL,PRIMARY KEY(workspace_id,command_id)
);
CREATE TABLE IF NOT EXISTS native_knowledge_clips (
 workspace_id TEXT NOT NULL,kb TEXT NOT NULL,path TEXT NOT NULL,sha256 TEXT NOT NULL,
 start_line INTEGER NOT NULL,end_line INTEGER NOT NULL,document_id TEXT NOT NULL,
 PRIMARY KEY(workspace_id,kb,path,sha256,start_line,end_line)
);
CREATE TABLE IF NOT EXISTS native_documents (
 workspace_id TEXT NOT NULL, id TEXT NOT NULL, payload_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id,id)
);
CREATE TABLE IF NOT EXISTS native_document_versions (
 workspace_id TEXT NOT NULL, document_id TEXT NOT NULL, revision INTEGER NOT NULL,
 payload_json TEXT NOT NULL, PRIMARY KEY(workspace_id,document_id,revision),
 FOREIGN KEY(workspace_id,document_id) REFERENCES native_documents(workspace_id,id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS native_document_imports(workspace_id TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS native_browser_config(workspace_id TEXT PRIMARY KEY,payload_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS native_browser_cases(workspace_id TEXT NOT NULL,id TEXT NOT NULL,payload_json TEXT NOT NULL,PRIMARY KEY(workspace_id,id));
CREATE TABLE IF NOT EXISTS native_browser_runs(
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,case_id TEXT,request_digest TEXT NOT NULL DEFAULT '',
 status TEXT NOT NULL,payload_json TEXT NOT NULL,created_at REAL NOT NULL,PRIMARY KEY(workspace_id,id)
);
CREATE INDEX IF NOT EXISTS native_browser_runs_scope ON native_browser_runs(workspace_id,created_at DESC);
CREATE TABLE IF NOT EXISTS native_browser_artifacts(
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,run_id TEXT NOT NULL,name TEXT NOT NULL,mime TEXT NOT NULL,data BLOB NOT NULL,
 PRIMARY KEY(workspace_id,id),FOREIGN KEY(workspace_id,run_id) REFERENCES native_browser_runs(workspace_id,id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS native_browser_sources(
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,name TEXT NOT NULL,content TEXT NOT NULL,original_content TEXT NOT NULL,
 original BLOB NOT NULL,current BLOB NOT NULL,revision INTEGER NOT NULL DEFAULT 0,created_at REAL NOT NULL,
 PRIMARY KEY(workspace_id,id)
);
CREATE TABLE IF NOT EXISTS native_browser_exports(
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,source_id TEXT NOT NULL,name TEXT NOT NULL,
 data BLOB NOT NULL,payload_json TEXT NOT NULL,created_at REAL NOT NULL,
 PRIMARY KEY(workspace_id,id),
 FOREIGN KEY(workspace_id,source_id) REFERENCES native_browser_sources(workspace_id,id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS native_memory_maintenance(workspace_id TEXT PRIMARY KEY,payload_json TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS native_document_import_receipts(workspace_id TEXT NOT NULL,command_id TEXT NOT NULL,request_digest TEXT NOT NULL,document_id TEXT NOT NULL,PRIMARY KEY(workspace_id,command_id));

-- Reading owns its snapshots: removing/replacing an indexed file cannot move a quote.
CREATE TABLE IF NOT EXISTS native_reading_sources(
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,content TEXT NOT NULL,original BLOB NOT NULL,
 PRIMARY KEY(workspace_id,id)
);
CREATE TABLE IF NOT EXISTS native_reading_items(
 workspace_id TEXT NOT NULL,id TEXT NOT NULL,source_id TEXT NOT NULL,kb TEXT NOT NULL,path TEXT NOT NULL,
 original_hash TEXT NOT NULL,payload_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id,id),UNIQUE(workspace_id,kb,path,original_hash),
 FOREIGN KEY(workspace_id,source_id) REFERENCES native_reading_sources(workspace_id,id)
);
CREATE TABLE IF NOT EXISTS native_reading_progress(
 workspace_id TEXT NOT NULL,item_id TEXT NOT NULL,payload_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id,item_id),
 FOREIGN KEY(workspace_id,item_id) REFERENCES native_reading_items(workspace_id,id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS native_reading_annotations(
 workspace_id TEXT NOT NULL,item_id TEXT NOT NULL,id TEXT NOT NULL,payload_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id,item_id,id),
 FOREIGN KEY(workspace_id,item_id) REFERENCES native_reading_items(workspace_id,id) ON DELETE CASCADE
);
-- Creation receipts survive annotation deletion, so an ambiguous retry never resurrects it.
CREATE TABLE IF NOT EXISTS native_reading_receipts(
 workspace_id TEXT NOT NULL,command_id TEXT NOT NULL,item_id TEXT NOT NULL,
 request_digest TEXT NOT NULL,annotation_id TEXT NOT NULL,
 PRIMARY KEY(workspace_id,command_id),
 FOREIGN KEY(workspace_id,item_id) REFERENCES native_reading_items(workspace_id,id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS native_reading_clips(
 workspace_id TEXT NOT NULL,item_id TEXT NOT NULL,annotation_id TEXT NOT NULL,document_id TEXT NOT NULL,
 PRIMARY KEY(workspace_id,item_id,annotation_id),
 FOREIGN KEY(workspace_id,item_id,annotation_id) REFERENCES native_reading_annotations(workspace_id,item_id,id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS native_calendar_state(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL);
INSERT OR IGNORE INTO native_calendar_state VALUES(1,0);
CREATE TABLE IF NOT EXISTS native_calendar_objects(kind TEXT NOT NULL,id TEXT NOT NULL,payload_json TEXT NOT NULL,PRIMARY KEY(kind,id));
CREATE TABLE IF NOT EXISTS native_calendar_receipts(command_id TEXT PRIMARY KEY,request_digest TEXT NOT NULL,payload_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS native_calendar_undo(id TEXT PRIMARY KEY,payload_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS native_flashcard_decks (
 workspace_id TEXT NOT NULL,page_id TEXT NOT NULL,payload_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id,page_id),
 FOREIGN KEY(workspace_id,page_id) REFERENCES native_documents(workspace_id,id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS native_flashcard_receipts (
 workspace_id TEXT NOT NULL,command_id TEXT NOT NULL,digest TEXT NOT NULL,payload_json TEXT NOT NULL,
 PRIMARY KEY(workspace_id,command_id)
);

CREATE TABLE IF NOT EXISTS native_reading_web_receipts(
 workspace_id TEXT NOT NULL,command_id TEXT NOT NULL,request_url TEXT NOT NULL,item_id TEXT NOT NULL,
 PRIMARY KEY(workspace_id,command_id)
);
-- Unlike implicit rowids, explicit INTEGER PRIMARY KEY values survive VACUUM.
-- Freeze the legacy insertion order without rewriting any timestamp payloads.
CREATE TABLE IF NOT EXISTS native_study_evidence_order (
 position INTEGER PRIMARY KEY AUTOINCREMENT,
 workspace_id TEXT NOT NULL,command_id TEXT NOT NULL,
 UNIQUE(workspace_id,command_id),
 FOREIGN KEY(workspace_id,command_id) REFERENCES native_study_attempts(workspace_id,command_id) ON DELETE CASCADE
);
INSERT OR IGNORE INTO native_study_evidence_order(workspace_id,command_id)
 SELECT a.workspace_id,a.command_id FROM native_study_attempts a
 WHERE NOT EXISTS (SELECT 1 FROM native_study_evidence_order o
  WHERE o.workspace_id=a.workspace_id AND o.command_id=a.command_id)
 ORDER BY a.rowid;
CREATE TRIGGER IF NOT EXISTS native_study_evidence_order_insert
 AFTER INSERT ON native_study_attempts BEGIN
 INSERT INTO native_study_evidence_order(workspace_id,command_id) VALUES(NEW.workspace_id,NEW.command_id);
END;
