//! Plane.so work items for the selected project — the data half of the Plane
//! right-pane surface ([`crate::plane_view`]) and the composer's in-progress
//! pills.
//!
//! Ported from pi-todos' `plane.ts`: the same REST endpoints (`/states/` plus
//! `/issues/` per project, `X-API-Key` auth), the same five-minute background
//! sync, and the same priority → state-group → title ordering. Unlike pi-todos
//! (one project per repo via `.dev/config.json`), each Zeron project (space)
//! links to its own Plane project through [`PlaneSettings::projects`].
//!
//! [`PlaneSettings::projects`]: crate::settings::PlaneSettings

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, Context, Hsla, SharedString, Task};
use serde::Deserialize;

use crate::settings::{self, PlaneProjectLink, PlaneSettings};
use crate::state::AppState;

const API_BASE: &str = "https://api.plane.so/api/v1";
const APP_BASE: &str = "https://app.plane.so";
/// pi-todos `SYNC_INTERVAL_MS`.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const PAGE_SIZE: usize = 100;
/// Hard stop for runaway pagination (2 000 work items per project).
const MAX_PAGES: usize = 20;

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

    fn sort_key(self) -> usize {
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

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub id: String,
    pub sequence_id: Option<u64>,
    pub title: SharedString,
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

    pub fn in_progress(&self) -> bool {
        self.state_group == StateGroup::Started
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

impl From<&PlaneProject> for PlaneProjectLink {
    fn from(project: &PlaneProject) -> Self {
        Self {
            id: project.id.clone(),
            identifier: project.identifier.clone(),
            name: project.name.clone(),
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

/// The Plane project linked to `space_id`, when Plane is connected.
pub fn linked_project(space_id: Option<&str>, cx: &App) -> Option<PlaneProjectLink> {
    let plane = settings::plane(cx);
    if !plane.is_connected() {
        return None;
    }
    plane.projects.get(space_id?).cloned()
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

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ProjectItems {
    pub items: Vec<WorkItem>,
    pub fetched_at: Option<Instant>,
    pub error: Option<SharedString>,
    task: Option<Task<()>>,
}

impl ProjectItems {
    pub fn loading(&self) -> bool {
        self.task.is_some()
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
/// composer pills. Keyed by Plane project id, so two Zeron projects linked to
/// the same Plane project share one fetch.
pub struct PlaneStore {
    projects: HashMap<String, ProjectItems>,
    workspace_projects: WorkspaceProjects,
    /// Credentials the cache was fetched with — a change drops everything.
    credentials: (String, String),
    _refresh: Task<()>,
}

impl PlaneStore {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_INTERVAL).await;
                let alive = this.update(cx, |store: &mut PlaneStore, cx| {
                    let ids: Vec<String> = store.projects.keys().cloned().collect();
                    for id in ids {
                        store.refresh(&id, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            projects: HashMap::new(),
            workspace_projects: WorkspaceProjects::NotLoaded,
            credentials: Default::default(),
            _refresh: refresh,
        }
    }

    /// Current credentials, clearing the cache when they changed since the
    /// last fetch. `None` while Plane is not connected.
    fn credentials(&mut self, cx: &App) -> Option<(String, String)> {
        let PlaneSettings {
            api_key,
            workspace_slug,
            ..
        } = settings::plane(cx);
        let current = (api_key.trim().to_owned(), workspace_slug.trim().to_owned());
        if current != self.credentials {
            self.projects.clear();
            self.workspace_projects = WorkspaceProjects::NotLoaded;
            self.credentials = current.clone();
        }
        (!current.0.is_empty() && !current.1.is_empty()).then_some(current)
    }

    pub fn items(&self, project_id: &str) -> Option<&ProjectItems> {
        self.projects.get(project_id)
    }

    /// Fetch `project_id` once; later calls are free until the next refresh.
    pub fn ensure(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let _ = self.credentials(cx);
        if !self.projects.contains_key(project_id) {
            self.refresh(project_id, cx);
        }
    }

    pub fn refresh(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let Some((key, slug)) = self.credentials(cx) else {
            return;
        };
        let entry = self.projects.entry(project_id.to_owned()).or_default();
        if entry.task.is_some() {
            return;
        }
        let id = project_id.to_owned();
        let fetch = gpui_tokio::Tokio::spawn(cx, fetch_work_items(key, slug, id.clone()));
        entry.task = Some(cx.spawn(async move |this, cx| {
            let result = match fetch.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            let _ = this.update(cx, |store: &mut PlaneStore, cx| {
                let Some(entry) = store.projects.get_mut(&id) else {
                    return;
                };
                entry.task = None;
                match result {
                    Ok(items) => {
                        entry.items = items;
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

    pub fn workspace_projects(&self) -> &WorkspaceProjects {
        &self.workspace_projects
    }

    /// Load the workspace's project list (for linking) unless already
    /// loading or loaded. `force` retries after a failure.
    pub fn load_workspace_projects(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some((key, slug)) = self.credentials(cx) else {
            return;
        };
        match self.workspace_projects {
            WorkspaceProjects::Loading(_) | WorkspaceProjects::Loaded(_) if !force => return,
            WorkspaceProjects::Failed(_) if !force => return,
            _ => {}
        }
        let fetch = gpui_tokio::Tokio::spawn(cx, fetch_projects(key, slug));
        self.workspace_projects = WorkspaceProjects::Loading(cx.spawn(async move |this, cx| {
            let result = match fetch.await {
                Ok(result) => result,
                Err(err) => Err(err.to_string()),
            };
            let _ = this.update(cx, |store: &mut PlaneStore, cx| {
                store.workspace_projects = match result {
                    Ok(mut projects) => {
                        projects.sort_by_key(|p| p.name.to_lowercase());
                        WorkspaceProjects::Loaded(projects)
                    }
                    Err(err) => WorkspaceProjects::Failed(err.into()),
                };
                cx.notify();
            });
        }));
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
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
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
    response
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
) -> Result<Vec<WorkItem>, String> {
    let client = client()?;
    let base = format!("{API_BASE}/workspaces/{slug}/projects/{project_id}");
    let (states_url, issues_url) = (format!("{base}/states/"), format!("{base}/issues/"));
    let (states, issues) = futures::try_join!(
        get_all::<RawState>(&client, &key, &states_url),
        get_all::<RawIssue>(&client, &key, &issues_url),
    )?;
    let states: HashMap<String, RawState> = states.into_iter().map(|s| (s.id.clone(), s)).collect();
    let mut items: Vec<WorkItem> = issues
        .into_iter()
        .map(|issue| {
            let state = issue.state.as_deref().and_then(|id| states.get(id));
            WorkItem {
                link: format!(
                    "{APP_BASE}/{slug}/projects/{project_id}/issues/{}",
                    issue.id
                ),
                id: issue.id,
                sequence_id: issue.sequence_id,
                title: issue.name.into(),
                state_name: state
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "Unknown".into())
                    .into(),
                state_group: state
                    .map(|s| StateGroup::parse(&s.group))
                    .unwrap_or(StateGroup::Unknown),
                state_color: state
                    .and_then(|s| parse_hex(&s.color))
                    .unwrap_or_else(|| gpui::rgb(0x808080).into()),
                priority: Priority::parse(issue.priority.as_deref()),
            }
        })
        .collect();
    sort_items(&mut items);
    Ok(items)
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

    fn item(title: &str, group: StateGroup, priority: Priority) -> WorkItem {
        WorkItem {
            id: title.into(),
            sequence_id: Some(1),
            title: title.to_owned().into(),
            state_name: "S".into(),
            state_group: group,
            state_color: gpui::rgb(0).into(),
            priority,
            link: String::new(),
        }
    }

    #[test]
    fn sorts_by_priority_then_group_then_title() {
        let mut items = vec![
            item("b", StateGroup::Backlog, Priority::None),
            item("a", StateGroup::Backlog, Priority::None),
            item("c", StateGroup::Started, Priority::None),
            item("d", StateGroup::Backlog, Priority::Urgent),
        ];
        sort_items(&mut items);
        let order: Vec<&str> = items.iter().map(|i| i.title.as_ref()).collect();
        assert_eq!(order, ["d", "c", "a", "b"]);
    }

    #[test]
    fn keys_use_the_project_identifier() {
        let i = item("x", StateGroup::Started, Priority::None);
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
    fn only_started_counts_as_in_progress() {
        assert!(item("x", StateGroup::Started, Priority::None).in_progress());
        assert!(!item("x", StateGroup::Unstarted, Priority::None).in_progress());
        assert!(!StateGroup::Completed.is_active());
        assert!(StateGroup::Triage.is_active());
    }
}
