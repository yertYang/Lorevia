//! Workspace-scoped notebook and memory storage. Tool mutations are applied by
//! the application's tool-result transaction, not by the asynchronous executor.
use crate::{
    Error, Result,
    config::{Settings, read_json, safe_path},
    store::{Store, now, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

pub const SURFACES: [&str; 7] = [
    "chat", "notebook", "quiz", "kb", "book", "partner", "cowriter",
];
pub const SLOTS: [&str; 4] = ["recent", "profile", "scope", "preferences"];
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolFlags {
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub video_lesson: bool,
    #[serde(default)]
    pub videogen: Option<crate::video::Selection>,
    pub notebooks: bool,
    pub memory: bool,
    #[serde(default)]
    pub knowledge_bases: Vec<String>,
    #[serde(default)]
    pub search: Option<crate::search::Selection>,
    #[serde(default)]
    pub browser: Option<crate::browser_testing::Selection>,
    #[serde(default)]
    pub study_target: Option<crate::study::Target>,
    #[serde(default)]
    pub proposal: Option<crate::study::ProposalContext>,
}
#[derive(Clone, Debug)]
pub enum Mutation {
    Video(crate::video::Artifact),
    Study(crate::study::Mutation),
    Browser(crate::browser_testing::Mutation),
    SaveFavorite {
        notebook: String,
        title: String,
        content: String,
        note: String,
    },
    Preference {
        content: String,
        target: Option<String>,
    },
    EditFavorite {
        notebook: String,
        record: String,
        patch: Value,
    },
}
pub fn string(input: &Value, key: &str, max: usize, required: bool) -> Result<String> {
    let s = match input.get(key) {
        None if !required => "",
        Some(Value::String(s)) => s.as_str(),
        _ => return Err(Error::invalid(format!("Invalid {key}."))),
    };
    if s.len() > max || (required && s.trim().is_empty()) {
        return Err(Error::invalid(format!("Invalid {key} length.")));
    }
    Ok(s.into())
}
fn decode(raw: String) -> Result<Value> {
    serde_json::from_str(&raw)
        .map_err(|_| Error::conflict("Stored learning data contains invalid JSON."))
}
pub fn notebook(conn: &Connection, scope: &str, id: &str) -> Result<Value> {
    let row:Option<(String,i64)>=conn.query_row("SELECT payload_json,revision FROM native_notebooks WHERE workspace_id=? AND id=? AND deleted=0",params![scope,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (raw, revision) = row.ok_or_else(Error::missing)?;
    let mut value = decode(raw)?;
    if !value.is_object() || !value["records"].is_array() {
        return Err(Error::conflict("Invalid notebook document."));
    }
    value["revision"] = json!(revision);
    Ok(value)
}
fn save_notebook(conn: &Connection, scope: &str, value: &mut Value) -> Result<()> {
    value["updated_at"] = json!(now());
    conn.execute("UPDATE native_notebooks SET payload_json=?,revision=revision+1 WHERE workspace_id=? AND id=? AND deleted=0",params![value.to_string(),scope,text(value,"id")])?;
    Ok(())
}
pub fn list_notebooks(conn: &Connection, scope: &str) -> Result<Vec<Value>> {
    let rows=conn.prepare("SELECT json_remove(payload_json,'$.records'),json_array_length(payload_json,'$.records'),revision FROM native_notebooks WHERE workspace_id=? AND deleted=0 ORDER BY json_extract(payload_json,'$.updated_at') DESC,id")?.query_map([scope],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(raw, count, revision)| {
            let mut value = decode(raw)?;
            if !value.is_object() {
                return Err(Error::conflict("Invalid notebook document."));
            }
            value["record_count"] = json!(count);
            value["revision"] = json!(revision);
            Ok(value)
        })
        .collect()
}

pub fn create_notebook(conn: &Connection, scope: &str, input: &Value) -> Result<Value> {
    let mut value = json!({"id":uuid::Uuid::new_v4().to_string(),"name":string(input,"name",400,true)?,"description":string(input,"description",4000,false)?,"color":input.get("color").cloned().unwrap_or(json!("#3B82F6")),"icon":input.get("icon").cloned().unwrap_or(json!("book")),"created_at":now(),"updated_at":now(),"records":[]});
    string(&value, "color", 100, true)?;
    string(&value, "icon", 100, true)?;
    conn.execute(
        "INSERT INTO native_notebooks(workspace_id,id,payload_json) VALUES (?,?,?)",
        params![scope, text(&value, "id"), value.to_string()],
    )?;
    value["revision"] = json!(1);
    Ok(value)
}
pub fn update_notebook(conn: &Connection, scope: &str, id: &str, input: &Value) -> Result<Value> {
    let mut value = notebook(conn, scope, id)?;
    check_revision(&value, input)?;
    for (key, max) in [
        ("name", 400),
        ("description", 4000),
        ("color", 100),
        ("icon", 100),
    ] {
        if input.get(key).is_some_and(|v| !v.is_null()) {
            value[key] = json!(string(input, key, max, key != "description")?);
        }
    }
    save_notebook(conn, scope, &mut value)?;
    notebook(conn, scope, id)
}
fn check_revision(current: &Value, input: &Value) -> Result<()> {
    if let Some(expected) = input.get("expected_revision")
        && expected != &current["revision"]
    {
        return Err(Error::conflict(
            "This document changed. Reload before saving.",
        ));
    }
    Ok(())
}
fn record_input(
    conn: &Connection,
    scope: &str,
    input: &Value,
    id: &str,
) -> Result<(Vec<Value>, Value)> {
    let ids = input["notebook_ids"]
        .as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= 50)
        .ok_or_else(|| Error::invalid("Choose 1-50 notebooks."))?;
    let mut notebooks = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| Error::invalid("Invalid notebook ID."))?;
        if seen.insert(id) {
            notebooks.push(notebook(conn, scope, id)?);
        }
    }
    let kind = input["record_type"].as_str().unwrap_or("chat");
    if ![
        "solve",
        "question",
        "research",
        "chat",
        "co_writer",
        "tutorbot",
        "reading",
        "video_learning",
    ]
    .contains(&kind)
    {
        return Err(Error::invalid("Invalid record type."));
    }
    let output = string(input, "output", 1024 * 1024, true)?;
    let summary = string(input, "summary", 16384, false)?;
    let summary = if summary.is_empty() {
        output.chars().take(240).collect::<String>()
    } else {
        summary
    };
    let record = json!({"id":id,"type":kind,"title":string(input,"title",1000,true)?,"summary":summary,"user_query":string(input,"user_query",65536,false)?,"output":output,"metadata":input.get("metadata").cloned().unwrap_or(json!({})),"kb_name":input["kb_name"],"created_at":now()});
    if !record["metadata"].is_object() {
        return Err(Error::invalid("Record metadata must be an object."));
    }
    Ok((notebooks, record))
}
pub fn validate_record(conn: &Connection, scope: &str, input: &Value) -> Result<()> {
    record_input(conn, scope, input, "validation").map(|_| ())
}
pub fn add_record(conn: &Connection, scope: &str, input: &Value, id: &str) -> Result<Value> {
    let (notebooks, record) = record_input(conn, scope, input, id)?;
    let summary = record["summary"].clone();
    let mut added = Vec::new();
    for mut nb in notebooks {
        nb["records"]
            .as_array_mut()
            .ok_or_else(|| Error::conflict("Invalid notebook records."))?
            .push(record.clone());
        save_notebook(conn, scope, &mut nb)?;
        added.push(nb["id"].clone());
    }
    Ok(
        json!({"success":true,"record":record,"summary":summary,"summary_source":if input["summary"].as_str().unwrap_or("").is_empty(){"excerpt"}else{"provided"},"added_to_notebooks":added}),
    )
}
pub fn record_action(
    conn: &Connection,
    scope: &str,
    notebook_id: &str,
    record_id: &str,
    action: &str,
    input: &Value,
) -> Result<Value> {
    let mut source = notebook(conn, scope, notebook_id)?;
    check_revision(&source, input)?;
    let records = source["records"]
        .as_array_mut()
        .ok_or_else(|| Error::conflict("Invalid notebook records."))?;
    let index = records
        .iter()
        .position(|r| r["id"] == record_id)
        .ok_or_else(Error::missing)?;
    let mut record = records[index].clone();
    match action {
        "delete" => {
            records.remove(index);
        }
        "update" => {
            if input.get("output").is_some() || input.get("user_query").is_some() {
                return Err(Error::invalid(
                    "Favorites preserve their original content. Create a new record for an edited body.",
                ));
            }
            for key in ["title", "summary", "metadata", "kb_name"] {
                if let Some(value) = input.get(key) {
                    if key == "metadata" && !value.is_object() {
                        return Err(Error::invalid("Invalid metadata."));
                    }
                    if matches!(key, "title" | "summary") {
                        string(input, key, 16384, key == "title")?;
                    }
                    record[key] = value.clone();
                }
            }
            records[index] = record.clone();
        }
        "copy" | "move" => {
            let target = string(input, "target_notebook_id", 256, true)?;
            if target == notebook_id {
                return Err(Error::invalid("Choose a different destination notebook."));
            }
            let mut destination = notebook(conn, scope, &target)?;
            if action == "copy" {
                record["id"] = json!(uuid::Uuid::new_v4().to_string());
                record["created_at"] = json!(now());
            }
            if destination["records"]
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["id"] == record["id"]))
            {
                return Err(Error::conflict("Destination already contains this record."));
            }
            destination["records"]
                .as_array_mut()
                .ok_or_else(|| Error::conflict("Invalid notebook records."))?
                .push(record.clone());
            save_notebook(conn, scope, &mut destination)?;
            if action == "move" {
                records.remove(index);
            }
        }
        _ => return Err(Error::invalid("Invalid record operation.")),
    }
    save_notebook(conn, scope, &mut source)?;
    Ok(json!({"success":true,"record":record}))
}
pub fn memory_key(layer: &str, key: &str) -> Result<()> {
    if (layer == "L2" && SURFACES.contains(&key)) || (layer == "L3" && SLOTS.contains(&key)) {
        Ok(())
    } else {
        Err(Error::missing())
    }
}
pub fn read_memory(conn: &Connection, scope: &str, layer: &str, key: &str) -> Result<Value> {
    memory_key(layer, key)?;
    let row:Option<(String,i64,f64)>=conn.query_row("SELECT content,revision,updated_at FROM native_memory_docs WHERE workspace_id=? AND layer=? AND key=?",params![scope,layer,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let (content, revision, updated) = row.unwrap_or((String::new(), 0, 0.0));
    let updated: Option<String> = if revision > 0 {
        conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ',?,'unixepoch')",
            [updated],
            |r| r.get(0),
        )?
    } else {
        None
    };
    Ok(json!({"layer":layer,"key":key,"content":content,"revision":revision,"updated_at":updated}))
}
pub fn write_memory(
    conn: &Connection,
    scope: &str,
    layer: &str,
    key: &str,
    input: &Value,
) -> Result<Value> {
    let old = read_memory(conn, scope, layer, key)?;
    check_revision(&old, input)?;
    if input.get("content").is_none() {
        return Err(Error::invalid("content is required."));
    }
    let content = string(input, "content", 512 * 1024, false)?;
    conn.execute("INSERT INTO native_memory_versions(workspace_id,layer,key,content,created_at) VALUES (?,?,?,?,?)",params![scope,layer,key,text(&old,"content"),now()])?;
    conn.execute("INSERT INTO native_memory_docs(workspace_id,layer,key,content,updated_at) VALUES (?,?,?,?,?) ON CONFLICT(workspace_id,layer,key) DO UPDATE SET content=excluded.content,revision=revision+1,updated_at=excluded.updated_at",params![scope,layer,key,content,now()])?;
    read_memory(conn, scope, layer, key)
}
/// Public editors must name the revision they read. Internal tool mutations
/// already run under the owning turn's write transaction and use write_memory.
pub fn write_memory_checked(
    conn: &Connection,
    scope: &str,
    layer: &str,
    key: &str,
    input: &Value,
) -> Result<Value> {
    if input.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !["content", "expected_revision"].contains(&k.as_str()))
    }) || !input["content"].is_string()
        || input["expected_revision"].as_u64().is_none()
    {
        return Err(Error::invalid(
            "content and a non-negative expected_revision are required.",
        ));
    }
    write_memory(conn, scope, layer, key, input)
}
pub fn memory_versions(conn: &Connection, scope: &str, layer: &str, key: &str) -> Result<Value> {
    memory_key(layer, key)?;
    let rows=conn.prepare("SELECT id,created_at FROM native_memory_versions WHERE workspace_id=? AND layer=? AND key=? ORDER BY id DESC LIMIT 100")?
        .query_map(params![scope,layer,key],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"created_at":r.get::<_,f64>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({"versions":rows}))
}
pub fn memory_overview(conn: &Connection, scope: &str) -> Result<Value> {
    let mut docs = Vec::new();
    for (layer, keys) in [("L2", SURFACES.as_slice()), ("L3", SLOTS.as_slice())] {
        for key in keys {
            let doc = read_memory(conn, scope, layer, key)?;
            docs.push(json!({"layer":layer,"key":key,"revision":doc["revision"],"exists":doc["revision"].as_u64().unwrap_or(0)>0,"updated_at":doc["updated_at"],"entry_count":text(&doc,"content").lines().filter(|l|l.trim_start().starts_with("- ")).count(),"backlog":0}));
        }
    }
    Ok(
        json!({"docs":docs,"backups":[],"native_features":{"manual_edit":true,"versions":true,"consolidation":true,"automatic_consolidation":false,"automatic_cleanup_proposals":true,"provenance_graph":true}}),
    )
}
pub fn restore_memory(
    conn: &Connection,
    scope: &str,
    layer: &str,
    key: &str,
    version: i64,
    input: &Value,
) -> Result<Value> {
    memory_key(layer, key)?;
    let content:Option<String>=conn.query_row("SELECT content FROM native_memory_versions WHERE id=? AND workspace_id=? AND layer=? AND key=?",params![version,scope,layer,key],|r|r.get(0)).optional()?;
    let content = content.ok_or_else(Error::missing)?;
    if input
        .as_object()
        .is_none_or(|o| o.keys().any(|k| k != "expected_revision"))
    {
        return Err(Error::invalid("Expected a revision restore object."));
    }
    write_memory_checked(
        conn,
        scope,
        layer,
        key,
        &json!({"content":content,"expected_revision":input["expected_revision"]}),
    )
}
pub fn apply(conn: &Connection, scope: &str, id: &str, mutation: Mutation) -> Result<Value> {
    match mutation {
        Mutation::Video(artifact) => crate::video::publish(conn, scope, id, artifact),
        Mutation::Study(mutation) => crate::study::apply(conn, scope, mutation),
        Mutation::Browser(mutation) => crate::browser_testing::apply(conn, scope, id, mutation),
        Mutation::SaveFavorite {
            notebook,
            title,
            content,
            note,
        } => {
            let saved = add_record(
                conn,
                scope,
                &json!({"notebook_ids":[notebook],"title":title,"output":content,"summary":note,"record_type":"chat"}),
                id,
            )?;
            Ok(
                json!({"success":true,"content":{"record_id":id,"notebook_id":notebook,"title":title,"summary_preview":text(&saved["record"],"summary").chars().take(240).collect::<String>()},"save_favorite":{"record_id":id,"notebook_id":notebook}}),
            )
        }
        Mutation::EditFavorite {
            notebook,
            record,
            patch,
        } => {
            let result = record_action(conn, scope, &notebook, &record, "update", &patch)?;
            Ok(
                json!({"success":true,"content":{"record_id":record,"notebook_id":notebook,"title":result["record"]["title"],"summary_preview":text(&result["record"],"summary").chars().take(240).collect::<String>()}}),
            )
        }
        Mutation::Preference { content, target } => {
            let old = read_memory(conn, scope, "L3", "preferences")?;
            let mut current = text(&old, "content").to_owned();
            let entry_id = target.clone().unwrap_or_else(|| memory_id(id));
            if let Some(target) = target {
                let marker = format!("<!--{target}-->");
                if !current
                    .lines()
                    .any(|line| line.trim_end().ends_with(&marker))
                {
                    return Err(Error::missing());
                }
                current = current
                    .lines()
                    .map(|line| {
                        if line.trim_end().ends_with(&marker) {
                            format!("- {} {marker}", content.trim())
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                write_memory(
                    conn,
                    scope,
                    "L3",
                    "preferences",
                    &json!({"content":current}),
                )?;
            } else if let Some(line) = current.lines().find(|line| {
                line.trim().trim_start_matches("- ").split(" <!--").next() == Some(content.trim())
            }) {
                let existing = line
                    .split("<!--")
                    .nth(1)
                    .and_then(|v| v.split("-->").next());
                return Ok(
                    json!({"success":true,"content":"Preference already saved.","write_memory":{"document":"L3/preferences","entry_id":existing,"deduplicated":true}}),
                );
            } else {
                current.push_str(&format!("\n- {} <!--{entry_id}-->\n", content.trim()));
                write_memory(
                    conn,
                    scope,
                    "L3",
                    "preferences",
                    &json!({"content":current}),
                )?;
            }
            Ok(
                json!({"success":true,"content":"Preference saved.","write_memory":{"document":"L3/preferences","entry_id":entry_id}}),
            )
        }
    }
}
pub(crate) fn legacy_root(settings: &Settings, scope: &str) -> Result<Option<PathBuf>> {
    if scope.is_empty() {
        return Ok(Some(settings.user_root.join("workspace")));
    }
    let catalog = settings.user_root.join("runtime/workspaces.sqlite3");
    safe_path(&settings.user_root, &catalog)?;
    if !catalog.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(catalog, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let raw: Option<String> = conn
        .query_row("SELECT payload FROM workspaces WHERE id=?", [scope], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(raw) = raw else { return Ok(None) };
    let entry = decode(raw)?;
    if entry["workspace_id"] != scope {
        return Err(Error::conflict("Invalid registered workspace identity."));
    }
    // General/system catalog entries are not custom learning roots. Keep
    // ordinary conversations usable without importing their files as a
    // synthetic application-data workspace.
    if entry["kind"] != "workspace" || entry["archived"] == true {
        return Ok(None);
    }
    let root = PathBuf::from(text(&entry, "path"));
    if !root.is_absolute()
        || root.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(Error::conflict("Invalid registered workspace root."));
    }
    safe_path(&root, &root)?;
    Ok(Some(root))
}
pub fn bootstrap(settings: &Settings, store: &Store, scope: &str) -> Result<ToolFlags> {
    let mut conn = store.connect()?;
    if conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_learning_imports WHERE workspace_id=?)",
        [scope],
        |r| r.get::<_, bool>(0),
    )? {
        return flags(&conn, scope);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(root) = legacy_root(settings, scope)? {
        let notebooks = root.join("notebook");
        safe_path(&root, &notebooks)?;
        if notebooks.is_dir() {
            for file in std::fs::read_dir(&notebooks)? {
                let path = file?.path();
                if path.extension().is_none_or(|e| e != "json")
                    || path
                        .file_name()
                        .is_some_and(|n| n == "notebooks_index.json")
                {
                    continue;
                }
                safe_path(&root, &path)?;
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| Error::conflict("Invalid notebook filename."))?;
                if tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_notebooks WHERE workspace_id=? AND id=?)",
                    params![scope, id],
                    |r| r.get::<_, bool>(0),
                )? {
                    continue;
                }
                if std::fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
                    return Err(Error::conflict("Legacy notebook exceeds the import limit."));
                }
                let value = read_json(&path, Value::Null)?;
                if value["id"] != id || !value["records"].is_array() {
                    return Err(Error::conflict(
                        "A legacy notebook is unreadable. Restore it before importing.",
                    ));
                }
                tx.execute(
                    "INSERT INTO native_notebooks(workspace_id,id,payload_json) VALUES (?,?,?)",
                    params![scope, id, value.to_string()],
                )?;
            }
        }
        for (layer, keys) in [("L2", SURFACES.as_slice()), ("L3", SLOTS.as_slice())] {
            for key in keys {
                let path = root.join("memory").join(layer).join(format!("{key}.md"));
                safe_path(&root, &path)?;
                if path.is_file() {
                    if std::fs::metadata(&path)?.len() > 512 * 1024 {
                        return Err(Error::conflict("Legacy memory exceeds the import limit."));
                    }
                    let content = std::fs::read_to_string(&path)?;
                    tx.execute("INSERT OR IGNORE INTO native_memory_docs(workspace_id,layer,key,content,updated_at) VALUES (?,?,?,?,?)",params![scope,layer,key,content,now()])?;
                }
            }
        }
    }
    tx.execute(
        "INSERT OR IGNORE INTO native_learning_imports(workspace_id) VALUES (?)",
        [scope],
    )?;
    tx.commit()?;
    flags(&conn, scope)
}
fn flags(conn: &Connection, scope: &str) -> Result<ToolFlags> {
    let notebooks = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_notebooks WHERE workspace_id=? AND deleted=0)",
        [scope],
        |r| r.get(0),
    )?;
    let memory = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_memory_docs WHERE workspace_id=?)",
        [scope],
        |r| r.get(0),
    )?;
    Ok(ToolFlags {
        allowed_tools: None,
        notebooks,
        memory,
        video_lesson: false,
        videogen: None,
        knowledge_bases: Vec::new(),
        search: None,
        browser: None,
        study_target: None,
        proposal: None,
    })
}

pub fn available(flags: &ToolFlags, name: &str) -> bool {
    (flags.notebooks && matches!(name, "list_favorites" | "save_favorite"))
        || (flags.memory && matches!(name, "read_memory" | "write_memory"))
}
pub fn definitions(flags: &ToolFlags) -> Vec<Value> {
    let mut tools = Vec::new();
    if flags.browser.is_some() {
        tools.push(crate::browser_testing::definition());
    }
    if flags.notebooks {
        tools.push(json!({"name":"list_favorites","description":"Discover favorites collections or saved record titles, summaries and IDs. Returns a bounded page, never full original bodies. Use offset and limit for subsequent pages. Only the current workspace is accessible.","parameters":{"type":"object","properties":{"notebook_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20}},"additionalProperties":false}}));
        tools.push(json!({"name":"save_favorite","description":"Save an original conversation to an existing favorites collection or edit a saved title/summary. Omitted, null or empty content saves the real conversation transcript. Original saved bodies cannot be edited.","parameters":{"type":"object","properties":{"mode":{"type":"string","enum":["append","edit"]},"notebook_id":{"type":"string"},"record_id":{"type":"string"},"title":{"type":"string"},"content":{"type":"string"},"turns_to_include":{"type":"string"},"note":{"type":"string"}},"required":["mode","notebook_id"],"additionalProperties":false}}));
    }
    if flags.memory {
        tools.push(json!({"name":"read_memory","description":"Read persistent profile, scope, preferences and recent learning memory for the current workspace.","parameters":{"type":"object","properties":{},"additionalProperties":false}}));
        tools.push(json!({"name":"write_memory","description":"Save ONLY a preference explicitly stated by the user. Never speculate. Use add or edit with an existing target_id.","parameters":{"type":"object","properties":{"op":{"type":"string","enum":["add","edit"]},"text":{"type":"string"},"target_id":{"type":"string"},"reason":{"type":"string"}},"required":["op","text"],"additionalProperties":false}}));
    }
    if flags.search.is_some() {
        tools.push(crate::search::definition());
    }
    if !flags.knowledge_bases.is_empty() {
        tools.push(crate::knowledge::definition(&flags.knowledge_bases));
    }
    tools
}
pub fn execute(
    store: &Store,
    admission: &crate::store::Admission,
    call: &crate::tools::ToolCall,
) -> Result<crate::tools::ToolOutcome> {
    let mut args: Value = serde_json::from_str(&call.arguments)
        .map_err(|_| Error::invalid("Expected JSON arguments."))?;
    let object = args
        .as_object_mut()
        .ok_or_else(|| Error::invalid("Expected an object."))?;
    let allowed: &[&str] = match call.name.as_str() {
        "list_favorites" => &["notebook_id", "offset", "limit"],
        "read_memory" => &[],
        "save_favorite" => &[
            "mode",
            "notebook_id",
            "record_id",
            "title",
            "content",
            "turns_to_include",
            "note",
        ],
        "write_memory" => &["op", "text", "target_id", "reason"],
        _ => return Err(Error::missing()),
    };
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::invalid("Unexpected tool argument."));
    }
    // Providers may materialize unused optional schema properties as null.
    // Required fields still pass through their normal non-empty validation.
    for optional in [
        "record_id",
        "content",
        "turns_to_include",
        "note",
        "target_id",
        "reason",
        "offset",
        "limit",
    ] {
        if object.get(optional).is_some_and(Value::is_null) {
            object.remove(optional);
        }
    }
    let scope = store.tool_session_info(admission)?;
    let scope = text(&scope, "workspace_id");
    let conn = store.connect()?;
    let mut outcome = crate::tools::ToolOutcome {
        result: Value::Null,
        mutation: None,
    };
    match call.name.as_str() {
        "list_favorites" => {
            outcome.result = json!({"success":true,"content":favorite_page(&conn,scope,&args)?});
        }
        "read_memory" => {
            let docs = SLOTS
                .iter()
                .map(|key| read_memory(&conn, scope, "L3", key))
                .collect::<Result<Vec<_>>>()?;
            outcome.result = json!({"success":true,"content":docs});
        }
        "save_favorite" => {
            let notebook = string(&args, "notebook_id", 256, true)?;
            match text(&args, "mode") {
                "append" => {
                    let title = string(&args, "title", 1000, true)?;
                    let note = string(&args, "note", 16384, false)?;
                    let content = string(&args, "content", 1024 * 1024, false)?;
                    let content = if !content.trim().is_empty() {
                        content
                    } else {
                        let mut branch =
                            store.branch(&admission.session, admission.user_message)?;
                        let count = text(&args, "turns_to_include");
                        if count != "all" {
                            let count = if count.is_empty() {
                                3
                            } else {
                                count
                                    .parse::<usize>()
                                    .ok()
                                    .filter(|n| *n > 0 && *n <= 100)
                                    .ok_or_else(|| Error::invalid("Invalid turns_to_include."))?
                            };
                            let skip = branch
                                .len()
                                .saturating_sub(count.saturating_mul(2).saturating_sub(1));
                            branch.drain(..skip);
                        }
                        let mut transcript = branch
                            .iter()
                            .map(|(_, m)| {
                                format!("## {}\n\n{}", text(m, "role"), text(m, "content"))
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        // The current turn has no assistant message yet. Its
                        // committed visible text and accepted Q&A live in the
                        // event log, including after an ask_user restart.
                        let events=conn.prepare("SELECT type,content,metadata_json FROM turn_events WHERE turn_id=? AND (type='content' OR (type IN ('wait_for_input','tool_result') AND json_extract(metadata_json,'$.tool_name')='ask_user')) ORDER BY seq")?.query_map([&admission.turn],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                        let mut assistant_text = false;
                        for (kind, content, metadata) in events {
                            let metadata = decode(metadata)?;
                            if kind == "content" {
                                if !assistant_text {
                                    transcript.push_str("\n\n## assistant\n\n");
                                }
                                transcript.push_str(&content);
                            } else if kind == "wait_for_input" {
                                transcript.push_str(&format!(
                                    "\n\n## assistant question\n\n{}",
                                    metadata["ask_user"]
                                ));
                            } else if metadata["success"] == true {
                                let reply = decode(content)?;
                                transcript.push_str(&format!(
                                    "\n\n## user reply\n\n{}",
                                    reply["content"]
                                ));
                            }
                            assistant_text = kind == "content";
                        }
                        if transcript.len() > 1024 * 1024 {
                            return Err(Error::invalid("Transcript is too large."));
                        }
                        transcript
                    };
                    outcome.mutation = Some(Mutation::SaveFavorite {
                        notebook,
                        title,
                        content,
                        note,
                    });
                }
                "edit" => {
                    if args.get("content").is_some() {
                        return Err(Error::invalid("Favorites preserve original content."));
                    }
                    let record = string(&args, "record_id", 256, true)?;
                    let mut patch = json!({});
                    if args.get("title").is_some() {
                        let title = string(&args, "title", 1000, false)?;
                        if !title.is_empty() {
                            patch["title"] = json!(title);
                        }
                    }
                    if args.get("note").is_some() {
                        patch["summary"] = json!(string(&args, "note", 16384, false)?);
                    }
                    outcome.mutation = Some(Mutation::EditFavorite {
                        notebook,
                        record,
                        patch,
                    });
                }
                _ => return Err(Error::invalid("mode must be append or edit.")),
            }
        }
        "write_memory" => {
            let content = string(&args, "text", 4096, true)?;
            if content.chars().count() > 240
                || content.contains(['\n', '\r'])
                || content.contains("<!--")
                || content.contains("[^")
            {
                return Err(Error::invalid(
                    "Preference must be a single line of at most 240 characters.",
                ));
            }
            let target = match text(&args, "op") {
                "add" => None,
                "edit" => {
                    let id = string(&args, "target_id", 128, true)?;
                    if !valid_memory_id(&id) {
                        return Err(Error::invalid("Invalid preference entry ID."));
                    }
                    Some(id)
                }
                _ => return Err(Error::invalid("op must be add or edit.")),
            };
            outcome.mutation = Some(Mutation::Preference { content, target });
        }
        _ => return Err(Error::missing()),
    }
    Ok(outcome)
}

fn memory_id(id: &str) -> String {
    let mut number = uuid::Uuid::parse_str(id)
        .unwrap_or_else(|_| uuid::Uuid::new_v4())
        .as_u128();
    let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = [b'0'; 26];
    for c in out.iter_mut().rev() {
        *c = alphabet[(number & 31) as usize];
        number >>= 5;
    }
    format!("m_{}", String::from_utf8_lossy(&out))
}

pub fn valid_memory_id(id: &str) -> bool {
    id.len() == 28
        && id.starts_with("m_")
        && id[2..]
            .bytes()
            .all(|c| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&c))
}

fn favorite_page(conn: &Connection, scope: &str, args: &Value) -> Result<Value> {
    let number = |key: &str, default: u64, max: u64| -> Result<usize> {
        Ok(args
            .get(key)
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n <= max)
                    .ok_or_else(|| Error::invalid(format!("Invalid {key}.")))
            })
            .transpose()?
            .unwrap_or(default) as usize)
    };
    let offset = number("offset", 0, 1_000_000)?;
    let limit = number("limit", 5, 20)?;
    if limit == 0 {
        return Err(Error::invalid("limit must be positive."));
    }
    let id = string(args, "notebook_id", 256, false)?;
    let clip = |value: &Value, key: &str, max: usize| {
        text(value, key).chars().take(max).collect::<String>()
    };
    let (key, rows) = if id.is_empty() {
        ("notebooks",list_notebooks(conn,scope)?.into_iter().map(|n|json!({"id":n["id"],"name":clip(&n,"name",100),"description":clip(&n,"description",120),"record_count":n["record_count"],"updated_at":n["updated_at"]})).collect::<Vec<_>>())
    } else {
        let nb = notebook(conn, scope, &id)?;
        let rows=nb["records"].as_array().ok_or_else(||Error::conflict("Invalid records."))?.iter().map(|r|json!({"id":r["id"],"record_id":r["id"],"title":clip(r,"title",100),"summary":clip(r,"summary",240),"type":r["type"],"created_at":r["created_at"]})).collect::<Vec<_>>();
        ("records", rows)
    };
    let total = rows.len();
    let page = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next = offset.saturating_add(page.len());
    let mut result = json!({"total":total,"offset":offset,"next_offset":if next<total{Some(next)}else{None},"summary_previews":true});
    result[key] = json!(page);
    Ok(result)
}
