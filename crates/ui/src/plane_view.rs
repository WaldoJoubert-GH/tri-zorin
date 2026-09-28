//! The Plane right-pane surface: every work item of the Plane project linked
//! to the current Zeron project, grouped by state (in progress first), plus
//! the pill the composer shows for each in-progress item.
//!
//! One view per window; its content follows the selected session's project.
//! Data comes from the shared [`PlaneStore`].

use gpui::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, prelude::*, px,
};

use crate::icons::{self, icon};
use crate::plane::{self, PlaneStore, StateGroup, WorkItem, WorkspaceProjects};
use crate::popover;
use crate::settings::PlaneProjectLink;
use crate::state::AppState;
use crate::theme::Theme;

/// Most in-progress pills shown above the composer before a "+N" overflow.
pub const MAX_PILLS: usize = 4;

#[derive(Debug, Clone)]
pub enum PlaneViewEvent {
    /// The "Connect Plane" call to action — open Settings → Plane.
    OpenSettings,
}

pub struct PlaneView {
    state: Entity<AppState>,
    store: Entity<PlaneStore>,
    /// Work item revealed from a composer pill; the row is washed with the
    /// accent until another item is picked.
    highlighted: Option<String>,
    /// Scroll the highlighted row into view on the next render.
    scroll_pending: bool,
    show_done: bool,
    scroll: ScrollHandle,
    _observers: [Subscription; 2],
}

impl EventEmitter<PlaneViewEvent> for PlaneView {}

impl PlaneView {
    pub fn new(state: Entity<AppState>, store: Entity<PlaneStore>, cx: &mut Context<Self>) -> Self {
        Self {
            _observers: [
                cx.observe(&state, |_, _, cx| cx.notify()),
                cx.observe(&store, |_, _, cx| cx.notify()),
            ],
            state,
            store,
            highlighted: None,
            scroll_pending: false,
            show_done: false,
            scroll: ScrollHandle::new(),
        }
    }

    /// Highlight `item_id` and scroll it into view.
    pub fn reveal(&mut self, item_id: Option<String>, cx: &mut Context<Self>) {
        self.scroll_pending = item_id.is_some();
        self.highlighted = item_id;
        cx.notify();
    }

    fn render_message(
        &self,
        theme: &Theme,
        title: &str,
        body: &str,
        action: Option<AnyElement>,
    ) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .child(
                div()
                    .max_w(px(300.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.0))
                    .text_center()
                    .child(
                        icon(icons::CHECKLIST)
                            .size(px(20.0))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(title.to_owned())),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(px(18.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(body.to_owned())),
                    )
                    .children(action.map(|a| div().mt(px(8.0)).child(a))),
            )
            .into_any_element()
    }

    /// Unlinked project: choose which Plane project it tracks.
    fn render_link_picker(&mut self, space_id: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        self.store
            .update(cx, |store, cx| store.load_workspace_projects(false, cx));
        let space_name = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_else(|| "this project".into());
        let projects: Result<Option<Vec<plane::PlaneProject>>, SharedString> =
            match self.store.read(cx).workspace_projects() {
                WorkspaceProjects::NotLoaded | WorkspaceProjects::Loading(_) => Ok(None),
                WorkspaceProjects::Failed(err) => Err(err.clone()),
                WorkspaceProjects::Loaded(projects) => Ok(Some(projects.clone())),
            };
        let body = match projects {
            Ok(None) => div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from("Loading Plane projects…"))
                .into_any_element(),
            Err(err) => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(crate::settings::widgets::error_strip(&theme, err))
                .child(
                    popover::btn_ghost(&theme, "Retry", "plane-projects-retry")
                        .id("plane-projects-retry")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.store
                                .update(cx, |store, cx| store.load_workspace_projects(true, cx));
                        })),
                )
                .into_any_element(),
            Ok(Some(projects)) if projects.is_empty() => div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from("This Plane workspace has no projects."))
                .into_any_element(),
            Ok(Some(projects)) => {
                let border = theme.border;
                let border_strong = theme.border_strong;
                let mut list = div().flex().flex_col().gap(px(6.0));
                for (ix, project) in projects.iter().enumerate() {
                    let link = PlaneProjectLink::from(project);
                    let space_id = space_id.clone();
                    list = list.child(
                        div()
                            .id(("plane-link-project", ix))
                            .w_full()
                            .px(px(12.0))
                            .py(px(8.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(border)
                            .bg(crate::theme::ink(0.02))
                            .hover(move |s| {
                                s.bg(crate::theme::ink(0.05)).border_color(border_strong)
                            })
                            .cursor_pointer()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(project.identifier.clone())),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .text_color(theme.text)
                                    .child(SharedString::from(project.name.clone())),
                            )
                            .on_click(cx.listener(move |_, _, _, cx| {
                                plane::link_project(&space_id, link.clone(), cx);
                                cx.notify();
                            })),
                    );
                }
                list.into_any_element()
            }
        };
        div()
            .id("plane-link-picker")
            .size_full()
            .overflow_y_scroll()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(format!(
                                "Link {space_name} to a Plane project"
                            ))),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .child(SharedString::from(
                                "Its work items will show here, and in-progress ones above the composer.",
                            )),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_items(
        &mut self,
        space_id: String,
        link: PlaneProjectLink,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        self.store.update(cx, |store, cx| store.ensure(&link.id, cx));
        let store = self.store.read(cx);
        let entry = store.items(&link.id);
        let loading = entry.is_some_and(|e| e.loading());
        let error = entry.and_then(|e| e.error.clone());
        let items: Vec<WorkItem> = entry.map(|e| e.items.clone()).unwrap_or_default();
        let fetched = entry.is_some_and(|e| e.fetched_at.is_some());
        let done_count = items.iter().filter(|i| !i.state_group.is_active()).count();

        let tool = |id: &'static str, label: SharedString| {
            div()
                .id(id)
                .h(px(crate::surface_chrome::CONTROL_SIZE))
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.bg(crate::theme::wash(0.08)))
                .child(label)
        };
        let project_id = link.id.clone();
        let toolbar = crate::surface_chrome::toolbar(&theme)
            .child(
                icon(icons::CHECKLIST)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text)
                    .child(SharedString::from(if link.identifier.is_empty() {
                        link.name.clone()
                    } else {
                        format!("{} · {}", link.identifier, link.name)
                    })),
            )
            .when(done_count > 0, |bar| {
                bar.child(
                    tool(
                        "plane-toggle-done",
                        if self.show_done {
                            "Hide done".into()
                        } else {
                            format!("Show done ({done_count})").into()
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_done = !this.show_done;
                        cx.notify();
                    })),
                )
            })
            .child(
                tool(
                    "plane-refresh",
                    if loading { "Syncing…" } else { "Refresh" }.into(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = project_id.clone();
                    this.store.update(cx, |store, cx| store.refresh(&id, cx));
                })),
            )
            .child(
                tool("plane-unlink", "Unlink".into()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        plane::unlink_project(&space_id, cx);
                        this.highlighted = None;
                        cx.notify();
                    },
                )),
            );

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(err) = error {
            rows.push(
                div()
                    .p(px(8.0))
                    .child(crate::settings::widgets::error_strip(&theme, err))
                    .into_any_element(),
            );
        }
        let mut highlighted_ix = None;
        for group in StateGroup::DISPLAY_ORDER {
            if !group.is_active() && !self.show_done {
                continue;
            }
            let in_group: Vec<&WorkItem> =
                items.iter().filter(|i| i.state_group == group).collect();
            if in_group.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .px(px(16.0))
                    .pt(px(12.0))
                    .pb(px(4.0))
                    .flex()
                    .flex_row()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(SharedString::from(group_label(group)))
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(in_group.len().to_string())),
                    )
                    .into_any_element(),
            );
            for item in in_group {
                let is_highlighted = self.highlighted.as_deref() == Some(item.id.as_str());
                if is_highlighted {
                    highlighted_ix = Some(rows.len());
                }
                rows.push(self.render_row(item, &link.identifier, is_highlighted, &theme, cx));
            }
        }
        if rows.is_empty() {
            let body = if fetched {
                "Nothing open in this Plane project."
            } else {
                "Loading work items…"
            };
            rows.push(
                div()
                    .p(px(16.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(body))
                    .into_any_element(),
            );
        }
        if self.scroll_pending
            && let Some(ix) = highlighted_ix
        {
            self.scroll.scroll_to_item(ix);
            self.scroll_pending = false;
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .id("plane-items")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .pb(px(12.0))
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        item: &WorkItem,
        identifier: &str,
        highlighted: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = item.id.clone();
        let link = item.link.clone();
        let urgent = matches!(
            item.priority,
            plane::Priority::Urgent | plane::Priority::High
        );
        div()
            .id(SharedString::from(format!("plane-item-{}", item.id)))
            .mx(px(8.0))
            .px(px(8.0))
            .py(px(6.0))
            .rounded(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .when(highlighted, |el| el.bg(theme.accent.opacity(0.16)))
            .when(!highlighted, |el| el.hover(|s| s.bg(crate::theme::wash(0.06))))
            .child(
                div()
                    .size(px(8.0))
                    .flex_none()
                    .rounded_full()
                    .bg(item.state_color),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(item.key(identifier)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text)
                    .child(item.title.clone()),
            )
            .when_some(item.priority.label().filter(|_| urgent), |el, label| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(if item.priority == plane::Priority::Urgent {
                            theme.danger
                        } else {
                            theme.warning
                        })
                        .child(SharedString::from(label)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .max_w(px(96.0))
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(item.state_name.clone()),
            )
            // Click selects; double-click opens the item on app.plane.so.
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    cx.open_url(&link);
                }
                this.highlighted = Some(id.clone());
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for PlaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if !crate::settings::plane(cx).is_connected() {
            let action = popover::btn_primary(&theme, "Open Plane settings")
                .id("plane-open-settings")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(PlaneViewEvent::OpenSettings)))
                .into_any_element();
            return self.render_message(
                &theme,
                "Connect Plane",
                "Add your Plane API key and workspace slug to see work items here.",
                Some(action),
            );
        }
        let Some(space_id) = plane::active_space_id(self.state.read(cx)) else {
            return self.render_message(
                &theme,
                "No project",
                "Plane work items follow the session's project. This session isn't in one.",
                None,
            );
        };
        match plane::linked_project(Some(&space_id), cx) {
            Some(link) => self.render_items(space_id, link, cx),
            None => self.render_link_picker(space_id, cx),
        }
    }
}

fn group_label(group: StateGroup) -> &'static str {
    match group {
        StateGroup::Started => "In progress",
        StateGroup::Unstarted => "Todo",
        StateGroup::Backlog => "Backlog",
        StateGroup::Triage => "Triage",
        StateGroup::Completed => "Done",
        StateGroup::Cancelled => "Cancelled",
        StateGroup::Unknown => "Other",
    }
}

/// In-progress work items for the composer pills: the linked project's
/// `started` items, or `None` when the project is unlinked or has none.
pub fn in_progress_items(
    store: &Entity<PlaneStore>,
    state: &Entity<AppState>,
    cx: &mut gpui::App,
) -> Option<(PlaneProjectLink, Vec<WorkItem>)> {
    let space_id = plane::active_space_id(state.read(cx));
    let link = plane::linked_project(space_id.as_deref(), cx)?;
    store.update(cx, |store, cx| store.ensure(&link.id, cx));
    let items: Vec<WorkItem> = store
        .read(cx)
        .items(&link.id)?
        .items
        .iter()
        .filter(|i| i.in_progress())
        .cloned()
        .collect();
    (!items.is_empty()).then_some((link, items))
}

/// One composer pill: state dot, `WEB-42`, truncated title. The caller adds
/// the click handler.
pub fn pill(theme: &Theme, id: impl Into<gpui::ElementId>, dot: Option<gpui::Hsla>, key: Option<SharedString>, label: SharedString) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(24.0))
        .max_w(px(240.0))
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_card)
        .cursor_pointer()
        .hover(|s| s.bg(crate::theme::wash(0.08)))
        .when_some(dot, |el, dot| {
            el.child(div().size(px(6.0)).flex_none().rounded_full().bg(dot))
        })
        .when_some(key, |el, key| {
            el.child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(key),
            )
        })
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text)
                .child(label),
        )
}
