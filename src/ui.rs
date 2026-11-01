//! Fluid Material 3 views built from API 4 native HTML primitives.
use crate::{
    astrobox::psys_host_v4::{dialog, ui},
    netease, state, transcode, transfer,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use qrcode::{QrCode, render::svg};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
const INK: &str = "#E0E4E0";
const PRIMARY: &str = "#A0D0BE";
const MUTED: &str = "#BEC9C3";
const SURFACE: &str = "#101512";
const CARD: &str = "#1C211F";
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
// The host renders every child of a raw root as plain HTML. GRID does not
// imply display:grid there; use explicit flex layout for every horizontal group.
fn equal_row() -> ui::Element {
    row().gap(8).width_full()
}
fn equal(el: ui::Element) -> ui::Element {
    el.width(0).min_width(0).flex_grow(1.0)
}
fn button(label: &str, event: &str, primary: bool) -> ui::Element {
    let el = ui::Element::new(ui::ElementType::Button, Some(label))
        .without_default_styles()
        .padding_top(12)
        .padding_bottom(12)
        .padding_left(20)
        .padding_right(20)
        .radius(24)
        .border(0, "transparent")
        .min_height(48)
        .max_width(320)
        .min_width(0)
        .bg(if primary { PRIMARY } else { "#303633" })
        .text_color(if primary { "#00382A" } else { INK })
        .size(14)
        .flex_shrink(0.0)
        .prop("key", event)
        .on(ui::Event::Click, event);
    disabled(
        el,
        state::action_busy()
            && event != "cancel"
            && !event.starts_with("tab:")
            && !event.starts_with("quality:"),
    )
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
    column().padding(24).radius(12).bg(CARD)
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
        .gap(12)
        .align_center()
        .min_width(0)
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
fn build(s: &state::State) -> ui::Element {
    // No fixed content width or duplicate app hero: the host already owns its title.
    let mut root = column()
        .width_full()
        .prop("class", "min-h-screen box-border")
        .flex_grow(1.0)
        .gap(24)
        .padding(16)
        .bg(SURFACE)
        .text_color(INK);
    let mut navigation = equal_row();
    for (id, label) in [
        ("local", "本地"),
        ("cloud", "网易云"),
        ("library", "曲库"),
        ("device", "设备"),
    ] {
        // Equal segments retain a 48px hit target at narrow phone widths.
        let selected = s.tab == id;
        navigation = navigation.child(equal(
            button(label, &format!("tab:{id}"), false)
                .max_width(4096)
                .padding_left(4)
                .padding_right(4)
                .height(56)
                .relative()
                .bg(if selected { CARD } else { SURFACE })
                .text_color(if selected { PRIMARY } else { MUTED })
                .radius(4)
                .child(
                    column()
                        .height(3)
                        .gap(0)
                        .absolute()
                        .left(12)
                        .right(12)
                        .bottom(0)
                        .radius(2)
                        .bg(if selected { PRIMARY } else { SURFACE }),
                ),
        ));
    }
    root = root.child(navigation);
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
    if !s.direct && s.tab != "device" {
        root = root.child(button("选择手环", "tab:device", false));
    }
    match s.tab.as_str() {
        "device" => {
            root = root.child(card()
                .child(text("连接手环", 28, INK))
                .child(text("先在 AstroBox 的设备页连接手环，并启用 NoirMusic。这里会列出已连接的手环。",16,MUTED))
                .child(disabled(button(if s.discovering {"正在刷新…"}else{"刷新设备"},"discover",true),s.discovering||s.active)));
            if s.devices.is_empty() {
                root = root.child(
                    card()
                        .child(text(
                            if s.discovering {
                                "正在读取设备…"
                            } else {
                                "暂无可用设备"
                            },
                            20,
                            INK,
                        ))
                        .child(text(
                            "在 AstroBox 连接手环后，点击上方刷新设备。",
                            14,
                            MUTED,
                        )),
                );
            }
            for (i, (addr, name)) in s.devices.iter().enumerate() {
                let connected = s.connected_devices.iter().any(|a| a == addr);
                root = root.child(
                    card()
                        .child(text(
                            if name.is_empty() {
                                "小米手环"
                            } else {
                                name
                            },
                            20,
                            INK,
                        ))
                        .child(text(
                            if s.direct && s.addr == *addr {
                                "音乐连接已就绪"
                            } else if connected {
                                "AstroBox 已连接"
                            } else {
                                "未连接 · 请先到 AstroBox 连接"
                            },
                            13,
                            MUTED,
                        ))
                        .child(disabled(
                            button(
                                if s.direct && s.addr == *addr {
                                    "重新连接"
                                } else {
                                    "连接音乐模块"
                                },
                                &format!("device:{i}"),
                                connected,
                            ),
                            s.active || !connected,
                        )),
                );
            }
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
            root = root.child(
                card()
                    .child(text("导入本地音乐", 22, INK))
                    .child(text("选择 MP3，压缩后发送到手环。", 14, MUTED))
                    .child(compression_options(&s))
                    .child(disabled(
                        button(
                            if s.local_path.is_empty() {
                                "选择 MP3"
                            } else {
                                "更换音频"
                            },
                            "pick",
                            false,
                        ),
                        s.active,
                    ))
                    .child(text("歌曲名称", 14, MUTED))
                    .child(
                        input("输入歌曲名称", &s.local_name, "name")
                            .prop("key", &format!("name:{}", s.local_path)),
                    )
                    .child(disabled(
                        button("开始导入", "upload", true),
                        s.active || s.local_path.is_empty() || !s.direct,
                    )),
            );
        }
    }
    root.child(column().height(24).flex_grow(1.0))
}
fn compression_options(s: &state::State) -> ui::Element {
    let mut choices = equal_row();
    for (rate, label) in [(24000, "极省"), (64000, "均衡"), (0, "原文件")] {
        choices = choices.child(equal(disabled(
            button(label, &format!("quality:{rate}"), s.bitrate == rate),
            s.active,
        )));
    }
    column()
        .child(text("压缩方式", 14, MUTED))
        .child(choices)
        .child(text("极省：24k 单声道 · 均衡：64k 立体声", 13, MUTED))
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
        state::with(|s| s.status = e);
    }
    drop(guard);
    rerender();
}
async fn action(id: &str, _value: &str) -> Result<(), String> {
    if let Some(rate) = id.strip_prefix("quality:").and_then(|r| r.parse().ok()) {
        state::with(|s| {
            if !s.active {
                s.bitrate = transcode::profile(rate);
            }
        });
        return Ok(());
    }
    if id == "predecode" {
        state::with(|s| {
            if !s.active {
                s.predecode = !s.predecode;
            }
        });
        return Ok(());
    }
    if let Some(index) = id
        .strip_prefix("device:")
        .and_then(|i| i.parse::<usize>().ok())
    {
        let s = state::snapshot();
        let device = s.devices.get(index).ok_or("设备列表已变化，请刷新")?;
        return transfer::connect_device(&device.0).await;
    }
    if let Some(tab) = id.strip_prefix("tab:") {
        state::with(|s| s.tab = tab.into());
        return Ok(());
    }
    if id == "cancel" {
        if transcode::cancel() {
            return Ok(());
        }
        return transfer::cancel().await;
    }
    if state::snapshot().active {
        return Err("正在传送音乐，请稍候".into());
    }
    match id {
        "discover" | "connect" => transfer::discover_devices().await,
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
            transcode::start(
                s.local_path,
                json!({"id":id,"name":s.local_name,"duration_ms":duration}),
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
                let prepared = netease::prepare(song, &s.cookie, s.bitrate)?;
                let meta = json!({"id":song.id,"name":song.name,"artists":song.artists.iter().map(|a|json!({"name":a})).collect::<Vec<_>>(),"album":{"id":song.album_id,"name":song.album},"duration_ms":song.duration_ms});
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
