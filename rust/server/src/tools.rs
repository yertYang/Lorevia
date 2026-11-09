//! Native tool surface. Reads and pending writes are scoped by the admitted turn, never
//! by model-supplied session or workspace identifiers.
use crate::{
    Error, Result,
    application::blocking,
    capability::Capability,
    store::{Admission, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_CALLS: usize = 16;
pub const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
impl ToolCall {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 256
            || self.name.is_empty()
            || self.name.len() > 128
            || self.arguments.len() > MAX_ARGUMENT_BYTES
        {
            return Err(Error::provider("Invalid or oversized tool call."));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ToolOutcome {
    pub result: Value,
    pub mutation: Option<crate::learning::Mutation>,
}
impl From<Value> for ToolOutcome {
    fn from(result: Value) -> Self {
        Self {
            result,
            mutation: None,
        }
    }
}

pub fn definitions(capability: Capability) -> Vec<Value> {
    definitions_scoped(capability, &Default::default())
}
pub fn definitions_scoped(
    capability: Capability,
    flags: &crate::learning::ToolFlags,
) -> Vec<Value> {
    let mut tools = vec![
        json!({"name":"session_info","description":"Read the current conversation title, reply language and workspace identifier. Takes no arguments and cannot access other conversations.","parameters":{"type":"object","properties":{},"additionalProperties":false,"required":[]}}),
        crate::ask_user::definition(),
    ];
    if capability == Capability::VideoLesson
        || (capability == Capability::Chat && flags.video_lesson)
    {
        tools.push(crate::video::lesson_definition());
    }
    if capability == Capability::Chat && flags.videogen.is_some() {
        tools.push(crate::video::video_definition());
    }
    if capability == Capability::DeepSolve {
        tools.extend(crate::solve::definitions());
    }
    if capability.is_study() {
        tools.extend(crate::study::definitions(
            capability == Capability::DeepResearch,
        ));
    }
    tools.extend(
        crate::learning::definitions(flags)
            .into_iter()
            .filter(|tool| {
                flags
                    .allowed_tools
                    .as_ref()
                    .is_none_or(|names| names.iter().any(|name| tool["name"] == name.as_str()))
            }),
    );
    tools
}

pub struct ToolRegistry {
    pub settings: std::sync::Arc<crate::config::Settings>,
    pub store: Store,
    pub admission: Admission,
}
impl ToolRegistry {
    pub async fn execute(
        &self,
        call: &ToolCall,
        invocation: &str,
        solve: &mut crate::solve::SolveState,
        flags: &crate::learning::ToolFlags,
    ) -> ToolOutcome {
        if let (Some(root), Some(id)) = (&self.settings.shared, &self.settings.account_id) {
            let allowed = (|| -> Result<bool> {
                let grant = crate::accounts::grant(root, id)?;
                if call.name == "browser_test" {
                    return Ok(false);
                }
                let optional = [
                    "list_favorites",
                    "save_favorite",
                    "read_memory",
                    "write_memory",
                    "rag",
                    "web_search",
                    "videogen",
                    "render_video_lesson",
                ];
                if optional.contains(&call.name.as_str())
                    && let Some(names) = grant["enabled_tools"].as_array()
                {
                    return Ok(names.iter().any(|v| v == call.name.as_str()));
                }
                Ok(true)
            })();
            if !matches!(allowed, Ok(true)) {
                return json!({"success":false,"error":"forbidden","content":"This tool is not assigned to your account."}).into();
            }
        }
        if (call.name == "render_video_lesson"
            && flags.video_lesson
            && matches!(
                self.admission.capability,
                Capability::Chat | Capability::VideoLesson
            ))
            || (call.name == "videogen"
                && flags.videogen.is_some()
                && self.admission.capability == Capability::Chat)
        {
            return crate::video::execute(
                self.settings.clone(),
                &self.admission,
                invocation,
                call,
                flags,
            )
            .await;
        }
        if self.admission.capability.is_study()
            && matches!(
                call.name.as_str(),
                "study_plan" | "study_evidence" | "study_publish"
            )
        {
            let input = match serde_json::from_str::<Value>(&call.arguments) {
                Ok(value) if value.is_object() => value,
                _ => return json!({"success":false,"error":"invalid_arguments","content":"Study tools require a JSON object."}).into(),
            };
            return ToolOutcome {
                result: Value::Null,
                mutation: Some(crate::learning::Mutation::Study(crate::study::Mutation {
                    turn: self.admission.turn.clone(),
                    kind: self.admission.capability.id().into(),
                    operation: call.name.clone(),
                    input,
                    knowledge: flags.knowledge_bases.clone(),
                    web: flags.search.is_some(),
                    target: flags.study_target.clone(),
                    proposal: flags.proposal.clone(),
                })),
            };
        }
        if call.name == "browser_test"
            && let Some(selection) = &flags.browser
        {
            return match crate::browser_testing::execute(
                self.store.clone(),
                self.admission.clone(),
                selection.clone(),
                &call.arguments,
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    json!({"success":false,"error":error.code,"content":error.message}).into()
                }
            };
        }
        if self.admission.capability == Capability::DeepSolve
            && matches!(
                call.name.as_str(),
                "solve_plan" | "solve_finish_step" | "solve_replan"
            )
        {
            return solve.execute(call).into();
        }
        if call.name == "web_search"
            && let Some(selection) = &flags.search
        {
            return match crate::search::execute(self.settings.clone(), selection, &call.arguments)
                .await
            {
                Ok(value) => value,
                Err(error) => json!({"success":false,"error":error.code,"content":error.message}),
            }
            .into();
        }
        if call.name == "rag" && !flags.knowledge_bases.is_empty() {
            let allowed = flags.knowledge_bases.clone();
            let store = self.store.clone();
            let admission = self.admission.clone();
            let call = call.clone();
            let result = blocking(move || {
                let args: Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| Error::invalid("Invalid JSON arguments."))?;
                if args.as_object().is_none_or(|m| {
                    m.keys().any(|k| {
                        !["query", "kb_name", "source_path", "top_k"].contains(&k.as_str())
                    })
                }) {
                    return Err(Error::invalid("Invalid retrieval arguments."));
                }
                let kb = crate::store::text(&args, "kb_name");
                if !allowed.iter().any(|selected| selected == kb) {
                    return Err(Error::missing());
                }
                let session = store.tool_session_info(&admission)?;
                crate::knowledge::search(
                    &store.connect()?,
                    crate::store::text(&session, "workspace_id"),
                    kb,
                    &args,
                )
            })
            .await;
            return match result {
                Ok(value) => json!({"success":true,"content":value}),
                Err(error) => json!({"success":false,"error":error.code,"content":error.message}),
            }
            .into();
        }
        if crate::learning::available(flags, &call.name) {
            let store = self.store.clone();
            let admission = self.admission.clone();
            let call = call.clone();
            return match blocking(move || crate::learning::execute(&store, &admission, &call)).await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    json!({"success":false,"error":error.code,"content":error.message}).into()
                }
            };
        }
        if call.name != "session_info" {
            return json!({"success":false,"error":"tool_unavailable","content":"This tool is not available in this turn."}).into();
        }
        let args = serde_json::from_str::<Value>(&call.arguments);
        if !args.is_ok_and(|v| v.as_object().is_some_and(|m| m.is_empty())) {
            return json!({"success":false,"error":"invalid_arguments","content":"session_info expects an empty JSON object."}).into();
        }
        let store = self.store.clone();
        let admission = self.admission.clone();
        let result = match blocking(move || store.tool_session_info(&admission)).await {
            Ok(value) => json!({"success":true,"content":value}),
            Err(_) => {
                json!({"success":false,"error":"session_unavailable","content":"The current conversation could not be read."})
            }
        };
        result.into()
    }
}
