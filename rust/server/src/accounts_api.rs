//! Authentication runs before choosing an application's data scope. Transport
//! query/body fields can never select another account's database or filesystem.
use crate::{
    Application, Error, Result, accounts, application::blocking, config::write_json, store::text,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, Request, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
type App = Arc<Application>;

#[derive(Clone)]
pub(crate) struct Principal {
    pub user: Value,
    pub token: String,
}
impl Principal {
    pub(crate) async fn valid(&self, app: &Application) -> bool {
        let accounts = app.accounts.clone();
        let token = self.token.clone();
        let expected = self.user.clone();
        blocking(move || {
            Ok(accounts
                .resolve(Some(&token))?
                .is_some_and(|u| u["id"] == expected["id"] && u["role"] == expected["role"]))
        })
        .await
        .unwrap_or(false)
    }
}
pub(crate) async fn session_valid(app: &Application, principal: &Option<Principal>) -> bool {
    match principal {
        Some(who) => who.valid(app).await,
        None => !app.accounts.is_enabled(),
    }
}
pub(crate) fn router(app: App) -> Router {
    Router::new()
        .route("/api/auth/status", get(status))
        .route("/api/auth/is_first_user", get(first))
        .route("/api/auth/setup", post(setup))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .fallback(gateway)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(app)
}
pub(crate) fn protected() -> Router<App> {
    Router::new()
        .route("/api/auth/users", get(users).post(create))
        .route("/api/auth/users/import", post(import))
        .route("/api/auth/users/batch-delete", post(batch_delete))
        .route("/api/auth/users/{username}", axum::routing::delete(delete))
        .route("/api/auth/users/{username}/role", axum::routing::put(role))
        .route(
            "/api/auth/users/{username}/learner-profile",
            get(learner).put(set_learner),
        )
        .route("/api/auth/profile", get(profile).put(update_profile))
        .route("/api/auth/profile/password", axum::routing::put(password))
        .route(
            "/api/auth/profile/models",
            get(personal_models).put(save_personal_models),
        )
        .route(
            "/api/auth/profile/learner-profile",
            get(own_learner).put(set_own_learner),
        )
        .merge(crate::guardian_api::router())
        .route("/api/multi-user/admin/resources", get(resources))
        .route(
            "/api/multi-user/users/{id}/grants",
            get(grants).put(save_grants),
        )
}
async fn identity(app: &App, headers: &HeaderMap) -> Result<Option<Principal>> {
    let accounts = app.accounts.clone();
    let token = accounts::token(headers).unwrap_or_default();
    let saved = token.clone();
    Ok(blocking(move || accounts.resolve(Some(&token)))
        .await?
        .map(|user| Principal { user, token: saved }))
}
async fn status(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>> {
    if !app.accounts.is_enabled() {
        return Ok(Json(
            json!({"enabled":false,"authenticated":true,"role":"admin","is_admin":true,"username":null,"user_id":null,"avatar":"","preset":null,"learning_policy":null,"native_accounts":true}),
        ));
    }
    let Some(principal) = identity(&app, &headers).await? else {
        return Ok(Json(
            json!({"enabled":true,"authenticated":false,"is_admin":false,"role":null}),
        ));
    };
    let mut user = principal.user;
    user["enabled"] = json!(true);
    user["authenticated"] = json!(true);
    user["native_accounts"] = json!(true);
    user["learning_policy"] = if user["role"] == "admin" {
        Value::Null
    } else {
        policy(&app.settings, &user)?
    };
    Ok(Json(user))
}
async fn first(State(app): State<App>) -> Result<Json<Value>> {
    Ok(Json(
        json!({"is_first_user":blocking(move||app.accounts.first()).await?}),
    ))
}
fn credentials(body: &Value) -> Result<(&str, &str)> {
    let name = body["username"]
        .as_str()
        .ok_or_else(|| Error::invalid("Username is required."))?;
    let password = body["password"]
        .as_str()
        .ok_or_else(|| Error::invalid("Password is required."))?;
    if name.len() > 512 || password.len() > 1024 {
        return Err(Error::invalid("Credentials exceed the size limit."));
    }
    Ok((name, password))
}
async fn sign_in(app: App, body: Value, setup: bool) -> Result<Response> {
    credentials(&body)?;
    let accounts = app.accounts.clone();
    let (user, token) = blocking(move || {
        let (name, password) = credentials(&body)?;
        accounts.login(name, password, setup)
    })
    .await?;
    let cookie = app.accounts.cookie(&token, false);
    Ok(([("set-cookie",cookie)],Json(json!({"ok":true,"username":user["username"],"role":user["role"],"must_change_password":user["must_change_password"]}))).into_response())
}
async fn setup(State(app): State<App>, Json(body): Json<Value>) -> Result<Response> {
    sign_in(app, body, true).await
}
async fn login(State(app): State<App>, Json(body): Json<Value>) -> Result<Response> {
    sign_in(app, body, false).await
}
async fn register(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    credentials(&body)?;
    let user = blocking(move || {
        let (name, password) = credentials(&body)?;
        app.accounts.create(name, password, "standard", true)
    })
    .await?;
    Ok(Json(
        json!({"ok":true,"role":user["role"],"is_first_user":user["role"]=="admin"}),
    ))
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Response> {
    let token = accounts::token(&headers);
    let accounts = app.accounts.clone();
    blocking(move || accounts.logout(token.as_deref())).await?;
    Ok((
        [("set-cookie", app.accounts.cookie("", true))],
        Json(json!({"ok":true})),
    )
        .into_response())
}
fn admin_only(path: &str, method: &Method) -> bool {
    let personal_setting = matches!(
        path,
        "/api/settings/ui"
            | "/api/settings/llm-options"
            | "/api/settings/voice-autoplay"
            | "/api/settings/voice-math-speak"
    ) || path.starts_with("/api/settings/workspace/registrations")
        || (method == Method::GET && path == "/api/settings/catalog");
    path.starts_with("/api/system/update")
        || path.starts_with("/api/auth/users")
        || (path.starts_with("/api/multi-user/") && !crate::guardian_api::delegated_path(path))
        || path.starts_with("/api/settings/providers/")
        || (path.starts_with("/api/settings/") && !personal_setting)
        || path.starts_with("/api/browser-testing")
        || path == "/api/voice/config"
        || path == "/api/native/metrics"
}
pub(crate) fn policy(settings: &crate::config::Settings, user: &Value) -> Result<Value> {
    let grant = accounts::grant(settings, text(user, "id"))?;
    if !grant["learning_policy"].is_null() {
        return Ok(grant["learning_policy"].clone());
    }
    if user["preset"] == "learner" {
        return Ok(
            json!({"age_band":"9-12","locked_persona":"teacher","allowed_capabilities":["chat","immersive_reading"],"default_capability":"chat","allowed_surfaces":["chat","reading"],"reading":{"allow_upload":false,"material_ids":[],"extensions":[]}}),
        );
    }
    Ok(Value::Null)
}
async fn gateway(State(app): State<App>, mut request: Request) -> Response {
    let result = async {
        let path = request.uri().path().to_owned();
        if matches!(
            path.as_str(),
            "/health" | "/health/ready" | "/health/live" | "/api/health"
        ) {
            return Ok(crate::api::workspace_router(app)
                .oneshot(request)
                .await
                .unwrap());
        }
        if !app.accounts.is_enabled() {
            if path.starts_with("/api/auth/") || path.starts_with("/api/multi-user/") {
                return Err(Error::invalid("Enable multi-user mode to manage accounts."));
            }
            return Ok(crate::api::workspace_router(app)
                .oneshot(request)
                .await
                .unwrap());
        }
        let principal = identity(&app, request.headers())
            .await?
            .ok_or_else(accounts::unauthorized)?;
        let account_shell = request.method() == Method::GET
            && matches!(
                path.as_str(),
                "/api/modules" | "/api/settings/ui" | "/api/native/status"
            );
        if principal.user["must_change_password"] == true
            && !path.starts_with("/api/auth/profile")
            && !account_shell
        {
            return Err(Error::new(
                StatusCode::FORBIDDEN,
                "password_change_required",
                "Change your temporary password in My profile before continuing.",
            ));
        }
        if principal.user["role"] != "admin" {
            if admin_only(&path, request.method()) {
                return Err(accounts::forbidden());
            }
            let policy = policy(&app.settings, &principal.user)?;
            if !policy.is_null() {
                let allowed = path.starts_with("/api/auth/profile")
                    || path == "/api/modules"
                    || path == "/api/native/status"
                    || path.starts_with("/api/multi-user/me/guardianships")
                    || path == "/api/settings"
                    || path == "/api/settings/ui"
                    || path == "/api/settings/llm-options"
                    || path == "/api/settings/catalog"
                    || path == "/api/capabilities/registered"
                    || path.starts_with("/api/settings/workspace/registrations")
                    || path.starts_with("/api/sessions")
                    || path.starts_with("/api/turns")
                    || path == "/ws"
                    || path.starts_with("/api/reading/");
                if !allowed {
                    return Err(Error::new(
                        StatusCode::FORBIDDEN,
                        "learning_policy",
                        "This surface is not enabled for your account.",
                    ));
                }
                let chat = policy["allowed_surfaces"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|s| s == "chat"));
                let reading = policy["allowed_surfaces"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|s| s == "reading"));
                if ((!chat)
                    && (path.starts_with("/api/sessions")
                        || path.starts_with("/api/turns")
                        || path == "/ws"))
                    || ((!reading) && path.starts_with("/api/reading/"))
                {
                    return Err(Error::new(
                        StatusCode::FORBIDDEN,
                        "learning_policy",
                        "This surface is not enabled for your account.",
                    ));
                }
                if let Some(suffix) = path
                    .strip_prefix("/api/reading/items/")
                    .filter(|_| policy["reading"]["assigned_materials"].is_array())
                {
                    let id = suffix.split('/').next().unwrap_or("");
                    if !policy["reading"]["material_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|value| value == id))
                    {
                        return Err(Error::missing());
                    }
                }
                if path == "/api/reading/items"
                    && request.method() == Method::POST
                    && policy["reading"]["allow_upload"] != true
                {
                    return Err(Error::new(
                        StatusCode::FORBIDDEN,
                        "learning_policy",
                        "Reading uploads are disabled for your account.",
                    ));
                }
            }
        }
        let scoped = app.for_account(&principal.user).await?;
        request.extensions_mut().insert(principal);
        // Account endpoints always use the deployment identity store; workspace
        // endpoints always use the server-selected account application.
        let target = if path.starts_with("/api/auth/") || path.starts_with("/api/multi-user/") {
            app
        } else {
            scoped
        };
        Ok(crate::api::workspace_router(target)
            .oneshot(request)
            .await
            .unwrap())
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn users(State(app): State<App>) -> Result<Json<Value>> {
    Ok(Json(json!(blocking(move || app.accounts.list()).await?)))
}
async fn create(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    credentials(&body)?;
    Ok(Json(
        blocking(move || {
            let (name, password) = credentials(&body)?;
            app.accounts.create(
                name,
                password,
                body["preset"].as_str().unwrap_or("standard"),
                false,
            )
        })
        .await?,
    ))
}
async fn delete(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<Value>> {
    let user = blocking(move || {
        app.accounts
            .change_role_or_delete(text(&who.user, "id"), &name, None)
    })
    .await?;
    Ok(Json(json!({"ok":true,"username":user["username"]})))
}
async fn role(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    Ok(Json(
        blocking(move || {
            app.accounts.change_role_or_delete(
                text(&who.user, "id"),
                &name,
                Some(text(&body, "role")),
            )
        })
        .await?,
    ))
}
async fn batch_delete(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let names = body["usernames"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 500)
        .ok_or_else(|| Error::invalid("Select 1-500 usernames."))?
        .clone();
    Ok(Json(blocking(move||{
        let mut results=vec![];let mut count=0;
        for name in names {let name=name.as_str().ok_or_else(||Error::invalid("Invalid username."))?;match app.accounts.change_role_or_delete(text(&who.user,"id"),name,None){Ok(_)=>{count+=1;results.push(json!({"username":name,"ok":true}));},Err(e)=>results.push(json!({"username":name,"ok":false,"error":e.message}))}}
        Ok(json!({"ok":count==results.len(),"deleted_count":count,"failed_count":results.len()-count,"results":results}))
    }).await?))
}
async fn import(State(app): State<App>, mut form: Multipart) -> Result<Json<Value>> {
    let field = form
        .next_field()
        .await
        .map_err(|_| Error::invalid("Invalid CSV upload."))?
        .ok_or_else(|| Error::invalid("Select a CSV file."))?;
    if field.name() != Some("file") {
        return Err(Error::invalid("Select a CSV file."));
    }
    let bytes = field
        .bytes()
        .await
        .map_err(|_| Error::invalid("CSV exceeds the 2 MiB limit."))?;
    Ok(Json(blocking(move||{
        let bytes=bytes.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(&bytes);
        let mut reader=csv::ReaderBuilder::new().trim(csv::Trim::All).from_reader(bytes);
        let headers=reader.headers().map_err(|_|Error::invalid("Invalid CSV header."))?.clone();
        if headers.iter().collect::<Vec<_>>()!=["username","password","preset"]{return Err(Error::invalid("Header must be username,password,preset."));}
        let rows=reader.records().collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::invalid("Invalid CSV rows."))?;
        if rows.is_empty() || rows.len()>500 {return Err(Error::invalid("CSV must contain 1-500 accounts."));}
        let mut results=vec![];let mut count=0;
        for (index,row) in rows.iter().enumerate(){match app.accounts.create(&row[0],&row[1],&row[2],false){Ok(user)=>{count+=1;results.push(json!({"row":index+2,"username":row[0],"ok":true,"user":user}));},Err(e)=>results.push(json!({"row":index+2,"username":row[0],"ok":false,"error":e.message}))}}
        Ok(json!({"ok":count==results.len(),"created_count":count,"failed_count":results.len()-count,"results":results}))
    }).await?))
}
async fn profile(Extension(who): Extension<Principal>) -> Json<Value> {
    Json(who.user)
}
async fn personal_models(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
) -> Result<Json<Value>> {
    let app = app.for_account(&who.user).await?;
    Ok(Json(
        blocking(move || {
            Ok(json!({"catalog":crate::config::redact(app.settings.personal_catalog()?)}))
        })
        .await?,
    ))
}
async fn save_personal_models(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let app = app.for_account(&who.user).await?;
    Ok(Json(
        blocking(move || {
            Ok(json!({"catalog":app.settings.save_catalog(body["catalog"].clone())?}))
        })
        .await?,
    ))
}
async fn update_profile(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let avatar = body["avatar"]
        .as_str()
        .ok_or_else(|| Error::invalid("Avatar is required."))?;
    if !avatar.is_empty()
        && (!avatar.starts_with("icon:")
            || avatar.len() > 128
            || avatar.chars().any(char::is_control))
    {
        return Err(Error::invalid("Invalid avatar marker."));
    }
    Ok(Json(
        blocking(move || {
            app.accounts
                .update_profile(text(&who.user, "id"), "avatar", body["avatar"].clone())
        })
        .await?,
    ))
}
async fn password(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Json(body): Json<Value>,
) -> Result<Response> {
    let accounts = app.accounts.clone();
    let token = blocking(move || {
        let current = body["current_password"]
            .as_str()
            .ok_or_else(|| Error::invalid("Current password is required."))?;
        let new = body["new_password"]
            .as_str()
            .ok_or_else(|| Error::invalid("New password is required."))?;
        accounts.password(text(&who.user, "id"), current, new)
    })
    .await?;
    Ok((
        [("set-cookie", app.accounts.cookie(&token, false))],
        Json(json!({"ok":true})),
    )
        .into_response())
}
fn normalize_learner(body: Value) -> Result<Value> {
    let map = body
        .as_object()
        .ok_or_else(|| Error::invalid("Expected a learner profile."))?;
    for (key, value) in map {
        match key.as_str() {
            "age" if value.as_u64().is_some_and(|n| n <= 120) => {}
            "grade_level" | "curriculum" | "language" | "reading_level" | "explanation_style"
                if value.as_str().is_some_and(|s| s.len() <= 2000) => {}
            _ => return Err(Error::invalid("Invalid learner profile field.")),
        }
    }
    Ok(body)
}
async fn learner(State(app): State<App>, Path(name): Path<String>) -> Result<Json<Value>> {
    let users = blocking(move || app.accounts.list()).await?;
    let user = users
        .into_iter()
        .find(|u| u["username"] == name)
        .ok_or_else(Error::missing)?;
    Ok(Json(json!({"learner_profile":user["learner_profile"]})))
}
async fn set_learner(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let body = normalize_learner(body)?;
    Ok(Json(
        blocking(move || {
            let user = app
                .accounts
                .list()?
                .into_iter()
                .find(|u| u["username"] == name)
                .ok_or_else(Error::missing)?;
            let user = app
                .accounts
                .update_profile(text(&user, "id"), "learner_profile", body)?;
            Ok(json!({"learner_profile":user["learner_profile"]}))
        })
        .await?,
    ))
}
async fn own_learner(Extension(who): Extension<Principal>) -> Json<Value> {
    Json(json!({"learner_profile":who.user["learner_profile"]}))
}
async fn set_own_learner(
    State(app): State<App>,
    Extension(who): Extension<Principal>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let body = normalize_learner(body)?;
    Ok(Json(
        blocking(move || {
            let user =
                app.accounts
                    .update_profile(text(&who.user, "id"), "learner_profile", body)?;
            Ok(json!({"learner_profile":user["learner_profile"]}))
        })
        .await?,
    ))
}
async fn resources(State(app): State<App>) -> Result<Json<Value>> {
    Ok(Json(blocking(move||{
        let catalog=app.settings.catalog()?;
        let models=catalog["services"]["llm"]["profiles"].as_array().into_iter().flatten().map(|p|json!({"profile_id":p["id"],"name":p["name"],"models":p["models"].as_array().into_iter().flatten().map(|m|json!({"model_id":m["id"],"name":m["name"],"model":m["model"]})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        Ok(json!({"models":{"llm":models},"knowledge_bases":[],"skills":[],"partners":[],"reading_materials":[],"reading_extensions":[],"tools":[{"name":"list_favorites"},{"name":"save_favorite"},{"name":"read_memory"},{"name":"write_memory"},{"name":"rag"},{"name":"videogen"},{"name":"render_video_lesson"}],"mcp_tools":[],"native":true}))
    }).await?))
}
async fn grants(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(
        blocking(move || {
            let user = app.accounts.user(&id)?;
            let mut grant = accounts::grant(&app.settings, &id)?;
            if user["preset"] == "learner" && grant["learning_policy"].is_null() {
                grant["learning_policy"] = policy(&app.settings, &user)?;
            }
            Ok(json!({"grant":grant}))
        })
        .await?,
    ))
}
async fn save_grants(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let body = body.get("grant").cloned().unwrap_or(body);
    Ok(Json(blocking(move||{
        let user=app.accounts.user(&id)?;
        if user["role"]=="admin" {return Err(Error::invalid("Administrators manage deployment resources directly."));}
        if body["user_id"]!=id || body["version"]!=2 || !body["models"]["llm"].is_array(){return Err(Error::invalid("Invalid account grant."));}
        for key in ["knowledge_bases","skills","partners","mcp_tools"]{if body[key].as_array().is_some_and(|v|!v.is_empty()){return Err(Error::unsupported("Sharing this resource has not been migrated. Account data remains private."));}}
        if body["exec_enabled"]==true {return Err(Error::invalid("Host execution is disabled for ordinary accounts."));}
        if !body["enabled_tools"].is_null() && body["enabled_tools"].as_array().is_none_or(|a|a.iter().any(|v| !["list_favorites","save_favorite","read_memory","write_memory","rag","videogen","render_video_lesson"].iter().any(|n|v==n))) {return Err(Error::invalid("Invalid tool assignment."));}
        let catalog=app.settings.catalog()?;
        for grant in body["models"]["llm"].as_array().unwrap(){
            let profile=catalog["services"]["llm"]["profiles"].as_array().into_iter().flatten().find(|p|p["id"]==grant["profile_id"]).ok_or_else(||Error::invalid("Unknown model profile."))?;
            let ids=grant["model_ids"].as_array().ok_or_else(||Error::invalid("Select model ids explicitly."))?;
            if ids.iter().any(|id|!profile["models"].as_array().into_iter().flatten().any(|m|&m["id"]==id)){return Err(Error::invalid("Unknown model id."));}
        }
        if !body["learning_policy"].is_null() {validate_policy(&body["learning_policy"])?;}
        if user["preset"] == "learner" && body["learning_policy"].is_null() { return Err(Error::invalid("Learner accounts require a learning policy.")); }
        write_json(&accounts::grant_path(&app.settings,&id)?,&body)?;
        Ok(json!({"grant":body}))
    }).await?))
}
fn validate_policy(value: &Value) -> Result<()> {
    if !["6-8", "9-12", "13-15"].contains(&text(value, "age_band"))
        || value["locked_persona"] != "teacher"
        || value["allowed_capabilities"].as_array().is_none_or(|a| {
            a.is_empty() || a.iter().any(|v| v != "chat" && v != "immersive_reading")
        })
        || value["allowed_surfaces"]
            .as_array()
            .is_none_or(|a| a.is_empty() || a.iter().any(|v| v != "chat" && v != "reading"))
    {
        return Err(Error::invalid("Invalid learning policy."));
    }
    Ok(())
}
