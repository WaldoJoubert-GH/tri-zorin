//! Offline Projects & worktrees sidebar fixture: the prototype's data (variant
//! B in the orca-sidebar prototype) in the real shell, for screenshots.
use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use serde_json::json;
use zeron_ui::*;

/// (project id, worktree branch, title, harness, session status, unread, minutes ago)
const SESSIONS: &[(&str, &str, &str, &str, &str, bool, i64)] = &[
    ("zeron", "zeron/orca-sidebar", "Prototype Orca-style sidebar", "claude-code", "working", false, 12),
    ("zeron", "zeron/orca-sidebar", "Audit sidebar row heights", "codex", "awaitingInput", false, 4),
    ("zeron", "main", "Diff Sidebar Updates", "claude-code", "idle", false, 120),
    ("zeron", "zeron/whale-ui-thread", "Prepare whale transcript off UI thread", "claude-code", "idle", true, 38),
    ("zeron", "zeron/transport-reliability", "Fix blank session revisits", "pi", "errored", true, 60),
    ("zeron", "zeron/transport-reliability", "Trace attachment race", "codex", "idle", false, 180),
    ("comet", "comet/takeover-cluster", "Merge and cut v0.1.47", "claude-code", "working", false, 6),
    ("comet", "main", "Triage flaky e2e", "cursor", "idle", false, 2880),
    ("pitodos", "feat/plane-sync", "Plane work item sync spike", "devin", "idle", true, 180),
    ("pitodos", "feat/plane-sync", "Due-date parsing", "pi", "working", false, 22),
    ("pitodos", "main", "Refresh README", "claude-code", "idle", false, 7200),
];

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
    gpui_platform::application().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let mut settings = settings::UiSettings::default();
        settings.sidebar_organization = settings::SidebarOrganization::ByWorktree;
        // ZERON_FIXTURE_FILTER=notes shows the empty project on its own.
        settings.space_filter = std::env::var("ZERON_FIXTURE_FILTER").ok();
        settings.surface = zeron_theme::SurfacePreference::Opaque;
        settings.save(&data).unwrap();
        settings::init(settings.clone(), data.clone(), cx);
        let fonts = typography::register_fonts(cx);
        typography::init(settings.ui_font_family.clone(), settings.ui_font_size, settings.terminal_font_family.clone(), settings.terminal_font_size, settings.code_font_family.clone(), settings.code_font_size, fonts, cx);
        theme_library::init(data.clone(), cx);
        appearance::init(appearance::AppearanceMode::Dark, settings.theme_selection, settings.accent, settings.surface, cx);
        history::init(settings.git_history_columns, settings.git_history_column_widths,
            settings.git_history_column_order, settings.git_history_author_display, cx);
        composer::init(cx, settings.composer_send_behavior);
        terminal::panel::init(cx);
        app_menus::init(cx);
        let state = cx.new(|_| {
            let now = chrono::Utc::now();
            let mut s = state::AppState::new();
            s.connection = zeron_proto::view::ConnectionStatus::Ready;
            s.workspace_scope = Some(zeron_proto::WorkspaceScope::Local);
            s.local_device_id = Some("metal".into());
            s.devices = serde_json::from_value(json!([
                {"id":"metal","name":"personal-metal","platform":"linux","lastSeenAt":null},
                {"id":"laptop","name":"WALDO-LAPTOP","platform":"windows","lastSeenAt":null}
            ])).unwrap();
            s.spaces = serde_json::from_value(json!([
                {"id":"zeron","deviceId":"metal","path":"/src/zeron","createdAt":"2026-09-01T00:00:00Z"},
                {"id":"comet","deviceId":"metal","path":"/src/comet","createdAt":"2026-09-01T00:00:00Z"},
                {"id":"pitodos","deviceId":"laptop","path":"C:/Projects/pi-todos","createdAt":"2026-09-01T00:00:00Z"},
                {"id":"notes","deviceId":"laptop","path":"C:/Projects/notes","createdAt":"2026-09-01T00:00:00Z"}
            ])).unwrap();
            for (ix, (space, branch, title, harness, status, unread, ago)) in SESSIONS.iter().enumerate() {
                let at = now - chrono::Duration::minutes(*ago);
                let device = if *space == "pitodos" { "laptop" } else { "metal" };
                let id = format!("chat-{ix}");
                s.chats.push(serde_json::from_value(json!({
                    "id": id, "deviceId": device, "spaceId": space, "title": title,
                    "archived": false, "branch": branch,
                    "checkoutId": format!("{space}:{branch}"),
                    "lastMessageAt": at, "createdAt": at,
                    "lastSeenAt": if *unread { at - chrono::Duration::minutes(1) } else { at },
                    "config": {"harness": harness, "model": null, "reasoning": null, "sandbox": "workspace-write"}
                })).unwrap());
                if *status != "idle" {
                    s.sessions.push(serde_json::from_value(json!({
                        "chatId": id, "deviceId": device, "status": status,
                        "startedAt": at, "updatedAt": now
                    })).unwrap());
                }
            }
            s.selected_chat = Some("chat-0".into());
            s.selected_space = Some("zeron".into());
            s.auto_selected = true;
            s.chats_synced = true;
            s.spaces_synced = true;
            s
        });
        let boot = EngineBootConfig { data_dir: data, ipc_port: 0, edge_url: String::new(), edge_token: None, org_id: None, workos_client_id: None, default_harness: HarnessId::ClaudeCode };
        cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(40.), px(40.)), size(px(1100.), px(1150.))))),
            titlebar: Some(gpui::TitlebarOptions { title: Some("sidebar-tree-fixture".into()), appears_transparent: true, traffic_light_position: Some(gpui::point(px(14.), px(14.))) }),
            app_owns_titlebar_drag: true,
            ..Default::default()
        }, |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx))).unwrap();
        state.update(cx, |_, cx| cx.notify());
        cx.activate(true);
    });
    Ok(())
}
