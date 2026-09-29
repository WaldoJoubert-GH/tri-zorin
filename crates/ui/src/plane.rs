//! Plane.so work items for the selected project — the data half of the Plane
//! right-pane surface ([`crate::plane_view`]) and the composer's in-progress
//! pills.
//!
//! Ported from pi-todos' `plane.ts`: the same REST endpoints (`/states/` plus
//! `/issues/` per project, `PATCH /issues/{id}/` to move state, `X-API-Key`
//! auth), the same five-minute background sync, the same priority →
//! state-group → title ordering, and the same "In Progress" state-name test
//! for the pills. Unlike pi-todos (one project per repo via
//! `.dev/config.json`), each Zeron project (space) links to its own Plane
//! workspace + project through [`PlaneSettings::projects`].
//!
//! [`PlaneSettings::projects`]: crate::settings::PlaneSettings

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Context, Hsla, SharedString, Task};
use serde::Deserialize;

use crate::settings::{self, PlaneProjectLink};
use crate::state::AppState;

const API_BASE: &str = "https://api.plane.so/api/v1";
const APP_BASE: &str = "https://app.plane.so";
/// pi-todos `SYNC_INTERVAL_MS`.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const PAGE_SIZE: usize = 100;
/// Hard stop for runaway pagination (2 000 work items per project).
const MAX_PAGES: usize = 20;
/// The state name pi-todos pins as "in progress" (`widget.ts`).
const IN_PROGRESS_STATE: &str = "In Progress";

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Plane's fixed state groups. Every custom state belongs to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateGroup {
    Started,
    Unstarted,
    Backlog,
    Triage,
    Completed,
    Cancelled,
    Unknown,
}

impl StateGroup {
    /// Display order in the Plane surface: what is being worked on first.
    pub const DISPLAY_ORDER: [StateGroup; 7] = [
        StateGroup::Started,
        StateGroup::Unstarted,
        StateGroup::Backlog,
        StateGroup::Triage,
        StateGroup::Completed,
        StateGroup::Cancelled,
        StateGroup::Unknown,
    ];

    fn parse(raw: &str) -> Self {
        match raw {
            "started" => Self::Started,
            "unstarted" => Self::Unstarted,
            "backlog" => Self::Backlog,
            "triage" => Self::Triage,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }

    /// Completed and cancelled work is hidden unless asked for (pi-todos
    /// `ACTIVE_GROUPS`).
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Completed | Self::Cancelled)
    }

    pub fn sort_key(self) -> usize {
        Self::DISPLAY_ORDER
            .iter()
            .position(|g| *g == self)
            .unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Urgent,
    High,
    Medium,
    Low,
    None,
}

impl Priority {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("urgent") => Self::Urgent,
            Some("high") => Self::High,
            Some("medium") => Self::Medium,
            Some("low") => Self::Low,
            _ => Self::None,
        }
    }

    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Urgent => Some("Urgent"),
            Self::High => Some("High"),
            Self::Medium => Some("Medium"),
            Self::Low => Some("Low"),
            Self::None => None,
        }
    }
}

/// One of a project's workflow states.
#[derive(Debug, Clone)]
pub struct PlaneState {
    pub id: String,
    pub name: SharedString,
    pub group: StateGroup,
    pub color: Hsla,
}

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub id: String,
    pub sequence_id: Option<u64>,
    pub title: SharedString,
    /// Plain text, paragraphs separated by newlines.
    pub description: SharedString,
    pub state_id: Option<String>,
    pub state_name: SharedString,
    pub state_group: StateGroup,
    pub state_color: Hsla,
    pub priority: Priority,
    pub link: String,
}

impl WorkItem {
    /// `WEB-42`, or `#42` when the project has no identifier.
    pub fn key(&self, identifier: &str) -> SharedString {
        match (self.sequence_id, identifier.is_empty()) {
            (Some(seq), false) => format!("{identifier}-{seq}").into(),
            (Some(seq), true) => format!("#{seq}").into(),
            (None, _) => "#?".into(),
        }
    }

    /// The composer-pill test, as pi-todos draws its widget: the state is
    /// literally named "In Progress" (not merely in the `started` group).
    pub fn in_progress(&self) -> bool {
        self.state_name.trim().eq_ignore_ascii_case(IN_PROGRESS_STATE)
    }

    fn apply_state(&mut self, state: &PlaneState) {
        self.state_id = Some(state.id.clone());
        self.state_name = state.name.clone();
        self.state_group = state.group;
        self.state_color = state.color;
    }
}

/// A workspace project offered when linking a Zeron project.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaneProject {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub identifier: String,
}

impl PlaneProject {
    pub fn link(&self, workspace_slug: &str) -> PlaneProjectLink {
        PlaneProjectLink {
            workspace_slug: workspace_slug.to_owned(),
            id: self.id.clone(),
            identifier: self.identifier.clone(),
            name: self.name.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Context helpers
// ---------------------------------------------------------------------------

/// The Zeron project the Plane surface and pills follow: the selected
/// session's project, else the new-session canvas's pick.
pub fn active_space_id(state: &AppState) -> Option<String> {
    match state.selected_chat_row() {
        Some(chat) => chat.space_id.clone(),
        None if state.no_project => None,
        None => state.selected_space.clone(),
    }
}

/// The Plane project linked to `space_id`, when Plane is connected and the
/// link names a workspace.
pub fn linked_project(space_id: Option<&str>, cx: &App) -> Option<PlaneProjectLink> {
    let plane = settings::plane(cx);
    if !plane.is_connected() {
        return None;
    }
    plane
        .link(space_id?)
        .filter(|link| !link.workspace_slug.is_empty())
}

pub fn link_project(space_id: &str, project: PlaneProjectLink, cx: &mut App) {
    settings::update(settings::SavePolicy::Immediate, cx, |s| {
        s.plane.projects.insert(space_id.to_owned(), project);
    });
}

pub fn unlink_project(space_id: &str, cx: &mut App) {
    settings::update(settings::SavePolicy::Immediate, cx, |s| {
        s.plane.projects.remove(space_id);
    });
}

/// Cache key for a linked project: the same project id never collides across
/// workspaces.
fn cache_key(link: &PlaneProjectLink) -> String {
    format!("{}/{}", link.workspace_slug, link.id)
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ProjectItems {
    pub items: Vec<WorkItem>,
    /// Workflow states in display order (group, then name).
    pub states: Vec<PlaneState>,
    pub fetched_at: Option<Instant>,
    pub error: Option<SharedString>,
    link: PlaneProjectLink,
    task: Option<Task<()>>,
    /// In-flight state changes by work item id.
    updates: HashMap<String, Task<()>>,
}

impl ProjectItems {
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }

    pub fn updating(&self, item_id: &str) -> bool {
        self.updates.contains_key(item_id)
    }

    pub fn item(&self, item_id: &str) -> Option<&WorkItem> {
        self.items.iter().find(|i| i.id == item_id)
    }
}

#[derive(Default)]
pub enum WorkspaceProjects {
    #[default]
    NotLoaded,
    Loading(#[allow(dead_code)] Task<()>),
    Loaded(Vec<PlaneProject>),
    Failed(SharedString),
}

/// Shared Plane cache: one per window, read by the Plane surface and the
/// composer pills. Keyed by workspace + Plane project, so two Zeron projects
/// linked to the same Plane project share one fetch.
pub struct PlaneStore {
    projects: HashMap<String, ProjectItems>,
    /// Project lists per workspace slug, for linking.
    workspace_projects: HashMap<String, WorkspaceProjects>,
    /// API key the cache was fetched with — a change drops everything.
    api_key: String,
    _refresh: Task<()>,
}

impl PlaneStore {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;
                let alive = this.update(cx, |store: &mut PlaneStore, cx| {
                    let links: Vec<PlaneProjectLink> =
                        store.projects.values().map(|p| p.link.clone()).collect();
                    for link in links {
                        store.refresh(&link, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            projects: HashMap::new(),
            workspace_projects: HashMap::new(),
            api_key: String::new(),
            _refresh: refresh,
        }
    }

    /// The current API key, clearing the cache when it changed since the last
    /// fetch. `None` while Plane is not connected.
    fn api_key(&mut self, cx: &App) -> Option<String> {
        let current = settings::plane(cx).api_key.trim().to_owned();
        if current != self.api_key {
            self.projects.clear();
            self.workspace_projects.clear();
            self.api_key = current.clone();
        }
        (!current.is_empty()).then_some(current)
    }

    pub fn items(&self, link: &PlaneProjectLink) -> Option<&ProjectItems> {
        self.projects.get(&cache_key(link))
    }

    /// Fetch `link` once; later calls are free until the next refresh.
    pub fn ensure(&mut self, link: &PlaneProjectLink, cx: &mut Context<Self>) {
        let _ = self.api_key(cx);
        if !self.projects.contains_key(&cache_key(link)) {
            self.refresh(link, cx);
        }
    }

    pub fn refresh(&mut self, link: &PlaneProjectLink, cx: &mut Context<Self>) {
        let Some(key) = self.api_key(cx) else {
            return;
        };
        let cache = cache_key(link);
        let entry = self.projects.entry(cache.clone()).or_default();
        entry.link = link.clone();
        if entry.task.is_some() {
            return;
        }
        let fetch = gpui_tokio::Tokio::spawn(
            cx,
            fetch_work_items(key, link.workspace_slug.clone(), link.id.clone()),
        );
        entry.task = Some(cx.spawn(async move |this, cx| {
            let result = match fetch.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            let _ = this.update(cx, |store: &mut PlaneStore, cx| {
                let Some(entry) = store.projects.get_mut(&cache) else {
                    return;
                };
                entry.task = None;
                match result {
                    Ok((mut items, states)) => {
                        // A state change still in flight wins over the
                        // snapshot fetched before it landed.
                        for item in &mut items {
                            if entry.updates.contains_key(&item.id)
                                && let Some(pending) = entry.item(&item.id)
                            {
                                *item = pending.clone();
                            }
                        }
                        entry.items = items;
                        entry.states = states;
                        entry.fetched_at = Some(Instant::now());
                        entry.error = None;
                    }
                    Err(err) => entry.error = Some(err.into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Move a work item to `state_id` (optimistically; reverted with an error
    /// when Plane refuses).
    pub fn set_state(
        &mut self,
        link: &PlaneProjectLink,
        item_id: &str,
        state_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.api_key(cx) else {
            return;
        };
        let cache = cache_key(link);
        let Some(entry) = self.projects.get_mut(&cache) else {
            return;
        };
        let Some(state) = entry.states.iter().find(|s| s.id == state_id).cloned() else {
            return;
        };
        let Some(item) = entry.items.iter_mut().find(|i| i.id == item_id) else {
            return;
        };
        if item.state_id.as_deref() == Some(state_id) {
            return;
        }
        let previous = item.clone();
        item.apply_state(&state);
        entry.error = None;

        let patch = gpui_tokio::Tokio::spawn(
            cx,
            patch_state(
                key,
                link.workspace_slug.clone(),
                link.id.clone(),
                item_id.to_owned(),
                state_id.to_owned(),
            ),
        );
        let item_id = item_id.to_owned();
        let task_item = item_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = match patch.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            let _ = this.update(cx, |store: &mut PlaneStore, cx| {
                let Some(entry) = store.projects.get_mut(&cache) else {
                    return;
                };
                entry.updates.remove(&task_item);
                if let Err(err) = result {
                    if let Some(item) = entry.items.iter_mut().find(|i| i.id == task_item) {
                        *item = previous;
                    }
                    entry.error = Some(format!("Couldn't change the state: {err}").into());
                }
                cx.notify();
            });
        });
        entry.updates.insert(item_id, task);
        cx.notify();
    }

    pub fn workspace_projects(&self, slug: &str) -> Option<&WorkspaceProjects> {
        self.workspace_projects.get(slug)
    }

    /// Load `slug`'s project list (for linking) unless already loading or
    /// loaded. `force` retries after a failure.
    pub fn load_workspace_projects(&mut self, slug: &str, force: bool, cx: &mut Context<Self>) {
        let Some(key) = self.api_key(cx) else {
            return;
        };
        let slug = slug.trim().trim_matches('/').to_owned();
        if slug.is_empty() {
            return;
        }
        match self.workspace_projects.get(&slug) {
            Some(WorkspaceProjects::NotLoaded) | None => {}
            Some(_) if !force => return,
            Some(_) => {}
        }
        let fetch = gpui_tokio::Tokio::spawn(cx, fetch_projects(key, slug.clone()));
        let task_slug = slug.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = match fetch.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            let _ = this.update(cx, |store: &mut PlaneStore, cx| {
                let loaded = match result {
                    Ok(mut projects) => {
                        projects.sort_by_key(|p| p.name.to_lowercase());
                        WorkspaceProjects::Loaded(projects)
                    }
                    Err(err) => WorkspaceProjects::Failed(err.into()),
                };
                store.workspace_projects.insert(task_slug, loaded);
                cx.notify();
            });
        });
        self.workspace_projects
            .insert(slug, WorkspaceProjects::Loading(task));
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Paged<T> {
    #[serde(default = "Vec::new")]
    results: Vec<T>,
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    next_page_results: bool,
}

/// List endpoints answer with a cursor page; tolerate a bare array too.
#[derive(Deserialize)]
#[serde(untagged)]
enum ListResponse<T> {
    Paged(Paged<T>),
    Bare(Vec<T>),
}

impl<T> From<ListResponse<T>> for Paged<T> {
    fn from(response: ListResponse<T>) -> Self {
        match response {
            ListResponse::Paged(page) => page,
            ListResponse::Bare(results) => Paged {
                results,
                next_cursor: None,
                next_page_results: false,
            },
        }
    }
}

#[derive(Deserialize)]
struct RawState {
    id: String,
    name: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    color: String,
}

#[derive(Deserialize)]
struct RawIssue {
    id: String,
    name: String,
    #[serde(default)]
    sequence_id: Option<u64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    description_html: Option<String>,
    #[serde(default)]
    description_stripped: Option<String>,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

/// Map a non-success response to a readable error.
async fn check(response: reqwest::Response) -> Result<reqwest::Response, String> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err("Plane rejected the API key. Check it in Settings → Plane.".into());
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("Not found on Plane — check the workspace slug and project link.".into());
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let body: String = body.chars().take(200).collect();
        return Err(format!("Plane API error {}: {body}", status.as_u16()));
    }
    Ok(response)
}

async fn get_page<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    key: &str,
    url: &str,
) -> Result<Paged<T>, String> {
    let response = client
        .get(url)
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Plane: {e}"))?;
    check(response)
        .await?
        .json::<ListResponse<T>>()
        .await
        .map(Paged::from)
        .map_err(|e| format!("Unexpected Plane response: {e}"))
}

/// Every page of a cursor-paginated list endpoint.
async fn get_all<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    key: &str,
    base_url: &str,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = format!("{base_url}?per_page={PAGE_SIZE}");
        if let Some(cursor) = &cursor {
            url.push_str("&cursor=");
            url.push_str(cursor);
        }
        let page = get_page::<T>(client, key, &url).await?;
        out.extend(page.results);
        match page.next_cursor {
            Some(next) if page.next_page_results => cursor = Some(next),
            _ => break,
        }
    }
    Ok(out)
}

async fn fetch_projects(key: String, slug: String) -> Result<Vec<PlaneProject>, String> {
    let client = client()?;
    get_all(&client, &key, &format!("{API_BASE}/workspaces/{slug}/projects/")).await
}

async fn fetch_work_items(
    key: String,
    slug: String,
    project_id: String,
) -> Result<(Vec<WorkItem>, Vec<PlaneState>), String> {
    let client = client()?;
    let base = format!("{API_BASE}/workspaces/{slug}/projects/{project_id}");
    let (states_url, issues_url) = (format!("{base}/states/"), format!("{base}/issues/"));
    let (raw_states, issues) = futures::try_join!(
        get_all::<RawState>(&client, &key, &states_url),
        get_all::<RawIssue>(&client, &key, &issues_url),
    )?;
    let mut states: Vec<PlaneState> = raw_states
        .into_iter()
        .map(|s| PlaneState {
            group: StateGroup::parse(&s.group),
            color: parse_hex(&s.color).unwrap_or_else(|| gpui::rgb(0x808080).into()),
            name: s.name.into(),
            id: s.id,
        })
        .collect();
    states.sort_by(|a, b| {
        a.group
            .sort_key()
            .cmp(&b.group.sort_key())
            .then_with(|| a.name.cmp(&b.name))
    });
    let by_id: HashMap<&str, &PlaneState> = states.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut items: Vec<WorkItem> = issues
        .into_iter()
        .map(|issue| {
            let state = issue.state.as_deref().and_then(|id| by_id.get(id).copied());
            let description = issue
                .description_html
                .as_deref()
                .map(html_to_text)
                .filter(|text| !text.is_empty())
                .or(issue.description_stripped)
                .unwrap_or_default();
            WorkItem {
                link: format!(
                    "{APP_BASE}/{slug}/projects/{project_id}/issues/{}",
                    issue.id
                ),
                id: issue.id,
                sequence_id: issue.sequence_id,
                title: issue.name.into(),
                description: description.into(),
                state_id: issue.state,
                state_name: state.map(|s| s.name.clone()).unwrap_or("Unknown".into()),
                state_group: state.map(|s| s.group).unwrap_or(StateGroup::Unknown),
                state_color: state
                    .map(|s| s.color)
                    .unwrap_or_else(|| gpui::rgb(0x808080).into()),
                priority: Priority::parse(issue.priority.as_deref()),
            }
        })
        .collect();
    sort_items(&mut items);
    Ok((items, states))
}

async fn patch_state(
    key: String,
    slug: String,
    project_id: String,
    item_id: String,
    state_id: String,
) -> Result<(), String> {
    let client = client()?;
    let url = format!("{API_BASE}/workspaces/{slug}/projects/{project_id}/issues/{item_id}/");
    let response = client
        .patch(url)
        .header("X-API-Key", key)
        .json(&serde_json::json!({ "state": state_id }))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Plane: {e}"))?;
    check(response).await.map(|_| ())
}

/// pi-todos order: priority, then state group, then title.
fn sort_items(items: &mut [WorkItem]) {
    items.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(a.state_group.sort_key().cmp(&b.state_group.sort_key()))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

/// Plane descriptions are rich-text HTML. Keep the paragraph structure
/// (block tags → newlines, list items → bullets), drop every other tag, and
/// decode the entities pi-todos' `stripHtml` handles.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end]
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = rest[start + 1..].starts_with('/');
        match tag.as_str() {
            "br" => out.push('\n'),
            "li" if !closing => out.push_str("\n• "),
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "pre"
            | "blockquote"
                if closing =>
            {
                out.push('\n')
            }
            _ => {}
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    // Trim each line; a run of blank lines becomes one paragraph gap, except
    // between consecutive bullets (each `<li><p>` ends in its own newline).
    // Leaving a list always opens a new paragraph.
    let mut text = String::new();
    let mut gap = false;
    let mut prev_bullet = false;
    for line in decoded.lines().map(str::trim) {
        if line.is_empty() {
            gap = true;
            continue;
        }
        let bullet = line.starts_with('•');
        if !text.is_empty() {
            let leaving_list = prev_bullet && !bullet;
            text.push_str(if leaving_list || (gap && !(prev_bullet && bullet)) {
                "\n\n"
            } else {
                "\n"
            });
        }
        text.push_str(line);
        gap = false;
        prev_bullet = bullet;
    }
    text
}

fn parse_hex(raw: &str) -> Option<Hsla> {
    let hex = raw.trim().trim_start_matches('#');
    let value = match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok()?,
        3 => {
            let short = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((short >> 8) & 0xF, (short >> 4) & 0xF, short & 0xF);
            (r * 0x11) << 16 | (g * 0x11) << 8 | (b * 0x11)
        }
        _ => return None,
    };
    Some(gpui::rgb(value).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, state: &str, group: StateGroup, priority: Priority) -> WorkItem {
        WorkItem {
            id: title.into(),
            sequence_id: Some(1),
            title: title.to_owned().into(),
            description: "".into(),
            state_id: None,
            state_name: state.to_owned().into(),
            state_group: group,
            state_color: gpui::rgb(0).into(),
            priority,
            link: String::new(),
        }
    }

    #[test]
    fn sorts_by_priority_then_group_then_title() {
        let mut items = vec![
            item("b", "Backlog", StateGroup::Backlog, Priority::None),
            item("a", "Backlog", StateGroup::Backlog, Priority::None),
            item("c", "In Progress", StateGroup::Started, Priority::None),
            item("d", "Backlog", StateGroup::Backlog, Priority::Urgent),
        ];
        sort_items(&mut items);
        let order: Vec<&str> = items.iter().map(|i| i.title.as_ref()).collect();
        assert_eq!(order, ["d", "c", "a", "b"]);
    }

    #[test]
    fn keys_use_the_project_identifier() {
        let i = item("x", "Todo", StateGroup::Unstarted, Priority::None);
        assert_eq!(i.key("WEB").as_ref(), "WEB-1");
        assert_eq!(i.key("").as_ref(), "#1");
    }

    #[test]
    fn parses_short_and_long_hex() {
        assert_eq!(parse_hex("#ffffff"), parse_hex("fff"));
        assert!(parse_hex("#12").is_none());
        assert!(parse_hex("").is_none());
    }

    #[test]
    fn in_progress_matches_the_state_name_not_the_group() {
        // pi-todos pins `state_name === "In Progress"`: another started-group
        // state (e.g. "In Review") is not a pill.
        assert!(item("x", "In Progress", StateGroup::Started, Priority::None).in_progress());
        assert!(!item("x", "In Review", StateGroup::Started, Priority::None).in_progress());
        assert!(!item("x", "Todo", StateGroup::Unstarted, Priority::None).in_progress());
        assert!(!StateGroup::Completed.is_active());
        assert!(StateGroup::Triage.is_active());
    }

    #[test]
    fn html_descriptions_keep_paragraphs_and_bullets() {
        let html = "<p>First &amp; <strong>bold</strong></p><p></p><ul><li><p>one</p></li><li>two</li></ul><p>a<br>b</p>";
        assert_eq!(html_to_text(html), "First & bold\n\n• one\n• two\n\na\nb");
        assert_eq!(html_to_text("<p></p>"), "");
    }

    #[test]
    fn list_responses_accept_pages_and_bare_arrays() {
        let paged: Paged<RawState> = serde_json::from_str::<ListResponse<RawState>>(
            r##"{"results":[{"id":"s1","name":"In Progress","group":"started","color":"#f59e0b"}],
                "next_cursor":"100:1:0","next_page_results":true,"total_count":150}"##,
        )
        .unwrap()
        .into();
        assert_eq!(paged.results.len(), 1);
        assert_eq!(paged.next_cursor.as_deref(), Some("100:1:0"));
        assert!(paged.next_page_results);

        let bare: Paged<RawState> = serde_json::from_str::<ListResponse<RawState>>(
            r#"[{"id":"s1","name":"Todo","group":"unstarted","color":""}]"#,
        )
        .unwrap()
        .into();
        assert_eq!(bare.results.len(), 1);
        assert!(!bare.next_page_results);
    }

    #[test]
    fn legacy_links_inherit_the_default_workspace() {
        let mut plane = crate::settings::PlaneSettings {
            api_key: "k".into(),
            workspace_slug: "acme".into(),
            ..Default::default()
        };
        plane.projects.insert(
            "s".into(),
            PlaneProjectLink {
                id: "p".into(),
                ..Default::default()
            },
        );
        plane.projects.insert(
            "t".into(),
            PlaneProjectLink {
                workspace_slug: "other".into(),
                id: "q".into(),
                ..Default::default()
            },
        );
        assert_eq!(plane.link("s").unwrap().workspace_slug, "acme");
        assert_eq!(plane.link("t").unwrap().workspace_slug, "other");
        assert_ne!(
            cache_key(&plane.link("s").unwrap()),
            cache_key(&plane.link("t").unwrap())
        );
    }
}
