//! Fluid Material 3 views built from API 4 native HTML primitives.
use crate::{
    astrobox::psys_host_v4::{dialog, ui},
    attachments, bootstrap, cloud_download, connection, delivery, modules, netease, state, transcode, transfer,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use qrcode::{QrCode, render::svg};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
const INK: &str = "#E0E4E0";
const PRIMARY: &str = "#A0D0BE";
const MUTED: &str = "#BEC9C3";
const SURFACE: &str = "#000000";
const CARD: &str = "#1C211F";
thread_local! {
    static UI_NODES: RefCell<HashMap<String, ui::Element>> = RefCell::new(HashMap::new());
}
// Host builders clone borrowed elements with their ID intact. Cache childless
// prototypes so refreshing keeps node identity without accumulating children.
fn stable_node(kind: ui::ElementType, content: Option<&str>, key: &str) -> ui::Element {
    UI_NODES.with(|nodes| {
        let mut nodes=nodes.borrow_mut();
        let prototype=nodes.entry(key.into()).or_insert_with(||
            ui::Element::new(kind,None).without_default_styles());
        prototype.content(content).prop("key",key)
    })
}
fn fixed_box(el: ui::Element, width: u32, height: u32) -> ui::Element {
    el.width(width).min_width(width).max_width(width)
        .height(height).min_height(height).max_height(height)
        .flex_grow(0.0).flex_shrink(0.0)
        .animation("none").transition("none").transform("none")
}
fn text(s: &str, size: u32, color: &str) -> ui::Element {
    // URL/file-name tokens must break even on hosts without overflow-wrap.
    let mut wrapped = String::with_capacity(s.len());
    let mut run = 0;
    for ch in s.chars() {
        run = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/') {
            run + 1
        } else {
            0
        };
        if run > 12 {
            wrapped.push('​');
            run = 1;
        }
        wrapped.push(ch);
    }
    ui::Element::new(ui::ElementType::P, Some(&wrapped))
        .without_default_styles()
        .size(size)
        .text_color(color)
        .width_full()
        .min_width(0)
        .margin(0)
}
fn column() -> ui::Element {
    ui::Element::new(ui::ElementType::Div, None)
        .without_default_styles()
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .gap(16)
        .min_width(0)
}
fn progress_bar(percent: u64) -> ui::Element {
    let percent = percent.min(100);
    row().width_full().gap(0).height(4).radius(2).bg("#3F4943")
        .prop("role", "progressbar").prop("aria-valuemin", "0").prop("aria-valuemax", "100")
        .prop("aria-valuenow", &percent.to_string())
        .child(column().gap(0).height(4).width(0).flex_grow(percent as f32).bg(PRIMARY))
        .child(column().gap(0).height(4).width(0).flex_grow((100-percent) as f32))
}
// The host renders every child of a raw root as plain HTML. GRID does not
// imply display:grid there; use explicit flex layout for every horizontal group.
fn equal_row() -> ui::Element {
    row().gap(8).width_full()
}
fn equal(el: ui::Element) -> ui::Element {
    el.width(0).min_width(0).flex_grow(1.0)
}
fn button(label: &str, event: &str, primary: bool) -> ui::Element {
    let el = stable_node(ui::ElementType::Button, Some(label), &format!("button:{event}"))
        .animation("none").transition("none").transform("none")
        .padding_top(12)
        .padding_bottom(12)
        .padding_left(20)
        .padding_right(20)
        .radius(28)
        .border(0, "transparent")
        .min_height(48)
        .width_full()
        .min_width(0)
        .bg(if primary { PRIMARY } else { "#303633" })
        .text_color(if primary { "#00382A" } else { INK })
        .size(14)
        .flex_shrink(0.0)
        .prop("key", event)
        .on(ui::Event::Click, event);
    disabled(
        el,
        (state::action_busy() || state::snapshot().installing || state::snapshot().download.is_some())
            && event != "cancel"
            && !event.starts_with("tab:")
            && !event.starts_with("quality:"),
    )
}
fn toggle_switch(key: &str, label: &str, event: &str, enabled: bool, busy: bool) -> ui::Element {
    let track=fixed_box(stable_node(ui::ElementType::Button,None,key),52,32)
        .padding(0).radius(16).border(0,"transparent").relative()
        .flex().flex_direction(ui::FlexDirection::Row).align_center()
        .bg(if enabled {PRIMARY}else{"#59625D"})
        .prop("role","switch").prop("aria-label",label)
        .prop("aria-checked",if enabled {"true"}else{"false"})
        .on(ui::Event::Click,event);
    let knob=fixed_box(stable_node(ui::ElementType::Div,None,&format!("{key}:knob")),24,24)
        .radius(12).absolute().top(4).left(if enabled {24}else{4})
        .bg(if enabled {"#00382A"}else{INK});
    disabled(track.child(knob),busy)
}
fn module_switch(module: &modules::Module, busy: bool) -> ui::Element {
    toggle_switch(&format!("switch:{}",module.id), &format!("启用 {}",module.name()),
        &format!("module-toggle:{}",module.id), module.intent==1, busy||module.intent==2)
}
fn input(label: &str, value: &str, event: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Input, None)
        .without_default_styles()
        .width_full()
        .min_height(56)
        .size(16)
        .border(1, "#89938D")
        .prop("placeholder", label)
        .prop("default-value", value)
        .prop("key", event)
        .min_width(0)
        .padding_top(12)
        .padding_bottom(12)
        .padding_left(12)
        .padding_right(12)
        .radius(4)
        .bg("#252B27")
        .text_color(INK)
        .on(ui::Event::Input, event)
}
fn card() -> ui::Element {
    column().padding(16).radius(16).bg(CARD)
}
fn image(src: &str, size: u32) -> ui::Element {
    // API3 hosts consume image content; src is also set for hosts supporting props.
    ui::Element::new(ui::ElementType::Image, Some(src))
        .prop("src", src)
        .width(size)
        .height(size)
        .flex_shrink(0.0)
}
fn disabled(el: ui::Element, yes: bool) -> ui::Element {
    if yes { el.disabled().opacity(0.48) } else { el }
}
fn row() -> ui::Element {
    ui::Element::new(ui::ElementType::Div, None)
        .without_default_styles()
        .flex()
        .flex_direction(ui::FlexDirection::Row)
        .gap(12)
        .align_center()
        .min_width(0)
}
fn module_busy(s: &state::State) -> bool {
    s.manager_busy || s.installing || s.download.is_some() || s.active || s.connection.busy() || !s.manager_ready
}
fn uploader_tab(s: &state::State, id: &str) -> &'static str {
    if s.modules.iter().any(|m| m.id == id && m.music() && m.importable()) && s.direct && !s.connection.busy() {
        "local"
    } else { "module-uploader" }
}
fn icon_button(path: &str, label: &str, event: &str, accent: bool) -> ui::Element {
    let color=if accent {PRIMARY}else{MUTED};
    let svg=format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="{color}"><path d="{path}"/></svg>"#);
    let src=format!("data:image/svg+xml;base64,{}",STANDARD.encode(svg));
    let icon=fixed_box(stable_node(ui::ElementType::Image,Some(&src),&format!("icon:{event}")),20,20)
        .prop("src",&src).prop("width","20").prop("height","20")
        .prop("alt","").prop("aria-hidden","true").prop("draggable","false");
    disabled(fixed_box(stable_node(ui::ElementType::Button,None,event),40,40)
        .padding(0).margin(0).radius(20).border(0,"transparent").bg("transparent")
        .flex().flex_direction(ui::FlexDirection::Row).align_center().justify_center()
        .prop("aria-label",label).prop("title",label)
        .on(ui::Event::Click,event).child(icon),state::action_busy())
}
const DELETE_ICON: &str = "M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z";
const UPDATE_ICON: &str = "M4 12l1.41 1.41L11 7.83V20h2V7.83l5.59 5.58L20 12l-8-8-8 8z";
fn module_controls(s: &state::State, m: &modules::Module) -> ui::Element {
    let busy = module_busy(s);
    let update_available=s.cloud_modules.iter().any(|c|c.id==m.id && c.version>m.version);
    let update_label=if update_available {format!("更新 {}",m.name())} else {format!("{}：暂无可用更新",m.name())};
    let update=disabled(icon_button(UPDATE_ICON,&update_label,&format!("module-install:{}",m.id),update_available),
        !update_available||busy||!s.native_delivery||m.intent==2||m.flags&4!=0);
    fixed_box(stable_node(ui::ElementType::Div,None,&format!("controls:{}",m.id)),136,40)
        .flex().flex_direction(ui::FlexDirection::Row).align_center().gap(2)
        .child(module_switch(m,busy))
        .child(disabled(icon_button(DELETE_ICON,&format!("删除 {}",m.name()),&format!("module-delete:{}",m.id),false),busy||m.intent==2))
        .child(update)
}
fn module_activity(mut card: ui::Element, s: &state::State, id: &str) -> ui::Element {
    if let Some(p) = s.module_download(id) {
        let label = p.percent().map(|n| format!("下载中 · {n}%")).unwrap_or_else(|| "下载中…".into());
        card = card.child(text(&label,14,MUTED));
        if let Some(n) = p.percent() { card = card.child(progress_bar(n)); }
        return card.child(button("取消安装","cancel",false));
    }
    if s.installing && s.install_id == id {
        let n = if s.install_total == 0 { 0 } else { (s.install_sent.saturating_mul(100)/s.install_total).min(100) };
        return card.child(text(&format!("安装中 · {n}%"),14,MUTED))
            .child(progress_bar(n)).child(button("取消安装","cancel",false));
    }
    if let Some(job) = s.module_jobs.iter().find(|j|j.id == id) {
        if !job.message.is_empty() { card = card.child(text(&job.message,13,MUTED)); }
    }
    card
}
pub fn rerender() {
    let s = state::snapshot();
    if let Some(root) = s.root.as_deref() {
        ui::render(root, build(&s));
    }
}
pub fn render(root: &str) {
    state::with(|s| s.root = Some(root.into()));
    rerender();
}
fn connection_panel_visible(s: &state::State) -> bool {
    !s.manager_ready
}
#[cfg(test)]
mod connection_visibility_tests {
    use super::*;
    #[test]
    fn module_entry_opens_uploader_only_for_a_ready_supported_module() {
        let mut s = state::State::default();
        s.modules.push(modules::Module { id:"bluetooth_audio".into(),state:8,intent:1,..Default::default() });
        assert_eq!(uploader_tab(&s,"bluetooth_audio"),"module-uploader");
        s.direct=true;s.connection=connection::Phase::Ready;
        assert_eq!(uploader_tab(&s,"bluetooth_audio"),"local");
        s.modules[0].intent=0;
        assert_eq!(uploader_tab(&s,"bluetooth_audio"),"module-uploader");
        assert_eq!(uploader_tab(&s,"unknown"),"module-uploader");
    }
    #[test]
    fn connected_manager_hides_connection_panel_during_module_work() {
        let mut s = state::State::default();
        assert!(connection_panel_visible(&s));
        s.manager_ready = true;
        for phase in [connection::Phase::Ready, connection::Phase::Modules, connection::Phase::Music] {
            s.connection = phase;
            s.installing = true;
            assert!(!connection_panel_visible(&s));
        }
        s.manager_ready = false;
        assert!(connection_panel_visible(&s));
    }
}
fn build(s: &state::State) -> ui::Element {
    // No fixed content width or duplicate app hero: the host already owns its title.
    let mut root = stable_node(ui::ElementType::Div,None,"chem-root")
        .flex().flex_direction(ui::FlexDirection::Column).min_width(0)
        .animation("none").transition("none").transform("none")
        .width_full()
        .prop("class", "min-h-screen box-border")
        .flex_grow(1.0)
        .gap(24)
        .padding(16)
        .bg(SURFACE)
        .text_color(INK);
    let connection_text = match &s.connection {
        connection::Phase::Checking => "正在检查连接…",
        connection::Phase::Modules => "正在读取模块…",
        connection::Phase::Installer(message) => message,
        connection::Phase::Music => "正在准备音乐导入…",
        connection::Phase::Ready => if s.device_name.is_empty() { "手环已连接" } else { &s.device_name },
        connection::Phase::Failed(message) => message,
    };
    let mut connection_bar = row().child(equal(text(connection_text, 14, MUTED)));
    if !s.connection.busy() {
        connection_bar = connection_bar.child(disabled(
            button("刷新", "refresh", false).width(88),
            s.active || s.installing || s.manager_busy || s.download.is_some() || transfer::busy(),
        ));
    }
    if connection_panel_visible(s) { root = root.child(connection_bar); }
    if let Some(action) = s.installer_action.filter(|_| !s.manager_ready) {
        let (label, event) = match action {
            bootstrap::InstallerAction::Install => ("安装", "loader-install"),
            bootstrap::InstallerAction::Open => ("打开安装器", "loader-install"),
        };
        root = root.child(card().child(text("Chem 安装器", 20, INK))
            .child(button(label, event, true)));
    }
    if let Some(p) = s.download.as_ref().filter(|p| matches!(p.owner, cloud_download::Owner::Loader)) {
        let detail = if p.connecting {
            format!("等待服务器响应 · {} 秒", p.elapsed)
        } else if let Some(percent) = p.percent() {
            format!("{percent}% · {:.1} / {:.1} KB · {} 秒", p.received as f64 / 1024.0, p.total.unwrap_or(0) as f64 / 1024.0, p.elapsed)
        } else {
            format!("已下载 {:.1} KB · {} 秒", p.received as f64 / 1024.0, p.elapsed)
        };
        let mut download_card = card().child(text(&p.label, 20, INK)).child(text(&detail, 14, MUTED));
        if let Some(percent) = p.percent() { download_card = download_card.child(progress_bar(percent)); }
        root = root.child(download_card.child(button("取消下载", "cancel", false)));
    }
    if !s.status.is_empty() {
        root = root.child(text(
            &s.status,
            13,
            if s.direct { "#B5F2C7" } else { MUTED },
        ));
    }
    if s.active {
        let progress = if s.total == 0 {
            0
        } else {
            (s.sent.saturating_mul(100) / s.total).min(100) as u32
        };
        root = root.child(
            card()
                .child(text(
                    if s.transcoding {
                        "正在压缩"
                    } else {
                        "正在导入"
                    },
                    22,
                    INK,
                ))
                .child(
                    row()
                        .width_full()
                        .gap(0)
                        .height(4)
                        .radius(2)
                        .bg("#3F4943")
                        .child(
                            column()
                                .gap(0)
                                .height(4)
                                .width(0)
                                .flex_grow(progress as f32)
                                .bg(PRIMARY),
                        )
                        .child(
                            column()
                                .gap(0)
                                .height(4)
                                .width(0)
                                .flex_grow((100 - progress) as f32),
                        ),
                )
                .child(text(
                    &format!(
                        "{progress}% · {:.1} / {:.1} MB · {:.0} KB/s",
                        s.sent as f64 / 1048576.0,
                        s.total as f64 / 1048576.0,
                        s.speed as f64 / 1024.0
                    ),
                    14,
                    MUTED,
                ))
                .child(button("取消导入", "cancel", false)),
        );
    }
    let tab = if s.tab == "module-uploader" { uploader_tab(s, &s.module_detail) } else { &s.tab };
    if matches!(tab, "local"|"cloud"|"library"|"module-uploader") {
        let name = s.modules.iter().find(|m|m.id==s.module_detail).map(|m|m.name()).unwrap_or("模块");
        root = root.child(row().child(equal(button("‹ 模块列表", "tab:modules", false)))
            .child(equal(text(name,22,INK))));
    }
    if matches!(tab, "local"|"cloud"|"library") {
        let mut music_nav = equal_row();
        for (id,label) in [("local","本地"),("cloud","网易云"),("library","曲库")] {
            music_nav=music_nav.child(equal(button(label,&format!("tab:{id}"),tab==id)));
        }
        root=root.child(music_nav);
    }
    match tab {
        "modules" => {
            root=root.child(row().child(equal(text("模块管理",28,INK)))
                .child(disabled(button("刷新","refresh",false).width(72),s.installing||s.manager_busy||s.active||s.download.is_some()))
                .child(disabled(button("导入 .chem","module-import",true).width(128),!s.manager_ready||!s.native_delivery||s.manager_busy||s.installing||s.active||s.download.is_some()||s.connection.busy())));
            if s.manager_ready && s.boot_status & 1 != 0 {
                let enabled = s.boot_status & 2 != 0;
                root = root.child(stable_node(ui::ElementType::Div,None,"boot-panel")
                    .flex().flex_direction(ui::FlexDirection::Row).align_center().gap(12)
                    .padding_left(16).padding_right(16).padding_top(8).padding_bottom(8)
                    .min_height(48).radius(16).bg(CARD)
                    .child(equal(text("自动启动", 16, INK)))
                    .child(toggle_switch("boot-switch", "自动启动", "boot-toggle", enabled,
                        module_busy(s)||state::action_busy())));
            }
            if s.manager_ready && s.safe_mode {
                root = root.child(text(if s.safe_exit_pending { "恢复设置已保存，请重启手环" } else { "安全模式 · 已暂停模块自动启动" }, 16, INK));
                if !s.safe_exit_pending {
                    root = root.child(row()
                        .child(disabled(button("退出并重试", "safe-exit:0", false), module_busy(s)))
                        .child(disabled(button("停用全部后退出", "safe-exit:1", false), module_busy(s)))
                        .child(disabled(button("回退更新后退出", "safe-exit:2", false), module_busy(s))));
                }
            }
            if s.manager_ready && s.modules.is_empty() && s.cloud_modules.is_empty() && s.module_jobs.is_empty() {
                root=root.child(card().child(text("暂无已安装模块",20,INK)));
            }
            for m in &s.modules {
                let entry = stable_node(ui::ElementType::Button,Some(m.name()),&format!("entry:{}",m.id))
                    .padding(0).radius(8).height(48).min_height(48).max_height(48)
                    .border(0,"transparent").bg("transparent").text_color(INK).size(16)
                    .prop("class","truncate text-left")
                    .animation("none").transition("none").transform("none")
                    .prop("aria-label",&format!("打开 {} 上传器",m.name()))
                    .prop("title",&format!("打开 {} 上传器",m.name()))
                    .on(ui::Event::Click,&format!("module:{}",m.id));
                let line=stable_node(ui::ElementType::Div,None,&format!("line:{}",m.id))
                    .flex().flex_direction(ui::FlexDirection::Row).align_center().width_full()
                    .min_width(0).height(48).min_height(48).max_height(48).gap(8)
                    .animation("none").transition("none").transform("none")
                    .child(equal(entry)).child(module_controls(s,m));
                let mut module_card = stable_node(ui::ElementType::Div,None,&format!("module:{}",m.id))
                    .flex().flex_direction(ui::FlexDirection::Column).min_width(0)
                    .padding(16).padding_top(8).padding_bottom(8).radius(16).bg(CARD).gap(8)
                    .animation("none").transition("none").transform("none")
                    .child(line);
                if m.error!=0 || m.intent==2 || m.flags&4!=0 || m.state==14 {
                    module_card=module_card.child(text(&m.status(),13,MUTED));
                }
                root=root.child(module_activity(module_card,s,&m.id));
            }
            for m in s.cloud_modules.iter().filter(|c|!s.modules.iter().any(|m|m.id==c.id)) {
                root=root.child(module_activity(card().child(row().child(equal(column().gap(6).child(text(&m.name,20,INK))
                    .child(text(&format!("{} · 未安装",m.version_name),13,MUTED))))
                    .child(disabled(button("安装",&format!("module-install:{}",m.id),true).width(80),!s.manager_ready||!s.native_delivery||s.manager_busy||s.installing||s.download.is_some()||s.active||s.connection.busy()))),s,&m.id));
            }
            // Locally imported modules have no cloud card until the device
            // registry refreshes. Keep their progress visible during upload.
            for job in s.module_jobs.iter().filter(|j| !s.modules.iter().any(|m|m.id==j.id) && !s.cloud_modules.iter().any(|m|m.id==j.id)) {
                root=root.child(module_activity(card().child(text(&job.name,20,INK)),s,&job.id));
            }
        }
        "module-uploader" => {
            let message = match s.modules.iter().find(|m|m.id==s.module_detail) {
                Some(m) if !m.music() => "此模块暂未提供内容上传器，可在模块列表中管理。",
                Some(m) if !m.importable() => "请打开模块右侧的开关，重启手环并运行 Chem 后即可上传。",
                Some(_) => "音乐服务尚未连接，请刷新后重试。",
                None => "模块已不存在，请返回模块列表刷新。",
            };
            root = root.child(card().child(text("上传内容",22,INK)).child(text(message,14,MUTED))
                .child(disabled(button("刷新连接","refresh",true),module_busy(s))));
        }

        "cloud" => {
            let mut pane = card()
                .child(text("发现音乐", 22, INK))
                .child(text("歌曲或歌手", 14, MUTED))
                .child(input("搜索你想听的音乐", &s.query, "query"))
                .child(disabled(button("搜索", "search", true), s.active))
                .child(disabled(
                    button(
                        if s.cookie.is_empty() {
                            "登录网易云"
                        } else {
                            "我的歌单"
                        },
                        if s.cookie.is_empty() {
                            "login"
                        } else {
                            "playlists"
                        },
                        false,
                    ),
                    s.active,
                ));
            pane = pane.child(compression_options(&s));
            if !s.cookie.is_empty() {
                pane = pane.child(button("退出账号", "logout", false));
            }
            if !s.qr_url.is_empty() {
                if let Ok(q) = QrCode::new(&s.qr_url) {
                    let svg = q.render::<svg::Color>().min_dimensions(192, 192).build();
                    pane = pane
                        .child(column().align_center().child(image(
                            &format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg)),
                            192,
                        )))
                        .child(button("已扫码，确认登录", "login-check", true));
                }
            }
            root = root.child(pane);
            for playlist in &s.playlists {
                root = root.child(
                    card()
                        .child(text(&playlist.name, 18, INK))
                        .child(text(
                            &format!("{} 首 · {}", playlist.track_count, playlist.creator),
                            13,
                            MUTED,
                        ))
                        .child(disabled(
                            button("打开歌单", &format!("playlist:{}", playlist.id), false),
                            s.active,
                        )),
                );
            }
            for (i, song) in s.results.iter().enumerate() {
                let mut title = column()
                    .flex_grow(1.0)
                    .child(text(&song.name, 18, INK))
                    .child(text(&song.artists.join(" / "), 13, MUTED));
                if !song.cover_url.is_empty() {
                    title = row()
                        .child(image(&song.cover_url, 44).radius(12))
                        .child(title);
                }
                root = root.child(card().child(title).child(disabled(
                    button("导入手环", &format!("cloud:{i}"), true),
                    s.active || !s.direct,
                )));
            }
        }
        "library" => {
            root = root.child(disabled(
                button("刷新曲库", "library", true),
                s.active || !s.direct,
            ));
            if s.library.is_empty() {
                root = root.child(card().child(text("你的随身曲库", 22, INK)).child(text(
                    "导入歌曲后，在这里管理音乐。",
                    14,
                    MUTED,
                )));
            }
            for track in &s.library {
                let mut item = card().child(text(
                    track["name"].as_str().unwrap_or("未命名歌曲"),
                    18,
                    INK,
                ));
                let artists = track["artists"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v["name"].as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    })
                    .unwrap_or_default();
                item = item.child(text(&artists, 13, MUTED));
                if track["native"] == true {
                    item = item.child(disabled(
                        button(
                            "删除",
                            &format!("delete:{}", track["id"].as_u64().unwrap_or(0)),
                            false,
                        ),
                        s.active,
                    ));
                }
                root = root.child(item);
            }
        }
        _ => {
            let mut pane = card().child(text("导入音乐",22,INK))
                .child(text("选择 MP3 文件，再点击开始导入。",14,MUTED))
                .child(disabled(button(if s.local_path.is_empty() {"选择 MP3 文件"}else{"更换 MP3 文件"},"pick",false),s.active));
            if !s.local_path.is_empty() {
                pane = pane.child(text(Path::new(&s.local_path).file_name().and_then(|n|n.to_str()).unwrap_or("已选择音频"),13,MUTED))
                    .child(text("歌曲名称",14,MUTED))
                    .child(input("输入歌曲名称",&s.local_name,"name").prop("key",&format!("name:{}",s.local_path)));
            }
            pane = pane.child(disabled(button("开始导入","upload",true),s.active||s.local_path.is_empty()||!s.direct))
                .child(button(if s.upload_options {"收起导入设置 ⌃"}else{"导入设置 · 音质、封面与歌词 ›"},"upload-options",false));
            if s.upload_options {
                pane = pane.child(compression_options(s))
                    .child(text("封面与歌词 · 可选",14,INK))
                    .child(equal_row()
                        .child(equal(disabled(button(if s.cover_path.is_empty(){"添加封面"}else{"更换封面"},"pick-cover",false),s.active)))
                        .child(equal(disabled(button(if s.lyric_path.is_empty(){"添加歌词"}else{"更换歌词"},"pick-lyrics",false),s.active))))
                    .child(text(&format!("封面：{} · 歌词：{}",Path::new(&s.cover_path).file_name().and_then(|p|p.to_str()).unwrap_or("未选择"),Path::new(&s.lyric_path).file_name().and_then(|p|p.to_str()).unwrap_or("未选择")),13,MUTED));
                if !s.cover_path.is_empty() || !s.lyric_path.is_empty() { pane = pane.child(disabled(button("清除附件","clear-attachments",false),s.active)); }
            }
            root = root.child(pane);
        }
    }
    root.child(column().height(24).flex_grow(1.0))
}
fn compression_options(s: &state::State) -> ui::Element {
    let mut choices = column().gap(8);
    for (rate, label) in [(24000, "极省 · 24 kbps · 0.18 MB/分钟"), (48000, "轻量 · 48 kbps · 0.36 MB/分钟"), (80000,"清晰 · 80 kbps · 0.60 MB/分钟"), (0, "原文件 · 不重新编码")] {
        choices = choices.child(disabled(
            button(&format!("{}{}", if s.bitrate==rate { "✓  " } else { "" }, label), &format!("quality:{rate}"), s.bitrate == rate),
            s.active,
        ).width_full().max_width(10000));
    }
    column()
        .child(text("压缩方式", 14, MUTED))
        .child(choices)
        .child(text("压缩档位均保留立体声，24 kHz 采样；极省适合人声，清晰保留更多细节。体积为 MP3 估算。", 13, MUTED))
        .child(disabled(
            button(
                if s.predecode {
                    "SBC 预解码 · 已开启"
                } else {
                    "SBC 预解码 · 已关闭"
                },
                "predecode",
                s.predecode,
            ),
            s.active,
        ))
        .child(text(
            "先按所选档位压缩，再预解码；占用更多空间，减轻手环播放负担",
            13,
            MUTED,
        ))
}
fn ui_value(payload: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(payload) else {
        return payload.to_owned();
    };
    v.as_str()
        .or_else(|| v["value"].as_str())
        .or_else(|| v["target"]["value"].as_str())
        .unwrap_or_default()
        .to_owned()
}
pub async fn event(id: &str, payload: &str) {
    let value = ui_value(payload);
    match id {
        "query" => {
            state::with(|s| s.query = value);
            return;
        }
        "name" => {
            state::with(|s| s.local_name = value);
            return;
        }
        "quality" => {
            state::with(|s| {
                if !s.active {
                    s.bitrate = transcode::profile(value.parse().unwrap_or(24000));
                }
            });
            return;
        }
        _ => {}
    }
    // Navigation and editing remain responsive while a dialog/host call awaits.
    if id.starts_with("tab:") || id.starts_with("quality:") || id == "predecode" || id == "cancel" {
        if let Err(e) = action(id, &value).await {
            state::with(|s| s.status = e);
        }
        rerender();
        return;
    }
    let Some(guard) = state::try_action() else {
        return;
    };
    rerender();
    let result = action(id, &value).await;
    if let Err(e) = result {
        // A rejected UI action must not clear a live transfer's active state.
        state::with(|s| {
            if let Some(module) = id.strip_prefix("module-install:") {
                let name = s.cloud_modules.iter().find(|m|m.id==module).map(|m|m.name.clone()).unwrap_or_else(|| module.into());
                s.module_message(module, &name, &e);
            } else { s.status = e; }
        });
    }
    drop(guard);
    rerender();
}
async fn action(id: &str, _value: &str) -> Result<(), String> {
    if cloud_download::busy() && id != "cancel" && !id.starts_with("tab:") && !id.starts_with("quality:") {
        return Err("请先完成或取消下载".into());
    }
    if id == "boot-toggle" || id.starts_with("safe-exit:") {
        let s = state::snapshot();
        if module_busy(&s) { return Err("请先完成当前操作并连接管理器".into()); }
        let (title, content) = if id == "boot-toggle" {
            if s.boot_status & 2 != 0 { ("关闭自动启动？", "关闭后，重启手环需要手动运行 Chem 安装器。") }
            else { ("开启自动启动？", "之后重启手环会自动加载 Chem 和已启用模块。首次开启会修改固件启动脚本，请保持充电或电量达到 30%，操作期间不要重启手环。") }
        } else {
            match id {
                "safe-exit:0" => ("退出安全模式并重试？", "重启后重新尝试已启用的模块；如果启动再次失败，将返回安全模式。"),
                "safe-exit:1" => ("停用全部模块后退出？", "保留模块和数据，停用全部模块；重启手环后退出安全模式。"),
                "safe-exit:2" => ("回退更新后退出？", "撤销未运行的更新，回退有旧版本的模块，无可用旧版本的模块将停用。重启后退出安全模式。"),
                _ => return Err("无效的恢复操作".into()),
            }
        };
        let result = dialog::show_dialog(dialog::DialogType::Alert, dialog::DialogStyle::Website,
            dialog::DialogInfo { title: title.into(), content: content.into(), buttons: vec![
                dialog::DialogButton {id:"cancel".into(), primary:false, content:"取消".into()},
                dialog::DialogButton {id:"confirm".into(), primary:true, content:"确认".into()},
            ] }).await;
        if result.clicked_btn_id != "confirm" { return Ok(()); }
        return if id == "boot-toggle" { modules::set_boot(s.boot_status & 2 == 0).await }
            else { modules::exit_safe_mode(id.trim_start_matches("safe-exit:").parse().map_err(|_| "无效的恢复操作")?).await };
    }
    if id=="refresh" { connection::refresh(true).await; return Ok(()); }
    if id == "loader-install" { return bootstrap::install().await; }
    if let Some(module)=id.strip_prefix("module-install:") {
        let s=state::snapshot();
        if s.active||s.installing||s.manager_busy||!s.manager_ready{return Err("请先完成当前操作并连接模块管理器".into());}
        let m=s.cloud_modules.iter().find(|m|m.id==module).ok_or("云端列表已变化，请刷新")?;
        return cloud_download::module(&s.addr,m);
    }
    if id=="module-import" {
        let s=state::snapshot();
        if s.active||s.installing||s.manager_busy||!s.manager_ready||!s.native_delivery{return Err("请先连接最新 Chem 安装器".into());}
        let picked=dialog::pick_file(dialog::PickConfig{read:false,copy_to:Some("data/imports".into())},
            dialog::FilterConfig{multiple:false,extensions:vec!["chem".into()],default_directory:String::new(),default_file_name:String::new()}).await.map_err(|e|e.to_string())?;
        if picked.name.is_empty(){return Ok(());}
        if Path::new(&picked.name).file_name().and_then(|n|n.to_str())!=Some(&picked.name){return Err("文件名称无效".into());}
        let path=format!("data/imports/{}",picked.name);
        let result=delivery::start(&path,None).await;
        let _=fs::remove_file(path);return result;
    }
    if let Some(module)=id.strip_prefix("module-rollback:"){return modules::mutate(module,7).await;}
    if id=="module-delete-cancel" { state::with(|s|s.delete_module.clear());return Ok(()); }
    if let Some(module)=id.strip_prefix("module:") {
        state::with(|s| {
            s.tab=uploader_tab(s,module).into();s.module_detail=module.into();
            s.delete_module.clear();
        });return Ok(());
    }
    if let Some(module)=id.strip_prefix("module-delete:") {
        let s=state::snapshot();
        let m=s.modules.iter().find(|m|m.id==module).ok_or("请刷新模块列表")?;
        if module_busy(&s)||m.intent==2 {return Err("请等待当前操作完成".into());}
        let result=dialog::show_dialog(dialog::DialogType::Alert,dialog::DialogStyle::Website,
            dialog::DialogInfo {
                title:format!("删除 {}？",m.name()),
                content:"重启手环后移除模块，音乐数据保留。".into(),
                buttons:vec![
                    dialog::DialogButton{id:"cancel".into(),primary:false,content:"取消".into()},
                    dialog::DialogButton{id:"delete".into(),primary:true,content:"删除".into()},
                ],
            }).await;
        if result.clicked_btn_id=="delete" {return modules::mutate(module,5).await;}
        return Ok(());
    }
    if let Some(module)=id.strip_prefix("module-toggle:") {
        let s=state::snapshot();let m=s.modules.iter().find(|m|m.id==module).ok_or("请刷新模块列表")?;
        return modules::mutate(module,if m.intent==1 {4}else{3}).await;
    }


    if let Some(rate) = id.strip_prefix("quality:").and_then(|r| r.parse().ok()) {
        state::with(|s| {
            if !s.active {
                s.bitrate = transcode::profile(rate);
            }
        });
        return Ok(());
    }
    if id == "upload-options" {
        state::with(|s|s.upload_options=!s.upload_options);return Ok(());
    }
    if id == "predecode" {
        state::with(|s| {
            if !s.active {
                s.predecode = !s.predecode;
            }
        });
        return Ok(());
    }
    if let Some(tab) = id.strip_prefix("tab:") {
        state::with(|s| s.tab = tab.into());
        return Ok(());
    }
    if id == "cancel" {
        if cloud_download::busy() { cloud_download::cancel(false); return Ok(()); }
        if delivery::busy(){return delivery::cancel().await;}
        if transcode::cancel() {
            return Ok(());
        }
        return transfer::cancel().await;
    }
    if state::snapshot().active {
        return Err("正在传送音乐，请稍候".into());
    }
    if state::snapshot().connection.busy()
        && (matches!(id, "upload" | "library") || id.starts_with("cloud:") || id.starts_with("delete:")) {
        return Err("正在准备连接，请稍候".into());
    }
    match id {
        "clear-attachments" => { state::with(|s| { s.cover_path.clear(); s.lyric_path.clear(); }); Ok(()) }
        "pick-cover" | "pick-lyrics" => {
            let cover = id == "pick-cover";
            let picked = dialog::pick_file(dialog::PickConfig { read:false,copy_to:Some("media".into()) },
                dialog::FilterConfig { multiple:false, extensions:if cover { vec!["jpg".into(),"jpeg".into(),"png".into()] } else { vec!["lrc".into()] },default_directory:String::new(),default_file_name:String::new() }).await.map_err(|e| e.to_string())?;
            if !picked.name.is_empty() { state::with(|s| { let p=format!("media/{}",picked.name); if cover { s.cover_path=p; } else { s.lyric_path=p; } }); }
            Ok(())
        }
        "pick" => {
            let result = dialog::pick_file(
                dialog::PickConfig {
                    read: false,
                    copy_to: Some("media".into()),
                },
                dialog::FilterConfig {
                    multiple: false,
                    extensions: vec!["mp3".into()],
                    default_directory: String::new(),
                    default_file_name: String::new(),
                },
            )
            .await
            .map_err(|e| format!("选择文件失败：{e}"))?;
            if result.name.is_empty() {
                return Ok(());
            }
            let path = format!("media/{}", result.name);
            fs::metadata(&path).map_err(|e| format!("读取选择文件失败：{e}"))?;
            state::with(|s| {
                if !s.local_path.is_empty() && s.local_path != path {
                    let _ = fs::remove_file(&s.local_path);
                }
                s.local_path = path;
                s.local_name = Path::new(&result.name)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                s.status = "音频已选择，确认曲名后导入".into();
            });
            Ok(())
        }
        "upload" => {
            let s = state::snapshot();
            if s.local_name.trim().is_empty() {
                return Err("请输入曲名".into());
            }
            // Conversion measures emitted samples. Avoid scanning the source
            // synchronously before a bounded compression job even starts.
            let duration = if s.bitrate == 0 && !s.predecode {
                mp3_duration::from_path(&s.local_path)
                    .map_err(|e| format!("MP3 无法读取：{e}"))?
                    .as_millis()
                    .min(u32::MAX as u128) as u32
            } else {
                0
            };
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if (!s.cover_path.is_empty() || !s.lyric_path.is_empty()) && !s.attachments_supported { return Err("请更新 NoirMusic 后重新连接，以导入封面与歌词".into()); }
            let mut song=json!({"id":id,"name":s.local_name,"duration_ms":duration});
            attachments::local(&mut song,&s.cover_path,&s.lyric_path,if s.attachments_supported { &s.local_path } else { "" })?;
            transcode::start(
                s.local_path,
                song,
                false,
                s.bitrate,
            )
            .await
        }
        "library" => transfer::refresh_library().await,
        "search" => {
            let s = state::snapshot();
            if s.query.trim().is_empty() {
                return Err("输入歌曲或歌手后搜索".into());
            }
            state::with(|s| s.status = "正在搜索音乐".into());
            rerender();
            let rows = netease::search(&s.query, &s.cookie)?;
            state::with(|s| {
                s.results = rows;
                s.playlists.clear();
                s.status = "选一首喜欢的音乐".into();
            });
            Ok(())
        }
        "playlists" => {
            let s = state::snapshot();
            let rows = netease::user_playlists(&s.cookie)?;
            state::with(|s| {
                s.playlists = rows;
                s.results.clear();
                s.status = "选择歌单".into();
            });
            Ok(())
        }
        "login" => {
            let (key, url) = netease::begin_qr_login()?;
            state::with(|s| {
                s.qr_key = key;
                s.qr_url = url;
                s.status = "使用网易云音乐扫描二维码".into();
            });
            Ok(())
        }
        "login-check" => {
            let key = state::snapshot().qr_key;
            match netease::poll_qr_login(&key)? {
                Some(cookie) => {
                    fs::create_dir_all("data").map_err(|e| e.to_string())?;
                    fs::write("data/netease.cookie", &cookie).map_err(|e| e.to_string())?;
                    state::with(|s| {
                        s.cookie = cookie;
                        s.qr_key.clear();
                        s.qr_url.clear();
                        s.status = "已登录网易云".into();
                    });
                }
                None => state::with(|s| s.status = "等待扫码并在网易云确认登录".into()),
            }
            Ok(())
        }
        "logout" => {
            let _ = fs::remove_file("data/netease.cookie");
            state::with(|s| {
                s.cookie.clear();
                s.qr_key.clear();
                s.qr_url.clear();
                s.playlists.clear();
                s.status = "已退出登录".into();
            });
            Ok(())
        }
        _ => {
            if let Some(i) = id
                .strip_prefix("cloud:")
                .and_then(|v| v.parse::<usize>().ok())
            {
                let s = state::snapshot();
                let song = s.results.get(i).ok_or("歌曲已失效，请重新搜索")?;
                state::with(|s| s.status = "正在下载音乐到手机".into());
                rerender();
                let prepared = netease::prepare(song, &s.cookie, if s.bitrate == 0 {320000} else {192000})?;
                let mut meta = json!({"id":song.id,"name":song.name,"artists":song.artists.iter().map(|a|json!({"name":a})).collect::<Vec<_>>(),"album":{"id":song.album_id,"name":song.album},"duration_ms":song.duration_ms});
                if s.attachments_supported { netease::prepare_attachments(song,&s.cookie,&mut meta); }
                let path = prepared.path.clone();
                let result = transcode::start(prepared.path, meta, true, s.bitrate).await;
                if result.is_err() {
                    let _ = fs::remove_file(path);
                }
                return result;
            }
            if let Some(id) = id.strip_prefix("playlist:").and_then(|v| v.parse().ok()) {
                let s = state::snapshot();
                let (name, tracks) = netease::playlist_tracks(id, &s.cookie)?;
                state::with(|s| {
                    s.results = tracks;
                    s.playlists.clear();
                    s.status = name;
                });
                return Ok(());
            }
            if let Some(id) = id
                .strip_prefix("delete:")
                .and_then(|v| v.parse::<u64>().ok())
            {
                let result = dialog::show_dialog(
                    dialog::DialogType::Alert,
                    dialog::DialogStyle::Website,
                    dialog::DialogInfo {
                        title: "删除手环歌曲".into(),
                        content: "删除后可重新导入。是否继续？".into(),
                        buttons: vec![
                            dialog::DialogButton {
                                id: "cancel".into(),
                                primary: false,
                                content: "取消".into(),
                            },
                            dialog::DialogButton {
                                id: "delete".into(),
                                primary: true,
                                content: "删除".into(),
                            },
                        ],
                    },
                )
                .await;
                if result.clicked_btn_id == "delete" {
                    return transfer::send_command(
                        7,
                        serde_json::to_vec(&json!({"id":id})).unwrap(),
                    )
                    .await;
                }
                return Ok(());
            }
            Ok(())
        }
    }
}
