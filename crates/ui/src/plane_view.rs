//! The Plane right-pane surface: every work item of the Plane project linked
//! to the current Zeron project, grouped by state (in progress first); a
//! detail page per item (title, description, state picker); and the pill the
//! composer shows for each "In Progress" item.
//!
//! One view per window; its content follows the selected session's project.
//! Data comes from the shared [`PlaneStore`].

use gpui::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, prelude::*, px,
};

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::plane::{self, PlaneState, PlaneStore, StateGroup, WorkItem, WorkspaceProjects};
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
    /// Workspace slug typed while linking a project.
    slug_input: Entity<ComposerInput>,
    /// The slug whose projects the link picker lists.
    picker_slug: Option<String>,
    /// Work item whose detail page is showing (list when `None`).
    open_item: Option<String>,
    /// The detail page's state dropdown.
    state_menu_open: bool,
    /// Last opened item — washed in the list after returning to it.
    highlighted: Option<String>,
    /// Scroll the highlighted row into view on the next list render.
    scroll_pending: bool,
    show_done: bool,
    scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PlaneViewEvent> for PlaneView {}

impl PlaneView {
    pub fn new(state: Entity<AppState>, store: Entity<PlaneStore>, cx: &mut Context<Self>) -> Self {
        let default_slug = crate::settings::plane(cx).workspace_slug;
        let slug_input = cx.new(|cx| {
            let mut input =
                ComposerInput::new("Workspace slug (app.plane.so/<slug>)", cx).with_single_line();
            input.set_text(default_slug, cx);
            input
        });
        let subs = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&slug_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.load_picker_projects(true, cx);
                }
            }),
        ];
        Self {
            state,
            store,
            slug_input,
            picker_slug: None,
            open_item: None,
            state_menu_open: false,
            highlighted: None,
            scroll_pending: false,
            show_done: false,
            scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    /// Open `item_id`'s detail page, or the list when `None`.
    pub fn reveal(&mut self, item_id: Option<String>, cx: &mut Context<Self>) {
        if item_id.is_some() {
            self.highlighted = item_id.clone();
        }
        self.open_item = item_id;
        self.state_menu_open = false;
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.open_item = None;
        self.state_menu_open = false;
        self.scroll_pending = self.highlighted.is_some();
        cx.notify();
    }

    fn load_picker_projects(&mut self, force: bool, cx: &mut Context<Self>) {
        let slug = self
            .slug_input
            .read(cx)
            .text()
            .trim()
            .trim_matches('/')
            .to_string();
        if slug.is_empty() {
            return;
        }
        self.picker_slug = Some(slug.clone());
        self.store
            .update(cx, |store, cx| store.load_workspace_projects(&slug, force, cx));
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

    /// Unlinked project: choose a workspace, then which of its Plane projects
    /// this Zeron project tracks.
    fn render_link_picker(&mut self, space_id: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        if self.picker_slug.is_none() {
            self.load_picker_projects(false, cx);
        }
        let space_name = self
            .state
            .read(cx)
            .space_row(&space_id)
            .map(|s| s.display_name().to_string())
            .unwrap_or_else(|| "this project".into());
        let slug = self.picker_slug.clone();
        let projects: Option<Result<Option<Vec<plane::PlaneProject>>, SharedString>> =
            slug.as_deref().map(|slug| {
                match self.store.read(cx).workspace_projects(slug) {
                    None | Some(WorkspaceProjects::NotLoaded) | Some(WorkspaceProjects::Loading(_)) => {
                        Ok(None)
                    }
                    Some(WorkspaceProjects::Failed(err)) => Err(err.clone()),
                    Some(WorkspaceProjects::Loaded(projects)) => Ok(Some(projects.clone())),
                }
            });
        let caption = |text: &str| {
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(text.to_owned()))
                .into_any_element()
        };
        let body = match projects {
            None => caption("Enter the workspace slug from your Plane URL."),
            Some(Ok(None)) => caption("Loading Plane projects…"),
            Some(Err(err)) => crate::settings::widgets::error_strip(&theme, err).into_any_element(),
            Some(Ok(Some(projects))) if projects.is_empty() => {
                caption("This Plane workspace has no projects.")
            }
            Some(Ok(Some(projects))) => {
                let slug = slug.clone().unwrap_or_default();
                let border = theme.border;
                let border_strong = theme.border_strong;
                let mut list = div().flex().flex_col().gap(px(6.0));
                for (ix, project) in projects.iter().enumerate() {
                    let link = project.link(&slug);
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                plane::link_project(&space_id, link.clone(), cx);
                                this.open_item = None;
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
                    .child(caption(
                        "Its work items will show here, and In Progress ones above the composer.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div().flex_1().min_w_0().child(popover::dialog_field(
                            self.slug_input.clone().into_any_element(),
                        )),
                    )
                    .child(
                        popover::btn_ghost(&theme, "Load projects", "plane-load-projects")
                            .id("plane-load-projects")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_picker_projects(true, cx)
                            })),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_list(
        &mut self,
        space_id: String,
        link: PlaneProjectLink,
        items: Vec<WorkItem>,
        loading: bool,
        fetched: bool,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let done_count = items.iter().filter(|i| !i.state_group.is_active()).count();
        let refresh_link = link.clone();
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
                    .child(SharedString::from(project_title(&link))),
            )
            .when(done_count > 0, |bar| {
                bar.child(
                    tool_button(
                        &theme,
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
                tool_button(
                    &theme,
                    "plane-refresh",
                    if loading { "Syncing…" } else { "Refresh" }.into(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let link = refresh_link.clone();
                    this.store.update(cx, |store, cx| store.refresh(&link, cx));
                })),
            )
            .child(
                tool_button(&theme, "plane-unlink", "Unlink".into()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        plane::unlink_project(&space_id, cx);
                        this.highlighted = None;
                        this.open_item = None;
                        this.picker_slug = None;
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
                        .text_color(priority_color(item.priority, theme))
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reveal(Some(id.clone()), cx);
            }))
            .into_any_element()
    }

    /// One work item: key, title, state picker, priority, description.
    fn render_detail(
        &mut self,
        link: PlaneProjectLink,
        item: WorkItem,
        states: Vec<PlaneState>,
        updating: bool,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let url = item.link.clone();
        let toolbar = crate::surface_chrome::toolbar(&theme)
            .child(
                tool_button(&theme, "plane-detail-back", "← All items".into()).on_click(
                    cx.listener(|this, _, _, cx| this.close_detail(cx)),
                ),
            )
            .child(div().flex_1())
            .child(
                tool_button(&theme, "plane-detail-open", "Open in Plane".into())
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );

        // The state trigger: current state's dot + name; opens the list of
        // this project's states below it.
        let trigger = div()
            .id("plane-state-trigger")
            .h(px(28.0))
            .px(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|s| s.bg(crate::theme::wash(0.06)))
            .child(
                div()
                    .size(px(8.0))
                    .rounded_full()
                    .bg(item.state_color),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text)
                    .child(item.state_name.clone()),
            )
            .child(
                icon(if self.state_menu_open {
                    icons::ALT_ARROW_UP
                } else {
                    icons::ALT_ARROW_DOWN
                })
                .size(px(12.0))
                .text_color(theme.text_muted),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.state_menu_open = !this.state_menu_open;
                cx.notify();
            }));

        let state_menu = self.state_menu_open.then(|| {
            let mut menu = popover::popover_card(&theme)
                .w(px(220.0))
                .flex()
                .flex_col()
                .gap(px(2.0));
            for (ix, state) in states.iter().enumerate() {
                let current = item.state_id.as_deref() == Some(state.id.as_str());
                let state_id = state.id.clone();
                let item_id = item.id.clone();
                let link = link.clone();
                menu = menu.child(
                    popover::menu_row(&theme, current, format!("plane-state-{ix}"))
                        .id(("plane-state-row", ix))
                        .child(div().size(px(8.0)).flex_none().rounded_full().bg(state.color))
                        .child(div().flex_1().child(state.name.clone()))
                        .when(current, |row| {
                            row.child(
                                icon(icons::CHECK)
                                    .size(px(12.0))
                                    .text_color(theme.text_muted),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state_menu_open = false;
                            let (link, item_id, state_id) =
                                (link.clone(), item_id.clone(), state_id.clone());
                            this.store.update(cx, |store, cx| {
                                store.set_state(&link, &item_id, &state_id, cx)
                            });
                            cx.notify();
                        })),
                );
            }
            menu
        });

        let description: AnyElement = if item.description.trim().is_empty() {
            div()
                .text_size(crate::typography::ui_rems(13.0))
                .text_color(theme.text_faint)
                .child(SharedString::from("No description."))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .text_size(crate::typography::ui_rems(13.0))
                .line_height(px(20.0))
                .text_color(theme.text)
                .children(item.description.split('\n').map(|line| {
                    if line.is_empty() {
                        div().h(px(8.0))
                    } else {
                        div().child(SharedString::from(line.to_owned()))
                    }
                }))
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .id("plane-detail")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .p(px(16.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .when_some(error, |el, err| {
                        el.child(crate::settings::widgets::error_strip(&theme, err))
                    })
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(11.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!(
                                "{} · {}",
                                item.key(&link.identifier),
                                link.name
                            ))),
                    )
                    .child(
                        div()
                            .text_size(crate::typography::ui_rems(16.0))
                            .line_height(px(22.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(item.title.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .child(trigger)
                            .when(updating, |el| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(theme.text_muted)
                                        .child(SharedString::from("Saving…")),
                                )
                            })
                            .when_some(item.priority.label(), |el, label| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(priority_color(item.priority, &theme))
                                        .child(SharedString::from(format!("{label} priority"))),
                                )
                            }),
                    )
                    .children(state_menu)
                    .child(div().h(px(1.0)).w_full().bg(theme.border))
                    .child(description),
            )
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
                "Add your Plane API key to see work items here.",
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
        let Some(link) = plane::linked_project(Some(&space_id), cx) else {
            return self.render_link_picker(space_id, cx);
        };

        self.store.update(cx, |store, cx| store.ensure(&link, cx));
        let store = self.store.read(cx);
        let entry = store.items(&link);
        let error = entry.and_then(|e| e.error.clone());
        if let Some(item_id) = self.open_item.clone()
            && let Some(entry) = entry
            && let Some(item) = entry.item(&item_id).cloned()
        {
            let states = entry.states.clone();
            let updating = entry.updating(&item_id);
            return self.render_detail(link, item, states, updating, error, cx);
        }
        let loading = entry.is_some_and(|e| e.loading());
        let fetched = entry.is_some_and(|e| e.fetched_at.is_some());
        let items = entry.map(|e| e.items.clone()).unwrap_or_default();
        self.render_list(space_id, link, items, loading, fetched, error, cx)
    }
}

fn project_title(link: &PlaneProjectLink) -> String {
    match link.identifier.is_empty() {
        true => format!("{} · {}", link.workspace_slug, link.name),
        false => format!("{} · {} · {}", link.workspace_slug, link.identifier, link.name),
    }
}

fn tool_button(theme: &Theme, id: &'static str, label: SharedString) -> gpui::Stateful<gpui::Div> {
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
}

fn priority_color(priority: plane::Priority, theme: &Theme) -> gpui::Hsla {
    match priority {
        plane::Priority::Urgent => theme.danger,
        plane::Priority::High => theme.warning,
        _ => theme.text_muted,
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

/// Composer pills: the linked project's items whose state is named
/// "In Progress" (pi-todos' widget rule), or `None` when there are none.
pub fn in_progress_items(
    store: &Entity<PlaneStore>,
    state: &Entity<AppState>,
    cx: &mut gpui::App,
) -> Option<(PlaneProjectLink, Vec<WorkItem>)> {
    let space_id = plane::active_space_id(state.read(cx));
    let link = plane::linked_project(space_id.as_deref(), cx)?;
    store.update(cx, |store, cx| store.ensure(&link, cx));
    let items: Vec<WorkItem> = store
        .read(cx)
        .items(&link)?
        .items
        .iter()
        .filter(|i| i.in_progress())
        .cloned()
        .collect();
    (!items.is_empty()).then_some((link, items))
}

/// One composer pill: state dot, `WEB-42`, truncated title. The caller adds
/// the click handler.
pub fn pill(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    dot: Option<gpui::Hsla>,
    key: Option<SharedString>,
    label: SharedString,
) -> gpui::Stateful<gpui::Div> {
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
