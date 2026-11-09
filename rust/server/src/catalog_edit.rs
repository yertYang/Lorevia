//! Scoped catalog edits. Applying one editor must not promote sibling drafts.
use crate::{Error, Result, config::restore_catalog, store::text};
use serde_json::{Value, json};

fn source<'a>(catalog: &'a Value, reference: &Value) -> Result<&'a Value> {
    let connection = text(reference, "connection_id");
    let items = if !connection.is_empty() {
        &catalog["connections"]
    } else {
        &catalog["services"][text(reference, "service")]["profiles"]
    };
    let id = if connection.is_empty() {
        text(reference, "profile_id")
    } else {
        connection
    };
    items
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["id"] == id)
        .ok_or_else(|| Error::invalid("Choose an existing provider."))
}
fn upsert(items: &mut Value, proposed: Value) -> Result<()> {
    if !proposed.is_object() || text(&proposed, "id").is_empty() {
        return Err(Error::invalid("A configuration ID is required."));
    }
    let items = items
        .as_array_mut()
        .ok_or_else(|| Error::invalid("Invalid catalog array."))?;
    if let Some(item) = items.iter_mut().find(|item| item["id"] == proposed["id"]) {
        *item = proposed;
    } else {
        items.push(proposed);
    }
    Ok(())
}
fn referenced(value: &Value, reference: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(|item| referenced(item, reference)),
        Value::Object(map) => {
            if let Some(found) = map.get("provider_ref") {
                let connection = text(reference, "connection_id");
                if !connection.is_empty() && found["connection_id"] == connection {
                    return true;
                }
                if connection.is_empty()
                    && found["service"] == reference["service"]
                    && found["profile_id"] == reference["profile_id"]
                {
                    return true;
                }
            }
            map.values().any(|item| referenced(item, reference))
        }
        _ => false,
    }
}
fn uses_model(value: &Value, profile: &str, model: &str) -> bool {
    match value {
        Value::Object(map) => {
            (value["profile_id"] == profile && value["model_id"] == model)
                || map.values().any(|v| uses_model(v, profile, model))
        }
        Value::Array(items) => items.iter().any(|v| uses_model(v, profile, model)),
        _ => false,
    }
}
fn managed(value: &Value) -> bool {
    value["owner_bound"] == true
        || !text(value, "managed_by").is_empty()
        || value["read_only"] == true
}
fn model_service(edit: &Value) -> Result<&str> {
    let service = text(edit, "service");
    if !matches!(service, "llm" | "videogen") {
        return Err(Error::unsupported(
            "Scoped settings for this service have not yet been migrated.",
        ));
    }
    Ok(service)
}
pub fn registry(current: &Value, edit: &Value, draft: &Value) -> Result<Value> {
    if !edit.is_object() {
        return Err(Error::invalid("Expected registry edit object."));
    }
    let mut result = current.clone();
    let deleting = edit["delete"] == true;
    match text(edit, "kind") {
        "provider" => {
            let reference = &edit["ref"];
            let mut provider = match source(&result, reference) {
                Ok(value) => value.clone(),
                Err(_)
                    if !deleting
                        && !text(reference, "connection_id").is_empty()
                        && !text(&edit["fields"], "provider").is_empty() =>
                {
                    json!({"id":reference["connection_id"]})
                }
                Err(error) => return Err(error),
            };
            if managed(&provider) {
                return Err(Error::invalid(
                    "Manage this provider through its authentication settings.",
                ));
            }
            let connection = !text(reference, "connection_id").is_empty();
            if deleting {
                if referenced(&result["services"], reference)
                    || provider["models"]
                        .as_array()
                        .is_some_and(|models| !models.is_empty())
                    || (connection
                        && result["services"]
                            .as_object()
                            .into_iter()
                            .flat_map(|s| s.values())
                            .flat_map(|b| b["profiles"].as_array().into_iter().flatten())
                            .any(|p| p["connection_id"] == reference["connection_id"]))
                {
                    return Err(Error::invalid(
                        "Remove or reassign this provider's models first.",
                    ));
                }
            } else {
                let fields = edit["fields"]
                    .as_object()
                    .ok_or_else(|| Error::invalid("Expected provider fields."))?;
                for key in [
                    "name",
                    "provider",
                    "binding",
                    "base_url",
                    "api_format",
                    "wire_api",
                    "api_key",
                    "api_version",
                    "extra_headers",
                    "proxy",
                    "app_id",
                    "discovery",
                    "source_service",
                    "service_overrides",
                    "user_name",
                ] {
                    if let Some(value) = fields.get(key) {
                        provider[key] = value.clone();
                    }
                }
                if text(&provider, "name").trim().is_empty() {
                    return Err(Error::invalid("A provider name is required."));
                }
            }
            let items = if connection {
                &mut result["connections"]
            } else {
                &mut result["services"][text(reference, "service")]["profiles"]
            };
            if deleting {
                items
                    .as_array_mut()
                    .ok_or_else(|| Error::invalid("Invalid provider collection."))?
                    .retain(|item| item["id"] != provider["id"]);
            } else {
                upsert(items, provider)?;
            }
        }
        "model" | "default" => {
            let service = model_service(edit)?;
            let profile_id = text(edit, "profile_id");
            if profile_id.is_empty() {
                return Err(Error::invalid("A model profile ID is required."));
            }
            let mut model = edit.get("model").cloned().unwrap_or_else(|| json!({}));
            if !model.is_object() {
                return Err(Error::invalid("Expected model object."));
            }
            let reference = model.get("provider_ref");
            let owner = reference.map(|r| source(&result, r).cloned()).transpose()?;
            let mut profile = result["services"][service]["profiles"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["id"] == profile_id)
                .cloned();
            if profile.is_none() {
                if deleting || edit["kind"] == "default" {
                    return Err(Error::invalid("Model profile does not exist."));
                }
                let owner = owner
                    .as_ref()
                    .ok_or_else(|| Error::invalid("Choose a provider before creating a model."))?;
                if managed(owner) {
                    return Err(Error::invalid("This account manages its own model list."));
                }
                let binding = reference
                    .and_then(|r| r["binding"].as_str())
                    .or_else(|| owner["binding"].as_str())
                    .or_else(|| owner["provider"].as_str())
                    .unwrap_or("custom");
                profile = Some(
                    json!({"id":profile_id,"name":owner["name"],"binding":binding,"provider_ref":reference,"models":[]}),
                );
            }
            let mut profile = profile.unwrap();
            if owner
                .as_ref()
                .is_some_and(|p| managed(p) && p["id"] != profile_id)
            {
                return Err(Error::invalid("Account-bound models cannot be reassigned."));
            }
            let model_id = edit["model_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(text(&model, "id"))
                .to_owned();
            if model_id.is_empty() {
                return Err(Error::invalid("A model ID is required."));
            }
            if edit["kind"] == "model" && managed(&profile) {
                return Err(Error::invalid(
                    "Refresh this account's catalog to change its models.",
                ));
            }
            let inherited = profile["provider_ref"].is_object();
            let models = profile["models"]
                .as_array_mut()
                .ok_or_else(|| Error::invalid("Invalid model collection."))?;
            let existing = models.iter().position(|m| m["id"] == model_id);
            if edit["kind"] == "default" {
                if existing.is_none() {
                    return Err(Error::invalid("The selected model no longer exists."));
                }
            } else if deleting {
                let task = &result["services"]["task"];
                if service == "llm" && uses_model(task, profile_id, &model_id) {
                    return Err(Error::invalid(
                        "Reassign background tasks before deleting this model.",
                    ));
                }
                models.retain(|m| m["id"] != model_id);
            } else {
                if text(&model, "model").trim().is_empty() {
                    return Err(Error::invalid("A model identifier is required."));
                }
                if existing.is_none() && reference.is_none() && !inherited {
                    return Err(Error::invalid("Choose a provider before creating a model."));
                }
                for key in ["api_key", "base_url", "extra_headers", "api_version"] {
                    model.as_object_mut().unwrap().remove(key);
                }
                let mut merged = existing
                    .map(|i| models[i].clone())
                    .unwrap_or_else(|| json!({}));
                if model.get("context_window").is_some()
                    && model["context_window"] != merged["context_window"]
                {
                    merged
                        .as_object_mut()
                        .unwrap()
                        .remove("context_window_tokens");
                }
                merged
                    .as_object_mut()
                    .unwrap()
                    .extend(model.as_object().unwrap().clone());
                merged["id"] = json!(model_id);
                if let Some(index) = existing {
                    models[index] = merged;
                } else {
                    models.push(merged);
                }
            }
            let empty = models.is_empty();
            let bucket = &mut result["services"][service];
            if deleting && empty && inherited {
                bucket["profiles"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|p| p["id"] != profile_id);
            } else {
                upsert(&mut bucket["profiles"], profile)?;
            }
            if edit["kind"] == "default"
                || (!deleting && text(bucket, "active_profile_id").is_empty())
            {
                bucket["active_profile_id"] = json!(profile_id);
                bucket["active_model_id"] = json!(model_id);
            }
            if deleting {
                select_remaining(bucket);
            }
        }
        _ => {
            return Err(Error::unsupported(
                "This registry operation has not yet been migrated.",
            ));
        }
    }
    let result = if draft.is_object() {
        restore_catalog(result, draft)?
    } else {
        result
    };
    restore_catalog(result, current)
}
fn select_remaining(bucket: &mut Value) {
    let candidates = bucket["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| {
            p["provider_only"] != true && p["models"].as_array().is_some_and(|m| !m.is_empty())
        })
        .collect::<Vec<_>>();
    let selected = candidates
        .iter()
        .find(|p| p["id"] == bucket["active_profile_id"])
        .copied()
        .or_else(|| candidates.first().copied())
        .cloned();
    if let Some(profile) = selected {
        bucket["active_profile_id"] = profile["id"].clone();
        if !profile["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == bucket["active_model_id"])
        {
            bucket["active_model_id"] = profile["models"][0]["id"].clone();
        }
    } else {
        bucket["active_profile_id"] = Value::Null;
        bucket["active_model_id"] = Value::Null;
    }
}
