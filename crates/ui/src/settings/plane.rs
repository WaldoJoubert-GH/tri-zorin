//! Settings → Plane: the Plane.so API key and workspace slug, plus the list of
//! Zeron projects linked to Plane projects (linking itself happens in the
//! Plane right-pane surface).
//!
//! Writes go straight to the central settings store — the Shell re-reads the
//! `plane` block in `sync_independent_settings`, so there is no page event.

use gpui::{Context, Entity, SharedString, Window, div, prelude::*, px};

use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons;
use crate::popover;
use crate::settings::{self, SavePolicy, widgets};
use crate::state::AppState;
use crate::theme::Theme;

pub struct PlaneSettingsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    key_input: Entity<ComposerInput>,
    slug_input: Entity<ComposerInput>,
    /// The stored key is never echoed back into an input: while one exists,
    /// the row shows a masked summary until "Replace" is pressed.
    editing_key: bool,
    notice: Option<SharedString>,
    _subs: Vec<gpui::Subscription>,
}

/// `••••1234` — enough to recognise which key is stored.
fn masked(key: &str) -> SharedString {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••{tail}").into()
}

impl PlaneSettingsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let current = settings::plane(cx);
        let key_input = cx.new(|cx| ComposerInput::new("Plane API key", cx).with_single_line());
        let slug_input = cx.new(|cx| {
            let mut input = ComposerInput::new("Workspace slug (app.plane.so/<slug>)", cx)
                .with_single_line();
            input.set_text(current.workspace_slug.clone(), cx);
            input
        });
        let subs = vec![
            cx.subscribe(&key_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save(cx);
                }
            }),
            cx.subscribe(&slug_input, |this: &mut Self, _, event, cx| {
                if matches!(event, ComposerInputEvent::Submitted) {
                    this.save(cx);
                }
            }),
        ];
        Self {
            state,
            scroll: widgets::PageScroll::default(),
            key_input,
            slug_input,
            editing_key: current.api_key.is_empty(),
            notice: None,
            _subs: subs,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let slug = self
            .slug_input
            .read(cx)
            .text()
            .trim()
            .trim_matches('/')
            .to_string();
        let key = self.key_input.read(cx).text().trim().to_string();
        let replace_key = self.editing_key && !key.is_empty();
        settings::update(SavePolicy::Immediate, cx, |s| {
            s.plane.workspace_slug = slug;
            if replace_key {
                s.plane.api_key = key;
            }
        });
        if replace_key {
            self.key_input.update(cx, |input, cx| input.set_text("", cx));
            self.editing_key = false;
        }
        self.notice = Some("Saved".into());
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        settings::update(SavePolicy::Immediate, cx, |s| {
            s.plane.api_key.clear();
        });
        self.editing_key = true;
        self.notice = Some("API key removed".into());
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for PlaneSettingsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for PlaneSettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let plane = settings::plane(cx);

        let key_row: gpui::AnyElement = if self.editing_key {
            popover::dialog_field(self.key_input.clone().into_any_element()).into_any_element()
        } else {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text)
                        .child(masked(&plane.api_key)),
                )
                .child(
                    popover::btn_ghost(&theme, "Replace", "plane-key-replace")
                        .id("plane-key-replace")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.editing_key = true;
                            cx.notify();
                        })),
                )
                .child(
                    popover::btn_ghost(&theme, "Remove", "plane-key-remove")
                        .id("plane-key-remove")
                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                )
                .into_any_element()
        };

        let connection = widgets::section_card(&theme).child(
            div()
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(widgets::field_label(&theme, "API key"))
                        .child(key_row),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(widgets::field_label(&theme, "Workspace slug"))
                        .child(popover::dialog_field(
                            self.slug_input.clone().into_any_element(),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_end()
                        .gap(px(12.0))
                        .when_some(self.notice.clone(), |el, notice| {
                            el.child(
                                div()
                                    .text_size(crate::typography::ui_rems(12.0))
                                    .text_color(theme.text_muted)
                                    .child(notice),
                            )
                        })
                        .child(
                            popover::btn_primary(&theme, "Save")
                                .id("plane-save")
                                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                        ),
                ),
        );

        let mut links: Vec<(String, String, crate::settings::PlaneProjectLink)> = {
            let state = self.state.read(cx);
            plane
                .projects
                .iter()
                .map(|(space_id, link)| {
                    let name = state
                        .space_row(space_id)
                        .map(|s| s.display_name().to_string())
                        .unwrap_or_else(|| "Removed project".into());
                    (space_id.clone(), name, link.clone())
                })
                .collect()
        };
        links.sort_by_key(|(_, name, _)| name.to_lowercase());
        let linked = (!links.is_empty()).then(|| {
            let mut card = widgets::section_card(&theme);
            for (ix, (space_id, name, link)) in links.into_iter().enumerate() {
                card = card.child(
                    widgets::card_row(&theme, ix == 0)
                        .child(widgets::row_tile(&theme, icons::CHECKLIST))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(widgets::row_title(&theme, name))
                                .child(widgets::meta_line(
                                    &theme,
                                    vec![
                                        div()
                                            .child(SharedString::from(format!(
                                                "{} · {}",
                                                link.identifier, link.name
                                            )))
                                            .into_any_element(),
                                    ],
                                )),
                        )
                        .child(
                            popover::btn_ghost(&theme, "Unlink", format!("plane-unlink-{ix}"))
                                .id(("plane-unlink", ix))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    crate::plane::unlink_project(&space_id, cx);
                                    cx.notify();
                                })),
                        ),
                );
            }
            card
        });

        let scrollbar = popover::rail(self, "plane-page-scrollbar", &theme, cx);
        div()
            .id("plane-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("plane-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_header(&theme, "Plane", None))
                            .child(
                                widgets::page_subtitle(
                                    &theme,
                                    "Connect Plane.so to see a project's work items in the Plane \
                                     tab, with in-progress items pinned above the composer. \
                                     Create an API key under Profile settings → Personal access \
                                     tokens. It stays on this device.",
                                )
                                .max_w(px(512.0))
                                .line_height(px(20.0)),
                            )
                            .child(connection)
                            .when_some(linked, |col, card| {
                                col.child(widgets::field_label(&theme, "Linked projects"))
                                    .child(card)
                            }),
                    ),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::masked;

    #[test]
    fn masks_all_but_the_last_four() {
        assert_eq!(masked("plane_api_abcd1234").as_ref(), "••••1234");
        assert_eq!(masked("ab").as_ref(), "••••ab");
    }
}
