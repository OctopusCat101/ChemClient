use super::*;
use std::collections::{HashMap, HashSet};
use ui::Node;

fn fixture() -> state::State {
    let mut s = state::State {
        addr: "band".into(),
        connected_devices: vec!["band".into()],
        manager_ready: true,
        direct: true,
        native_delivery: true,
        bulk_delivery: true,
        connection: connection::Phase::Ready,
        tab: "modules".into(),
        module_detail: "bluetooth_audio".into(),
        bitrate: 24000,
        boot_status: 3,
        ..Default::default()
    };
    for id in ["bluetooth_audio", "com.chem.magic"] {
        s.modules.push(modules::Module {
            id: id.into(),
            state: 9,
            intent: 1,
            ..Default::default()
        });
    }
    s.cloud_modules.push(crate::cloud::Module {
        id: "new_module".into(),
        name: "待安装模块".into(),
        ..Default::default()
    });
    s.results = [7, 8]
        .into_iter()
        .map(|id| state::CloudSong {
            id,
            name: format!("歌曲 {id}"),
            artists: vec!["歌手".into()],
            ..Default::default()
        })
        .collect();
    s.library = vec![json!({"id":7,"name":"歌曲 7","native":true})];
    s
}
fn walk<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    out.push(n);
    for c in &n.children {
        walk(c, out);
    }
}
fn nodes(n: &Node) -> Vec<&Node> {
    let mut out = Vec::new();
    walk(n, &mut out);
    out
}
fn key(n: &Node) -> &str {
    n.props.get("key").expect("every node needs a scoped key")
}
fn identities(tree: &Node) -> HashMap<String, (u64, String)> {
    let mut seen_ids = HashSet::new();
    let mut seen_keys = HashMap::new();
    for n in nodes(tree) {
        assert!(
            seen_ids.insert(n.id),
            "duplicate native ID {} at {}",
            n.id,
            key(n)
        );
        assert!(
            seen_keys
                .insert(key(n).to_owned(), (n.id, n.kind.clone()))
                .is_none(),
            "duplicate key {}",
            key(n)
        );
    }
    seen_keys
}
fn tree(s: &state::State) -> Node {
    state::with(|current| *current = s.clone());
    let out = build(s).snapshot();
    identities(&out);
    UI_NODES.with(|cache| {
        for prototype in cache.borrow().values() {
            assert!(
                prototype.snapshot().children.is_empty(),
                "cached prototypes must stay childless"
            );
            assert!(
                prototype.snapshot().events.is_empty(),
                "cached prototypes must not retain event listeners"
            );
        }
        assert_eq!(
            cache.borrow().len(),
            nodes(&out).len(),
            "only visible nodes should be retained"
        );
    });
    out
}
fn event_node<'a>(tree: &'a Node, event: &str) -> &'a Node {
    nodes(tree)
        .into_iter()
        .find(|n| {
            n.events.iter().any(|(_, id)| {
                id.strip_prefix("view:")
                    .and_then(|s| s.split_once(':'))
                    .is_some_and(|(_, id)| id == event)
            })
        })
        .unwrap_or_else(|| panic!("missing event {event}"))
}
fn has_text(tree: &Node, text: &str) -> bool {
    nodes(tree)
        .iter()
        .any(|n| n.content.as_deref().is_some_and(|c| c.contains(text)))
}
#[test]
fn complete_trees_remain_isolated_across_navigation_and_background_progress() {
    crate::magic::test_view(Default::default());
    let mut s = fixture();
    s.active = true;
    s.song_job = Some(state::SongJob {
        id: 7,
        name: "后台歌曲".into(),
        network: true,
        received: 123,
        message: "专属歌曲进度".into(),
        ..Default::default()
    });
    s.module_message("new_module", "待安装模块", "专属模块进度");
    let mut previous = None::<(String, HashMap<String, (u64, String)>)>;
    for _ in 0..4 {
        for tab in [
            "modules",
            "cloud",
            "magic",
            "local",
            "library",
            "module-uploader",
        ] {
            s.tab = tab.into();
            if tab == "module-uploader" {
                s.module_detail = "missing-module".into();
            }
            let t = tree(&s);
            let ids = identities(&t);
            if let Some((page, old)) = &previous {
                if page != tab {
                    let old_ids: HashSet<_> = old.values().map(|v| v.0).collect();
                    assert!(
                        !ids.values().any(|v| old_ids.contains(&v.0)),
                        "nodes leaked from {page} to {tab}"
                    );
                }
            }
            assert_eq!(has_text(&t, "专属模块进度"), tab == "modules");
            assert_eq!(
                has_text(&t, "专属歌曲进度"),
                matches!(tab, "local" | "cloud" | "library")
            );
            previous = Some((tab.into(), ids));
            s.song_job.as_mut().unwrap().received += 100;
        }
    }
}
#[test]
fn conditional_sections_do_not_change_existing_controls_or_input_identity() {
    crate::magic::test_view(Default::default());
    let mut s = fixture();
    s.tab = "local".into();
    let first = tree(&s);
    let upload = event_node(&first, "upload").id;
    let options = event_node(&first, "upload-options").id;
    s.local_path = "data/song.mp3".into();
    s.local_name = "测试歌曲".into();
    s.upload_options = true;
    s.cover_path = "cover.png".into();
    s.status_owner = state::NoticeOwner::Local;
    s.status = "已选择".into();
    let selected = tree(&s);
    assert_eq!(event_node(&selected, "upload").id, upload);
    assert_eq!(event_node(&selected, "upload-options").id, options);
    let name = event_node(&selected, "name:data/song.mp3").id;
    s.upload_options = false;
    s.status.clear();
    assert_eq!(event_node(&tree(&s), "name:data/song.mp3").id, name);
    s.local_path = "data/another.mp3".into();
    assert_ne!(event_node(&tree(&s), "name:data/another.mp3").id, name);
}
#[test]
fn list_reordering_and_progress_do_not_move_another_module_or_song() {
    crate::magic::test_view(Default::default());
    let mut s = fixture();
    let first = tree(&s);
    let music = event_node(&first, "module-toggle:bluetooth_audio").id;
    let magic = event_node(&first, "module-delete:com.chem.magic").id;
    s.modules.reverse();
    s.safe_mode = true;
    s.module_message("bluetooth_audio", "NoirMusic", "正在安装");
    s.installing = true;
    s.install_id = "bluetooth_audio".into();
    s.install_total = 100;
    s.install_sent = 20;
    let progress = tree(&s);
    assert_eq!(
        event_node(&progress, "module-toggle:bluetooth_audio").id,
        music
    );
    assert_eq!(
        event_node(&progress, "module-delete:com.chem.magic").id,
        magic
    );
    s.tab = "cloud".into();
    s.active = false;
    let initial = tree(&s);
    let second_song = event_node(&initial, "cloud:8").id;
    s.results.reverse();
    s.results[1].cover_url = "https://example.invalid/cover.png".into();
    s.song_job = Some(state::SongJob {
        id: 7,
        network: true,
        total: Some(100),
        ..Default::default()
    });
    assert_eq!(event_node(&tree(&s), "cloud:8").id, second_song);
}
#[test]
fn late_notices_stay_on_the_origin_page() {
    crate::magic::test_view(Default::default());
    let mut s = fixture();
    for (owner, pages) in [
        (state::NoticeOwner::Modules, vec!["modules"]),
        (state::NoticeOwner::Cloud, vec!["cloud"]),
        (state::NoticeOwner::Local, vec!["local"]),
        (state::NoticeOwner::Library, vec!["library"]),
        (state::NoticeOwner::Magic, vec!["magic"]),
        (state::NoticeOwner::Music, vec!["local", "cloud", "library"]),
    ] {
        s.notice(owner, "延迟完成的异步操作");
        for tab in ["modules", "local", "cloud", "library", "magic"] {
            s.tab = tab.into();
            assert_eq!(
                has_text(&tree(&s), "延迟完成的异步操作"),
                pages.contains(&tab),
                "{owner:?} on {tab}"
            );
        }
    }
}
#[test]
fn same_named_magic_packs_have_separate_stable_controls_after_removal() {
    let mut s = fixture();
    s.tab = "magic".into();
    let mut view = crate::magic::View {
        rows: ["alpha", "beta"]
            .into_iter()
            .map(|id| crate::magic::Pack {
                id: id.into(),
                name: "同名资源包".into(),
                enabled: false,
            })
            .collect(),
        ..Default::default()
    };
    crate::magic::test_view(view.clone());
    let initial = tree(&s);
    let beta = event_node(&initial, "magic-toggle:beta").id;
    assert_eq!(pack_index(&view, "beta"), Ok(1));
    view.rows.remove(0);
    crate::magic::test_view(view.clone());
    assert_eq!(event_node(&tree(&s), "magic-toggle:beta").id, beta);
    assert_eq!(pack_index(&view, "beta"), Ok(0));
    assert!(pack_index(&view, "alpha").is_err());
    crate::magic::test_view(Default::default());
}
#[test]
fn reopening_uses_the_new_host_root_and_does_not_accumulate_children() {
    crate::magic::test_view(Default::default());
    let s = fixture();
    state::with(|current| *current = s);
    ui::RENDERS.with(|renders| renders.borrow_mut().clear());
    render("first-host-root");
    let first = ui::RENDERS.with(|r| r.borrow().last().unwrap().1.clone());
    rerender();
    let refreshed = ui::RENDERS.with(|r| r.borrow().last().unwrap().1.clone());
    assert_eq!(first.id, refreshed.id);
    assert_eq!(nodes(&first).len(), nodes(&refreshed).len());
    render("second-host-root");
    state::with(|s| s.notice(state::NoticeOwner::Cloud, "旧任务刚刚完成"));
    rerender();
    ui::RENDERS.with(|r| {
        let r = r.borrow();
        assert_eq!(r.last().unwrap().0, "second-host-root");
        assert_ne!(first.id, r.last().unwrap().1.id);
        assert!(!has_text(&r.last().unwrap().1, "旧任务刚刚完成"));
    });
}
#[test]
fn clicks_queued_by_an_old_page_are_ignored_even_after_returning_to_it() {
    crate::magic::test_view(Default::default());
    let mut s = fixture();
    let first = tree(&s);
    let old = event_node(&first, "module-toggle:bluetooth_audio").events[0].1.clone();
    assert_eq!(visible_event(&old, &s), Some("module-toggle:bluetooth_audio"));
    s.install_sent = 10;
    tree(&s);
    assert_eq!(visible_event(&old, &s), Some("module-toggle:bluetooth_audio"));
    s.tab = "cloud".into();
    assert!(visible_event(&old, &s).is_none());
    tree(&s);
    s.tab = "modules".into();
    tree(&s);
    assert!(visible_event(&old, &s).is_none());
}
#[test]
fn export_layout_fixtures_when_requested() {
    let Some(path) = std::env::var_os("CHEM_UI_FIXTURES") else {
        return;
    };
    let mut fixtures = Vec::new();
    let mut s = fixture();
    crate::magic::test_view(Default::default());
    for tab in ["modules", "local", "cloud", "library", "magic"] {
        s.tab = tab.into();
        fixtures.push(json!({"name":tab,"tree":tree(&s)}));
    }
    s.tab = "modules".into();
    s.safe_mode = true;
    s.installing = true;
    s.install_id = "bluetooth_audio".into();
    s.install_total = 100;
    s.install_sent = 50;
    fixtures.push(json!({"name":"module-progress","tree":tree(&s)}));
    s.tab = "cloud".into();
    s.active = true;
    s.song_job = Some(state::SongJob {
        id: 7,
        network: true,
        total: Some(100),
        received: 25,
        message: "下载中".into(),
        ..Default::default()
    });
    fixtures.push(json!({"name":"song-progress","tree":tree(&s)}));
    s.tab = "local".into();
    s.active = false;
    s.song_job = None;
    s.upload_options = true;
    s.local_path = "data/a-very-long-song-filename-to-check-the-mobile-layout.mp3".into();
    fixtures.push(json!({"name":"local-options","tree":tree(&s)}));
    std::fs::write(path, serde_json::to_vec(&fixtures).unwrap()).unwrap();
}
