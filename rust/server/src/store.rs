//! The native application owns schema initialization, session/message writes and
//! turn admission. No Python process participates in these transactions.
use crate::{Error, Result, capability::Capability, config::Settings};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn id(prefix: &str) -> String {
    format!(
        "{prefix}_{}_{}",
        (now() * 1000.0) as u64,
        uuid::Uuid::new_v4().simple()
    )
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}
fn value(row: &Row<'_>, name: &str, fallback: Value) -> rusqlite::Result<Value> {
    Ok(row
        .get::<_, Option<String>>(name)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(fallback))
}
fn session_row(r: &Row<'_>) -> rusqlite::Result<Value> {
    Ok(
        json!({"id":r.get::<_,String>("id")?,"session_id":r.get::<_,String>("id")?,
      "title":r.get::<_,String>("title")?,"created_at":r.get::<_,f64>("created_at")?,
      "updated_at":r.get::<_,f64>("updated_at")?,"preferences":value(r,"preferences_json",json!({}))?,
      "compressed_summary":r.get::<_,Option<String>>("compressed_summary")?.unwrap_or_default(),
      "summary_up_to_msg_id":r.get::<_,Option<i64>>("summary_up_to_msg_id")?.unwrap_or(0)}),
    )
}
fn message_row(r: &Row<'_>) -> rusqlite::Result<Value> {
    let mut metadata = value(r, "metadata_json", json!({}))?;
    // Provider response handles and internal prompts never cross the HTTP boundary.
    if let Some(map) = metadata.as_object_mut() {
        map.retain(|k, _| {
            !k.starts_with('_')
                && !matches!(
                    k.as_str(),
                    "provider_response_state" | "model_turn" | "native_history" | "request_payload"
                )
        });
    }
    Ok(
        json!({"id":r.get::<_,i64>("id")?,"session_id":r.get::<_,String>("session_id")?,
      "role":r.get::<_,String>("role")?,"content":r.get::<_,String>("content")?,
      "capability":r.get::<_,Option<String>>("capability")?.unwrap_or_default(),
      "created_at":r.get::<_,f64>("created_at")?,"parent_message_id":r.get::<_,Option<i64>>("parent_message_id")?,
      "events":value(r,"events_json",json!([]))?,"attachments":value(r,"attachments_json",json!([]))?,"metadata":metadata}),
    )
}

type SubmissionRow = (String, String, String, Option<String>, Option<String>);
#[derive(Clone)]
pub struct Store {
    pub path: PathBuf,
    event_writer: Arc<Mutex<Connection>>,
}
pub struct Event {
    pub kind: String,
    pub content: String,
    pub metadata: Value,
}
#[derive(Default)]
pub struct EventWriteTiming {
    pub writer_mutex_wait_ms: f64,
    pub sqlite_begin_ms: f64,
    pub write_commit_ms: f64,
    pub committed_at: Option<Instant>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Admission {
    pub session: String,
    pub turn: String,
    pub user_message: i64,
    pub replay: bool,
    #[serde(default)]
    pub capability: Capability,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = Self {
            path: path.to_path_buf(),
            event_writer: Arc::new(Mutex::new(Self::connect_path(path)?)),
        };
        let mut conn = store.connect()?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > crate::backup::SCHEMA_VERSION {
            return Err(Error::conflict(
                "Database schema is newer than this binary. Restore a compatible snapshot before downgrading.",
            ));
        }
        let populated = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='sessions')",
            [],
            |r| r.get::<_, bool>(0),
        )?;
        if version < crate::backup::SCHEMA_VERSION && populated {
            let directory = path
                .parent()
                .ok_or_else(|| Error::invalid("Invalid database path."))?
                .join("backups");
            crate::backup::snapshot(
                &conn,
                &directory,
                &format!("before-native-v{}", crate::backup::SCHEMA_VERSION),
            )?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("schema.sql"))?;
        for (table, column, definition) in [
            ("sessions", "preferences_json", "TEXT DEFAULT '{}'"),
            ("sessions", "deleted_at", "REAL"),
            ("messages", "metadata_json", "TEXT DEFAULT '{}'"),
            ("messages", "parent_message_id", "INTEGER"),
            ("turn_submissions", "superseded_by", "TEXT"),
            ("turns", "owner_id", "TEXT DEFAULT ''"),
            ("turns", "fencing_token", "INTEGER NOT NULL DEFAULT 0"),
            ("turns", "state_version", "INTEGER NOT NULL DEFAULT 1"),
            ("turns", "failure_code", "TEXT DEFAULT ''"),
            ("turns", "retryable", "INTEGER NOT NULL DEFAULT 0"),
            ("turns", "assistant_message_id", "INTEGER"),
        ] {
            let columns = tx
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !columns.iter().any(|c| c == column) {
                tx.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))?;
                if table == "messages" && column == "parent_message_id" {
                    tx.execute_batch("UPDATE messages SET parent_message_id=(SELECT MAX(p.id) FROM messages p WHERE p.session_id=messages.session_id AND p.id<messages.id)")?;
                }
            }
        }
        tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_messages_parent ON messages(session_id,parent_message_id)")?;
        // Create these after legacy columns (especially assistant_message_id)
        // have been added. The indexes also make correlated lookups bounded to
        // the selected session instead of rescanning the whole turn table.
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id,id);
             CREATE INDEX IF NOT EXISTS idx_turns_session_created_id ON turns(session_id,created_at DESC,id DESC);
             CREATE INDEX IF NOT EXISTS idx_turns_session_assistant_id ON turns(session_id,assistant_message_id,id) WHERE assistant_message_id IS NOT NULL;",
        )?;
        tx.pragma_update(None, "user_version", crate::backup::SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(store)
    }
    pub fn connect(&self) -> Result<Connection> {
        Self::connect_path(&self.path)
    }
    fn connect_path(path: &Path) -> Result<Connection> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(30))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }
    pub fn operation(&self, operation: &str, args: Value) -> Result<Value> {
        crate::turn_queries::read(&self.connect()?, operation, args)
    }
    pub fn session(&self, session: &str, workspace: &str) -> Result<Value> {
        self.connect()?.query_row("SELECT * FROM sessions WHERE id=? AND deleted_at IS NULL AND COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?",params![session,workspace],session_row).optional()?.ok_or_else(Error::missing)
    }
    pub fn detail(&self, session: &str, workspace: &str) -> Result<Value> {
        let mut item = self
            .session(session, workspace)
            .map_err(|_| Error::missing())?;
        item["messages"] = json!(self.messages(session)?);
        item["active_turns"] =
            self.operation("list_active_turns", json!({"session_id":session}))?;
        Ok(item)
    }
    pub fn messages(&self, session: &str) -> Result<Vec<Value>> {
        let conn = self.connect()?;
        // One statement gives messages and traces a consistent read snapshot.
        // Historical duplicate links select one stable turn, never duplicate a
        // message. A malformed link from another session cannot supply a trace.
        Ok(conn
            .prepare(
                "SELECT m.*, t.id AS trace_turn_id, COUNT(e.seq) AS trace_total,
                        COALESCE(MAX(e.seq),0) AS trace_last_seq,
                        MIN(e.timestamp) AS trace_started_at, MAX(e.timestamp) AS trace_ended_at
                 FROM messages m
                 LEFT JOIN turns t ON t.id=(
                     SELECT id FROM turns
                     WHERE session_id=?1 AND assistant_message_id=m.id ORDER BY id LIMIT 1
                 )
                 LEFT JOIN turn_events e ON e.turn_id=t.id
                 WHERE m.session_id=?1 GROUP BY m.id ORDER BY m.id",
            )?
            .query_map([session], |r| {
                let mut message = message_row(r)?;
                if let Some(turn) = r.get::<_, Option<String>>("trace_turn_id")? {
                    message["trace"] = json!({
                        "turn_id": turn, "total": r.get::<_,i64>("trace_total")?,
                        "last_seq": r.get::<_,i64>("trace_last_seq")?,
                        "started_at": r.get::<_,Option<f64>>("trace_started_at")?,
                        "ended_at": r.get::<_,Option<f64>>("trace_ended_at")?, "truncated":true,
                    });
                }
                Ok(message)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn message_trace(
        &self,
        session: &str,
        workspace: &str,
        message: i64,
        after: i64,
        limit: u32,
    ) -> Result<Value> {
        self.session(session, workspace)?;
        let conn = self.connect()?;
        let turn = conn.query_row(
            "SELECT t.id FROM messages m LEFT JOIN turns t ON t.assistant_message_id=m.id WHERE m.session_id=? AND m.id=?",
            params![session,message], |r| r.get::<_,Option<String>>(0),
        ).optional()?.ok_or_else(Error::missing)?;
        let mut events = json!([]);
        let (total, last) = if let Some(turn) = &turn {
            events = self.operation(
                "get_events",
                json!({"turn_id":turn,"after_seq":after,"limit":limit.clamp(1,1000)}),
            )?;
            conn.query_row(
                "SELECT COUNT(*),COALESCE(MAX(seq),0) FROM turn_events WHERE turn_id=?",
                [turn],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?
        } else {
            (0, 0)
        };
        let loaded = events
            .as_array()
            .and_then(|v| v.last())
            .and_then(|v| v["seq"].as_i64())
            .unwrap_or(after.max(0));
        Ok(
            json!({"session_id":session,"message_id":message,"turn_id":turn,"events":events,
            "total":total,"last_seq":last,"next_seq":if loaded<last{Some(loaded)}else{None},"complete":loaded>=last}),
        )
    }
    pub fn history(&self, session: &str, leaf: i64) -> Result<Vec<Value>> {
        Ok(self
            .branch(session, leaf)?
            .into_iter()
            .map(|(_, msg)| msg)
            .collect())
    }
    pub(crate) fn branch(&self, session: &str, leaf: i64) -> Result<Vec<(i64, Value)>> {
        // History does not need UI traces, attachment metadata, or per-message
        // event queries. Index parent links once instead of scanning at every hop.
        let conn = self.connect()?;
        let rows = conn
            .prepare("SELECT id,role,content,parent_message_id FROM messages WHERE session_id=?")?
            .query_map([session], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    (
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                    ),
                ))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        let mut chain = Vec::new();
        let mut current = Some(leaf);
        let mut visited = HashSet::new();
        while let Some(mid) = current {
            if !visited.insert(mid) {
                return Err(Error::conflict("Conversation has a cyclic message branch."));
            }
            let (role, content, parent) = rows
                .get(&mid)
                .ok_or_else(|| Error::conflict("Conversation branch is incomplete."))?;
            chain.push((mid, json!({"role":role,"content":content})));
            current = *parent;
        }
        chain.reverse();
        // Read native learning records once, then project only records attached
        // to assistant messages in the selected ancestry into model history.
        let records=conn.prepare("SELECT t.assistant_message_id,i.name,i.arguments_json,i.result_json FROM turns t JOIN native_tool_invocations i ON i.turn_id=t.id WHERE t.session_id=? AND t.assistant_message_id IS NOT NULL AND t.owner_id LIKE 'rust-server:%' AND i.name IN ('ask_user','solve_plan','solve_finish_step','solve_replan') AND i.status='completed' ORDER BY i.created_at,i.invocation_id")?
            .query_map([session],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut by_message: HashMap<i64, Value> = HashMap::new();
        for (message, name, arguments, result) in records {
            if !visited.contains(&message) {
                continue;
            }
            let Ok(result) = serde_json::from_str::<Value>(&result) else {
                continue;
            };
            let data = by_message
                .entry(message)
                .or_insert_with(|| json!({"questions_and_replies":[],"solve_progress":null}));
            if name == "ask_user" {
                if let Ok(question) = serde_json::from_str::<Value>(&arguments) {
                    data["questions_and_replies"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"question":question,"reply":result["content"]}));
                }
            } else if !result[&name].is_null() {
                data["solve_progress"] = result[&name].clone();
            }
        }
        for (id, message) in &mut chain {
            if let Some(records) = by_message.get(id) {
                message["content"] = json!(format!(
                    "{}\n\nRecorded questions, user replies and solve progress:\n{}",
                    text(message, "content"),
                    records
                ));
            }
        }
        Ok(chain)
    }
    pub fn list(
        &self,
        workspace: Option<&str>,
        limit: i64,
        offset: i64,
        archived: Option<bool>,
    ) -> Result<Vec<Value>> {
        let conn = self.connect()?;
        // Select the page before calculating its summaries. Indexed subqueries
        // keep the work local to the page; no three extra statements per row.
        Ok(conn
            .prepare(
                "SELECT s.*,
                        (SELECT COUNT(*) FROM messages WHERE session_id=s.id AND role!='system') AS message_count,
                        COALESCE((SELECT content FROM messages WHERE session_id=s.id AND role!='system' ORDER BY id DESC LIMIT 1),'') AS last_message,
                        t.id AS latest_turn_id, t.status AS latest_status, t.capability AS latest_capability
                 FROM (
                     SELECT * FROM sessions WHERE deleted_at IS NULL
                     AND (?1 IS NULL OR COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?1)
                     AND (?2 IS NULL OR COALESCE(json_extract(preferences_json,'$.archived'),0)=?2)
                     ORDER BY updated_at DESC,id LIMIT ?3 OFFSET ?4
                 ) s
                 LEFT JOIN turns t ON t.id=(
                     SELECT id FROM turns WHERE session_id=s.id ORDER BY created_at DESC,id DESC LIMIT 1
                 )
                 ORDER BY s.updated_at DESC,s.id",
            )?
            .query_map(params![workspace, archived, limit.clamp(1, 200), offset.max(0)], |r| {
                let mut row = session_row(r)?;
                row["message_count"] = json!(r.get::<_,i64>("message_count")?);
                row["last_message"] = json!(r.get::<_,String>("last_message")?);
                let status = r.get::<_,Option<String>>("latest_status")?.unwrap_or_else(|| "idle".into());
                row["active_turn_id"] = if terminal(&status) || status == "idle" {
                    json!("")
                } else {
                    json!(r.get::<_,Option<String>>("latest_turn_id")?.unwrap_or_default())
                };
                row["status"] = json!(status);
                row["capability"] = json!(r.get::<_,Option<String>>("latest_capability")?.unwrap_or_default());
                Ok(row)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn rename(&self, session: &str, workspace: &str, title: &str) -> Result<Value> {
        self.session(session, workspace)
            .map_err(|_| Error::missing())?;
        let title = title.trim().chars().take(100).collect::<String>();
        if title.is_empty() {
            return Err(Error::invalid("Title cannot be empty."));
        }
        self.connect()?.execute(
            "UPDATE sessions SET title=?,updated_at=? WHERE id=?",
            params![title, now(), session],
        )?;
        Ok(json!({"status":"success","session_id":session,"title":title}))
    }
    pub fn preferences(&self, session: &str, workspace: &str, patch: Value) -> Result<Value> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut prefs:Value=tx.query_row("SELECT preferences_json FROM sessions WHERE id=? AND deleted_at IS NULL AND COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?",params![session,workspace],|r|value(r,"preferences_json",json!({}))).optional()?.ok_or_else(Error::missing)?;
        let target = prefs
            .as_object_mut()
            .ok_or_else(|| Error::conflict("Invalid stored preferences."))?;
        let patch = patch
            .as_object()
            .ok_or_else(|| Error::invalid("Expected preference object."))?;
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
        tx.execute(
            "UPDATE sessions SET preferences_json=?,updated_at=? WHERE id=?",
            params![prefs.to_string(), now(), session],
        )?;
        tx.commit()?;
        Ok(prefs)
    }
    pub fn admit(&self, request: &Value, owner: &str) -> Result<Admission> {
        self.admit_selected(request, owner, &request["llm_selection"])
    }
    pub fn admit_selected(
        &self,
        request: &Value,
        owner: &str,
        selection: &Value,
    ) -> Result<Admission> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = text(request, "workspace_id");
        let supplied = text(request, "session_id");
        let submission = text(request, "client_submission_id");
        let mut canonical = request.clone();
        // Retry transports may acquire a session id only after the first acknowledgment.
        for key in [
            "type",
            "protocol_version",
            "session_id",
            "client_submission_id",
        ] {
            canonical.as_object_mut().unwrap().remove(key);
        }
        let digest = format!("{:x}", Sha256::digest(canonical.to_string().as_bytes()));
        if !submission.is_empty() {
            let prior:Option<SubmissionRow>=tx.query_row("SELECT request_digest,workspace_id,session_id,turn_id,superseded_by FROM turn_submissions WHERE client_submission_id=?",[submission],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            if let Some((old_digest, old_workspace, session, turn, superseded)) = prior {
                if old_digest != digest
                    || old_workspace != workspace
                    || (!supplied.is_empty() && supplied != session)
                    || superseded.is_some()
                {
                    return Err(Error::conflict(
                        "Submission identity belongs to a different request.",
                    ));
                }
                let exists=tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=? AND deleted_at IS NULL AND COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?)",params![session,workspace],|r|r.get::<_,bool>(0))?;
                if !exists {
                    return Err(Error::missing());
                }
                let turn = turn.ok_or_else(|| Error::conflict("Submission is not admitted."))?;
                let user_message=tx.query_row("SELECT id FROM messages WHERE session_id=? AND role='user' AND json_extract(metadata_json,'$.client_submission_id')=?",params![session,submission],|r|r.get(0))?;
                let capability: String =
                    tx.query_row("SELECT capability FROM turns WHERE id=?", [&turn], |r| {
                        r.get(0)
                    })?;
                let capability = Capability::parse(&capability)?;
                return Ok(Admission {
                    capability,
                    session,
                    turn,
                    user_message,
                    replay: true,
                });
            }
        }
        let capability = Capability::parse(text(request, "capability"))?;
        let session = if supplied.is_empty() {
            let sid = id("unified");
            let prefs = json!({"workspace_id":workspace});
            tx.execute("INSERT INTO sessions(id,title,created_at,updated_at,preferences_json) VALUES (?,'New conversation',?,?,?)",params![sid,now(),now(),prefs.to_string()])?;
            sid
        } else {
            let exists=tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=? AND deleted_at IS NULL AND COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?)",params![supplied,workspace],|r|r.get::<_,bool>(0))?;
            if !exists {
                return Err(Error::missing());
            }
            supplied.to_string()
        };
        let active=tx.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE session_id=? AND status IN ('queued','running','waiting_input'))",[&session],|r|r.get::<_,bool>(0))?;
        if active {
            return Err(Error::conflict("Session already has an active turn."));
        }
        let parent = if let Some(parent) = request.get("parent_message_id") {
            if parent.is_null() {
                None
            } else {
                Some(
                    parent
                        .as_i64()
                        .ok_or_else(|| Error::invalid("Invalid parent message."))?,
                )
            }
        } else {
            selected_leaf(&tx, &session)?
        };
        if let Some(parent) = parent
            && !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=? AND session_id=?)",
                params![parent, session],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err(Error::invalid(
                "Parent message belongs to another conversation.",
            ));
        }
        let turn = id("turn");
        tx.execute("INSERT INTO turns(id,session_id,capability,status,created_at,updated_at,owner_id) VALUES (?,?,?,'running',?,?,?)",params![turn,session,capability.id(),now(),now(),owner])?;
        let metadata =
            json!({"client_submission_id":submission,"turn_id":turn,"llm_selection":selection});
        tx.execute("INSERT INTO messages(session_id,role,content,capability,events_json,attachments_json,metadata_json,created_at,parent_message_id) VALUES (?,'user',?,?,'[]','[]',?,?,?)",params![session,text(request,"content"),capability.id(),metadata.to_string(),now(),parent])?;
        let user_message = tx.last_insert_rowid();
        select_path(&tx, &session, user_message)?;
        if !submission.is_empty() {
            tx.execute("INSERT INTO turn_submissions(client_submission_id,workspace_id,request_digest,session_id,turn_id) VALUES (?,?,?,?,?)",params![submission,workspace,digest,session,turn])?;
        }
        if !selection.is_null() {
            tx.execute("UPDATE sessions SET preferences_json=json_set(preferences_json,'$.llm_selection',json(?)) WHERE id=?",params![selection.to_string(),session])?;
        }
        if let Some(language) = request.get("reply_language_override") {
            crate::language::validate_override(language)?;
            tx.execute("UPDATE sessions SET preferences_json=json_set(preferences_json,'$.reply_language_override',json(?)) WHERE id=?", params![language.to_string(), session])?;
        }
        tx.execute(
            "UPDATE sessions SET updated_at=? WHERE id=?",
            params![now(), session],
        )?;
        let event = json!({"session_id":session,"turn_id":turn,"client_submission_id":submission,"user_message_id":user_message,"capability":capability.id()});
        append(&tx, &turn, "session", "", event)?;
        tx.commit()?;
        Ok(Admission {
            capability,
            session,
            turn,
            user_message,
            replay: false,
        })
    }
    pub fn emit(&self, turn: &str, kind: &str, content: &str, metadata: Value) -> Result<()> {
        self.emit_batch(
            turn,
            &[Event {
                kind: kind.into(),
                content: content.into(),
                metadata,
            }],
        )
    }
    /// The entire batch becomes visible to replay readers only after commit.
    /// Cloned stores share this writer; callers run it on the blocking pool.
    pub fn emit_batch(&self, turn: &str, events: &[Event]) -> Result<()> {
        self.emit_batch_timed(turn, events).map(|_| ())
    }
    pub fn emit_batch_timed(&self, turn: &str, events: &[Event]) -> Result<EventWriteTiming> {
        if events.is_empty() {
            return Ok(EventWriteTiming::default());
        }
        let waiting = Instant::now();
        let mut conn = self
            .event_writer
            .lock()
            .map_err(|_| Error::conflict("Event writer is unavailable."))?;
        let writer_mutex_wait_ms = crate::metrics::elapsed(waiting);
        let beginning = Instant::now();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sqlite_begin_ms = crate::metrics::elapsed(beginning);
        let writing = Instant::now();
        let status: String =
            tx.query_row("SELECT status FROM turns WHERE id=?", [turn], |r| r.get(0))?;
        if terminal(&status) {
            return Err(Error::conflict("Turn is already terminal."));
        }
        for event in events {
            append(
                &tx,
                turn,
                &event.kind,
                &event.content,
                event.metadata.clone(),
            )?;
        }
        tx.commit()?;
        Ok(EventWriteTiming {
            writer_mutex_wait_ms,
            sqlite_begin_ms,
            write_commit_ms: crate::metrics::elapsed(writing),
            committed_at: Some(Instant::now()),
        })
    }
    pub(crate) fn start_tool(
        &self,
        turn: &str,
        invocation: &str,
        call: &crate::tools::ToolCall,
    ) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String =
            tx.query_row("SELECT status FROM turns WHERE id=?", [turn], |r| r.get(0))?;
        if status != "running" {
            return Err(Error::conflict("Turn cannot execute tools."));
        }
        tx.execute("INSERT INTO native_tool_invocations(invocation_id,turn_id,call_id,name,arguments_json,status,created_at) VALUES (?,?,?,?,?,'running',?)",params![invocation,turn,call.id,call.name,call.arguments,now()])?;
        append(
            &tx,
            turn,
            "tool_call",
            "",
            json!({"invocation_id":invocation,"tool_call_id":call.id,"tool_name":call.name,"arguments":call.arguments,"tool_input":call.arguments}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn commit_tool(
        &self,
        turn: &str,
        invocation: &str,
        outcome: crate::tools::ToolOutcome,
    ) -> Result<Value> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (call,name,status,prior):(String,String,String,Option<String>)=tx.query_row("SELECT call_id,name,status,result_json FROM native_tool_invocations WHERE invocation_id=? AND turn_id=?",params![invocation,turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if status != "running" {
            return prior
                .and_then(|s| serde_json::from_str(&s).ok())
                .ok_or_else(|| Error::conflict("Tool result is unavailable."));
        }
        let (running,prefs):(String,String)=tx.query_row("SELECT t.status,s.preferences_json FROM turns t JOIN sessions s ON s.id=t.session_id WHERE t.id=? AND s.deleted_at IS NULL",[turn],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if running != "running" {
            return Err(Error::conflict("Turn cannot commit tools."));
        }
        let result = if let Some(mutation) = outcome.mutation {
            let prefs: Value = serde_json::from_str(&prefs)
                .map_err(|_| Error::conflict("Invalid session preferences."))?;
            tx.execute_batch("SAVEPOINT learning_write")?;
            match crate::learning::apply(&tx, text(&prefs, "workspace_id"), invocation, mutation) {
                Ok(result) => {
                    tx.execute_batch("RELEASE learning_write")?;
                    result
                }
                Err(error) if error.status.is_client_error() => {
                    tx.execute_batch("ROLLBACK TO learning_write; RELEASE learning_write")?;
                    json!({"success":false,"error":error.code,"content":error.message})
                }
                Err(error) => return Err(error),
            }
        } else {
            outcome.result
        };
        tx.execute("UPDATE native_tool_invocations SET status=?,result_json=?,finished_at=? WHERE invocation_id=? AND status='running'",params![if result["success"]==true {"completed"}else{"failed"},result.to_string(),now(),invocation])?;
        append(
            &tx,
            turn,
            "tool_result",
            &result.to_string(),
            json!({"invocation_id":invocation,"tool_call_id":call,"tool_name":name,"success":result["success"],"tool_metadata":result}),
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn tool_session_info(&self, admission: &Admission) -> Result<Value> {
        let (title,prefs):(String,Value)=self.connect()?.query_row("SELECT s.title,s.preferences_json FROM sessions s JOIN turns t ON t.session_id=s.id WHERE t.id=? AND s.id=? AND s.deleted_at IS NULL",params![admission.turn,admission.session],|r|Ok((r.get(0)?,value(r,"preferences_json",json!({}))?)))?;
        Ok(
            json!({"session_id":admission.session,"title":title.chars().take(512).collect::<String>(),"workspace_id":text(&prefs,"workspace_id"),"reply_language_override":prefs["reply_language_override"]}),
        )
    }
    pub fn finish(
        &self,
        admission: &Admission,
        status: &str,
        content: &str,
        usage: Value,
        error: Option<&str>,
    ) -> Result<()> {
        self.finish_with_context(admission, status, content, usage, None, error)
    }
    pub(crate) fn finish_with_context(
        &self,
        admission: &Admission,
        status: &str,
        content: &str,
        usage: Value,
        context_budget: Option<Value>,
        error: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: String = tx.query_row(
            "SELECT status FROM turns WHERE id=?",
            [&admission.turn],
            |r| r.get(0),
        )?;
        if terminal(&prior) {
            return Ok(());
        }
        close_tools(&tx, &admission.turn, "interrupted")?;
        tx.execute(
            "DELETE FROM native_turn_checkpoints WHERE turn_id=?",
            [&admission.turn],
        )?;
        let meta = json!({"turn_id":admission.turn,"status":status,"usage_summary":usage});
        tx.execute("INSERT INTO messages(session_id,role,content,capability,events_json,attachments_json,metadata_json,created_at,parent_message_id) VALUES (?,'assistant',?,?,'[]','[]',?,?,?)",params![admission.session,content,admission.capability.id(),meta.to_string(),now(),admission.user_message])?;
        let assistant = tx.last_insert_rowid();
        if let Some(message) = error {
            append(
                &tx,
                &admission.turn,
                "error",
                message,
                json!({"error_code":"provider_error"}),
            )?;
        }
        if status == "completed" {
            append(
                &tx,
                &admission.turn,
                "result",
                "",
                json!({"capability":admission.capability.id(),"response":content,"metadata":{"cost_summary":usage,"usage_summary":usage}}),
            )?;
        }
        append(
            &tx,
            &admission.turn,
            "done",
            "",
            json!({"status":status,"user_message_id":admission.user_message,"assistant_message_id":assistant,"context_budget":context_budget,"cost_summary":usage,"usage_summary":usage}),
        )?;
        // Keep only compact terminal snapshots on the message. The complete
        // trace remains in turn_events and is replayed by cursor.
        tx.execute("UPDATE messages SET events_json=(SELECT json_group_array(json_object('type',type,'source',source,'stage',stage,'content',content,'metadata',json(metadata_json),'seq',seq,'timestamp',timestamp,'turn_id',turn_id)) FROM (SELECT * FROM turn_events WHERE turn_id=? AND (type IN ('result','done') OR (type='tool_result' AND json_extract(metadata_json,'$.success')=1 AND json_extract(metadata_json,'$.tool_name') IN ('render_video_lesson','videogen'))) ORDER BY seq)) WHERE id=?", params![admission.turn,assistant])?;
        tx.execute("UPDATE turns SET status=?,error=?,failure_code=?,retryable=?,assistant_message_id=?,finished_at=?,updated_at=?,state_version=state_version+1 WHERE id=?",params![status,error.unwrap_or(""),if status=="failed" {"provider_error"} else {""},status=="failed",assistant,now(),now(),admission.turn])?;
        tx.execute(
            "UPDATE sessions SET updated_at=? WHERE id=?",
            params![now(), admission.session],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn recover(&self) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active=tx.prepare("SELECT id,owner_id,session_id FROM turns WHERE status IN ('queued','running','waiting_input')")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        if active
            .iter()
            .any(|(_, owner, _)| !owner.starts_with("rust-server:"))
        {
            return Err(Error::conflict(
                "Another backend has active turns. Stop it and complete recovery before switching runtimes.",
            ));
        }
        tx.execute("UPDATE native_browser_runs SET status='interrupted',payload_json=json_set(payload_json,'$.status','interrupted','$.report',json_object('status','interrupted','error','Server stopped. Start a new run explicitly.')) WHERE status='running'", [])?;
        for (turn, _, session) in active {
            let resumable:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_turn_checkpoints c JOIN turns t ON t.id=c.turn_id WHERE c.turn_id=? AND ((c.phase='waiting' AND t.status='waiting_input') OR (c.phase='ready' AND t.status='queued')))",[&turn],|r|r.get(0))?;
            if resumable {
                continue;
            }
            tx.execute(
                "DELETE FROM native_turn_checkpoints WHERE turn_id=?",
                [&turn],
            )?;
            close_tools(&tx, &turn, "interrupted")?;
            let parent = tx.query_row("SELECT id FROM messages WHERE session_id=? AND role='user' AND json_extract(metadata_json,'$.turn_id')=? ORDER BY id DESC LIMIT 1",params![session,turn], |r|r.get::<_,i64>(0)).optional()?;
            let partial = tx.prepare("SELECT content FROM turn_events WHERE turn_id=? AND type='content' ORDER BY seq")?
                .query_map([&turn],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.concat();
            tx.execute("INSERT INTO messages(session_id,role,content,capability,events_json,attachments_json,metadata_json,created_at,parent_message_id) VALUES (?,'assistant',?,(SELECT capability FROM turns WHERE id=?),'[]','[]',?,?,?)",params![session,partial,turn,json!({"turn_id":turn,"status":"failed","failure_code":"worker_lost"}).to_string(),now(),parent])?;
            let assistant = tx.last_insert_rowid();
            append(
                &tx,
                &turn,
                "error",
                "Server stopped before this turn finished.",
                json!({"error_code":"worker_lost","retryable":true}),
            )?;
            append(
                &tx,
                &turn,
                "done",
                "",
                json!({"status":"failed","error_code":"worker_lost","retryable":true,"assistant_message_id":assistant,"user_message_id":parent}),
            )?;
            tx.execute("UPDATE messages SET events_json=(SELECT json_group_array(json_object('type',type,'content',content,'metadata',json(metadata_json),'seq',seq,'turn_id',turn_id)) FROM (SELECT * FROM turn_events WHERE turn_id=? AND type IN ('error','done') ORDER BY seq)) WHERE id=?",params![turn,assistant])?;
            tx.execute("UPDATE turns SET status='failed',error='Server interrupted',failure_code='worker_lost',retryable=1,assistant_message_id=?,finished_at=?,updated_at=?,state_version=state_version+1 WHERE id=?",params![assistant,now(),now(),turn])?;
            tx.execute(
                "UPDATE sessions SET updated_at=? WHERE id=?",
                params![now(), session],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn selected_leaf(conn: &Connection, session: &str) -> Result<Option<i64>> {
    let prefs = conn.query_row(
        "SELECT preferences_json FROM sessions WHERE id=?",
        [session],
        |r| value(r, "preferences_json", json!({})),
    )?;
    let parents = conn
        .prepare("SELECT id,parent_message_id FROM messages WHERE session_id=? ORDER BY id")?
        .query_map([session], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    if parents.is_empty() {
        return Ok(None);
    }
    let mut children: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
    for (&id, &parent) in &parents {
        if parent.is_some_and(|p| !parents.contains_key(&p)) {
            return Err(Error::conflict("Conversation branch is incomplete."));
        }
        children.entry(parent).or_default().push(id);
    }
    // Compute the longest continuation in linear time without recursion. This
    // matches Web buildVisiblePath: depth first, then the newest sibling ID.
    let mut remaining: HashMap<i64, usize> = parents
        .keys()
        .map(|&id| (id, children.get(&Some(id)).map_or(0, Vec::len)))
        .collect();
    let mut ready: VecDeque<i64> = remaining
        .iter()
        .filter_map(|(&id, &n)| (n == 0).then_some(id))
        .collect();
    let mut depths: HashMap<i64, usize> = HashMap::new();
    let mut processed = 0;
    while let Some(id) = ready.pop_front() {
        processed += 1;
        let depth = depths.get(&id).copied().unwrap_or(0) + 1;
        if let Some(parent) = parents[&id] {
            let longest = depths.entry(parent).or_default();
            *longest = (*longest).max(depth);
            let count = remaining.get_mut(&parent).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.push_back(parent);
            }
        }
    }
    if processed != parents.len() {
        return Err(Error::conflict("Conversation branch contains a cycle."));
    }
    let mut parent = None;
    while let Some(siblings) = children.get(&parent) {
        let key = parent.map_or_else(|| "null".into(), |id: i64| id.to_string());
        let preferred = prefs["selected_branches"][&key].as_i64().or_else(|| {
            if parent.is_none() {
                prefs["selected_branches"]["root"].as_i64()
            } else {
                None
            }
        });
        parent = preferred.filter(|id| siblings.contains(id)).or_else(|| {
            siblings
                .iter()
                .max_by_key(|id| (depths.get(id).copied().unwrap_or(0), **id))
                .copied()
        });
    }
    Ok(parent)
}

fn select_path(conn: &Connection, session: &str, leaf: i64) -> Result<()> {
    let mut prefs = conn.query_row(
        "SELECT preferences_json FROM sessions WHERE id=?",
        [session],
        |r| value(r, "preferences_json", json!({})),
    )?;
    if !prefs.is_object() {
        return Err(Error::conflict("Invalid stored preferences."));
    }
    if !prefs["selected_branches"].is_object() {
        prefs["selected_branches"] = json!({});
    }
    let branches = prefs["selected_branches"].as_object_mut().unwrap();
    branches.remove("root");
    let mut child = leaf;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(child) {
            return Err(Error::conflict("Conversation branch contains a cycle."));
        }
        let parent: Option<i64> = conn.query_row(
            "SELECT parent_message_id FROM messages WHERE session_id=? AND id=?",
            params![session, child],
            |r| r.get(0),
        )?;
        branches.insert(
            parent.map_or_else(|| "null".into(), |p| p.to_string()),
            json!(child),
        );
        match parent {
            Some(id) => child = id,
            None => break,
        }
    }
    conn.execute(
        "UPDATE sessions SET preferences_json=? WHERE id=?",
        params![prefs.to_string(), session],
    )?;
    Ok(())
}
fn append(conn: &Connection, turn: &str, kind: &str, content: &str, metadata: Value) -> Result<()> {
    conn.execute("INSERT INTO turn_events(turn_id,seq,type,source,stage,content,metadata_json,timestamp,created_at) SELECT ?,COALESCE(MAX(seq),0)+1,?,COALESCE((SELECT NULLIF(capability,'') FROM turns WHERE id=?),'chat'),'responding',?,?,?,? FROM turn_events WHERE turn_id=?",params![turn,kind,turn,content,metadata.to_string(),now(),now(),turn])?;
    Ok(())
}

impl Store {
    pub fn command_receipt(&self, command: &str, turn: &str) -> Result<Option<Value>> {
        let row: Option<(String, String)> = self
            .connect()?
            .query_row(
                "SELECT turn_id,response_json FROM native_command_receipts WHERE command_id=?",
                [command],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((target, body)) if target == turn => {
                let receipt: Value = serde_json::from_str(&body)
                    .map_err(|_| Error::conflict("Invalid stored command receipt."))?;
                if receipt["command_type"] != "cancel_turn" {
                    return Err(Error::conflict(
                        "Command identifier belongs to a different command type.",
                    ));
                }
                Ok(Some(receipt))
            }
            Some(_) => Err(Error::conflict("Command identity belongs to another turn.")),
            None => Ok(None),
        }
    }
    pub fn save_command_receipt(&self, command: &str, turn: &str, value: &Value) -> Result<()> {
        self.connect()?.execute("INSERT INTO native_command_receipts(command_id,turn_id,command_type,response_json) VALUES (?,?,'cancel_turn',?)",params![command,turn,value.to_string()])?;
        Ok(())
    }
    pub fn branch_selection(
        &self,
        session: &str,
        workspace: &str,
        selection: Value,
    ) -> Result<Value> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut prefs:Value=tx.query_row("SELECT preferences_json FROM sessions WHERE id=? AND deleted_at IS NULL AND COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?",params![session,workspace],|r|value(r,"preferences_json",json!({}))).optional()?.ok_or_else(Error::missing)?;
        if !prefs.is_object() {
            return Err(Error::conflict("Invalid stored preferences."));
        }
        let map = selection
            .as_object()
            .ok_or_else(|| Error::invalid("Expected selected_branches object."))?;
        let mut normalized = serde_json::Map::new();
        for (key, child) in map {
            let parent = if matches!(key.as_str(), "root" | "null") {
                None
            } else {
                Some(
                    key.parse::<i64>()
                        .ok()
                        .filter(|id| *id > 0)
                        .ok_or_else(|| Error::invalid("Invalid parent message."))?,
                )
            };
            let child = child
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or_else(|| Error::invalid("Invalid child message."))?;
            if !tx.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=? AND id=? AND parent_message_id IS ?)",params![session,child,parent],|r|r.get::<_,bool>(0))? {
                return Err(Error::invalid("Selected branch does not belong to this conversation."));
            }
            let key = parent.map_or_else(|| "null".into(), |id| id.to_string());
            if normalized
                .insert(key, json!(child))
                .is_some_and(|old| old != child)
            {
                return Err(Error::invalid("Conflicting branch selections."));
            }
        }
        let selection = Value::Object(normalized);
        prefs["selected_branches"] = selection.clone();
        tx.execute(
            "UPDATE sessions SET preferences_json=?,updated_at=? WHERE id=?",
            params![prefs.to_string(), now(), session],
        )?;
        tx.commit()?;
        Ok(json!({"selected_branches":selection}))
    }
    pub fn delete_session(&self, session: &str, workspace: &str) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=? AND COALESCE(json_extract(preferences_json,'$.workspace_id'),'')=?)",params![session,workspace],|r|r.get(0))?;
        if !exists {
            return Err(Error::missing());
        }
        // Until capability-owned artifact cleanup is migrated, refuse to erase
        // sessions with dependent learning records rather than orphan them.
        let children:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE json_extract(preferences_json,'$.parent_session_id')=?)",[session],|r|r.get(0))?;
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE session_id=? AND status IN ('queued','running','waiting_input'))",[session],|r|r.get(0))?;
        let advanced:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM messages m WHERE m.session_id=? AND COALESCE(m.capability,'') NOT IN ('','chat') AND NOT (m.capability IN ('ask_questions','deep_solve','video_lesson') AND EXISTS(SELECT 1 FROM turns t WHERE t.session_id=m.session_id AND t.capability=m.capability AND t.owner_id LIKE 'rust-server:%' AND (t.id=json_extract(CASE WHEN json_valid(m.metadata_json) THEN m.metadata_json ELSE '{}' END,'$.turn_id') OR t.assistant_message_id=m.id))))",[session],|r|r.get(0))?;
        if children || advanced {
            return Err(Error::unsupported(
                "This conversation has learning data whose cleanup is not migrated. Archive it instead.",
            ));
        }
        if active {
            return Err(Error::conflict(
                "Cancel the active turn before deleting the conversation.",
            ));
        }
        tx.execute("DELETE FROM sessions WHERE id=?", [session])?;
        tx.commit()?;
        Ok(())
    }
}

// Terminal transactions pair every persisted tool start with a durable failure
// result, including when a worker was cancelled or lost between commits.
fn close_tools(tx: &rusqlite::Transaction<'_>, turn: &str, status: &str) -> Result<()> {
    let rows=tx.prepare("SELECT invocation_id,call_id,name FROM native_tool_invocations WHERE turn_id=? AND status='running' ORDER BY created_at,invocation_id")?
        .query_map([turn],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (invocation, call, name) in rows {
        let result =
            json!({"success":false,"error":status,"content":"Tool execution was interrupted."});
        tx.execute("UPDATE native_tool_invocations SET status=?,result_json=?,finished_at=? WHERE invocation_id=?",params![status,result.to_string(),now(),invocation])?;
        if name == "ask_user" {
            append(
                tx,
                turn,
                "progress",
                "",
                json!({"ask_user_resolved":true,"ask_user_tool_call_id":call,"trace_kind":"user_reply","cancelled":true}),
            )?;
        }
        append(
            tx,
            turn,
            "tool_result",
            &result.to_string(),
            json!({"invocation_id":invocation,"tool_call_id":call,"tool_name":name,"success":false,"tool_metadata":result}),
        )?;
    }
    Ok(())
}

impl Store {
    pub(crate) fn park(&self, checkpoint: &crate::application::Checkpoint) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pause = &checkpoint.pause;
        let turn = &checkpoint.admission.turn;
        let updated=tx.execute("UPDATE turns SET status='waiting_input',updated_at=?,state_version=state_version+1 WHERE id=? AND status='running'",params![now(),turn])?;
        if updated != 1 {
            return Err(Error::conflict("Turn cannot wait for input."));
        }
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_tool_invocations WHERE turn_id=? AND invocation_id=? AND name='ask_user' AND status='running')",params![turn,pause.invocation],|r|r.get(0))?;
        if !active {
            return Err(Error::conflict("Question tool is not running."));
        }
        tx.execute("INSERT INTO native_turn_checkpoints(turn_id,phase,payload_json) VALUES (?,'waiting',?) ON CONFLICT(turn_id) DO UPDATE SET phase='waiting',payload_json=excluded.payload_json",params![turn,serde_json::to_string(checkpoint).map_err(|_|Error::conflict("Unable to save turn checkpoint."))?])?;
        let metadata = json!({"tool_name":"ask_user","tool_call_id":pause.call.id,"invocation_id":pause.invocation,"ask_user":pause.question,"tool_metadata":{"ask_user":pause.question}});
        append(&tx, turn, "tool_result", "", metadata.clone())?;
        append(&tx, turn, "wait_for_input", "", metadata)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn checkpoint(&self, turn: &str) -> Result<Option<crate::application::Checkpoint>> {
        let data: Option<String> = self
            .connect()?
            .query_row(
                "SELECT payload_json FROM native_turn_checkpoints WHERE turn_id=?",
                [turn],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| {
            serde_json::from_str(&s).map_err(|_| Error::conflict("Invalid turn checkpoint."))
        })
        .transpose()
    }
    pub(crate) fn ready_checkpoints(&self) -> Result<Vec<crate::application::Checkpoint>> {
        let conn = self.connect()?;
        let data=conn.prepare("SELECT c.payload_json FROM native_turn_checkpoints c JOIN turns t ON t.id=c.turn_id WHERE c.phase='ready' AND t.status='queued' ORDER BY t.created_at,t.id")?
            .query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        data.into_iter()
            .map(|s| {
                serde_json::from_str(&s).map_err(|_| Error::conflict("Invalid turn checkpoint."))
            })
            .collect()
    }
    pub(crate) fn claim_checkpoint(&self, turn: &str, owner: &str) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute("UPDATE native_turn_checkpoints SET phase='executing' WHERE turn_id=? AND phase='ready'",[turn])?!=1 {return Err(Error::conflict("Turn continuation is already claimed."));}
        if tx.execute("UPDATE turns SET status='running',owner_id=?,updated_at=?,state_version=state_version+1 WHERE id=? AND status='queued'",params![owner,now(),turn])?!=1 {return Err(Error::conflict("Turn continuation is not queued."));}
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn accept_reply(
        &self,
        command: &Value,
        workspace: &str,
        settings: &Settings,
    ) -> Result<Value> {
        crate::ask_user::command_identity(command)?;
        let turn = text(command, "turn_id");
        let cid = text(command, "command_id");
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scoped:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM turns t JOIN sessions s ON s.id=t.session_id WHERE t.id=? AND s.deleted_at IS NULL AND COALESCE(json_extract(s.preferences_json,'$.workspace_id'),'')=?)",params![turn,workspace],|r|r.get(0))?;
        if !scoped {
            return Err(Error::missing());
        }
        let digest=format!("{:x}",Sha256::digest(json!({"type":command["type"],"turn_id":turn,"tool_call_id":command["tool_call_id"],"text":command["text"],"answers":command["answers"]}).to_string().as_bytes()));
        let receipt:Option<(String,String,Option<String>)>=tx.query_row("SELECT r.turn_id,r.response_json,d.request_digest FROM native_command_receipts r LEFT JOIN native_reply_commands d ON d.command_id=r.command_id WHERE r.command_id=?",[cid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((target, receipt, prior)) = receipt {
            if target != turn || prior.as_deref() != Some(&digest) {
                return Err(Error::conflict(
                    "Command identifier was reused with a different reply.",
                ));
            }
            return serde_json::from_str(&receipt)
                .map_err(|_| Error::conflict("Invalid stored reply receipt."));
        }
        let data:Option<String>=tx.query_row("SELECT c.payload_json FROM native_turn_checkpoints c JOIN turns t ON t.id=c.turn_id WHERE c.turn_id=? AND c.phase='waiting' AND t.status='waiting_input'",[turn],|r|r.get(0)).optional()?;
        let mut checkpoint: crate::application::Checkpoint = serde_json::from_str(
            &data.ok_or_else(|| Error::conflict("This turn is not waiting for a reply."))?,
        )
        .map_err(|_| Error::conflict("Invalid turn checkpoint."))?;
        if command["tool_call_id"] != checkpoint.pause.call.id {
            return Err(Error::conflict("This question is no longer active."));
        }
        let reply = crate::ask_user::reply(command, &checkpoint.pause.question)?;
        if reply.to_string().len() > crate::tools::MAX_ARGUMENT_BYTES {
            return Err(Error::invalid("Reply exceeds size limit."));
        }
        checkpoint.reply = Some(reply.clone());
        checkpoint.resume_input(settings)?;
        let pause = &checkpoint.pause;
        if tx.execute("UPDATE native_tool_invocations SET status='completed',result_json=?,finished_at=? WHERE invocation_id=? AND turn_id=? AND status='running'",params![reply.to_string(),now(),pause.invocation,turn])?!=1 {return Err(Error::conflict("Question invocation is no longer waiting."));}
        append(
            &tx,
            turn,
            "tool_result",
            &reply.to_string(),
            json!({"tool_name":"ask_user","tool_call_id":pause.call.id,"invocation_id":pause.invocation,"success":true,"tool_metadata":reply}),
        )?;
        append(
            &tx,
            turn,
            "progress",
            "",
            json!({"trace_kind":"user_reply","ask_user_resolved":true,"ask_user_tool_call_id":pause.call.id,"reply_preview":reply["content"]["text"],"answers":reply["content"]["answers"]}),
        )?;
        tx.execute(
            "UPDATE native_turn_checkpoints SET phase='ready',payload_json=? WHERE turn_id=?",
            params![
                serde_json::to_string(&checkpoint)
                    .map_err(|_| Error::conflict("Unable to save continuation."))?,
                turn
            ],
        )?;
        tx.execute("UPDATE turns SET status='queued',updated_at=?,state_version=state_version+1 WHERE id=?",params![now(),turn])?;
        let receipt = json!({"type":"command_ack","command_id":cid,"command_type":"submit_user_reply","turn_id":turn,"accepted":true,"protocol_version":"2.0"});
        tx.execute("INSERT INTO native_command_receipts(command_id,turn_id,command_type,response_json) VALUES (?,?,'submit_user_reply',?)",params![cid,turn,receipt.to_string()])?;
        tx.execute(
            "INSERT INTO native_reply_commands(command_id,request_digest) VALUES (?,?)",
            params![cid, digest],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}
