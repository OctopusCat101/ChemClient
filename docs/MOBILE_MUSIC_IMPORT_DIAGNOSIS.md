# 手机端音乐导入失败诊断：连接探测、压缩窗口与传输层参数

日期：2026-11-15。反馈来源：手机端大量「蓝牙上传有问题」，PC 端不复现。

三条关键线索（用户补充）：

1. 不选压缩（原样传输）能正常导入；**一选压缩就失败**。
2. 有人**导入到一半提示「手环已断开」**。
3. 有人**音乐下载完的一瞬间就中止**，根本没进入上传。

本文只做静态代码复核 + 官方文档比对，未修改任何源码。行号对应当前工作区（`git` 干净，`manifest.json` 1.7.36）。

---

## 0. 结论摘要

三个症状出自**同一个缺陷族**：导入流程把「宿主此刻没报告手环」等同于「手环物理断开」，并且这个判断在**任务进行中仍然照常执行、且零容错**。手机端让这个判断频繁为假，PC 端几乎永不为假。

| 编号 | 缺陷 | 位置 | 破坏力 | 手机命中率 |
|---|---|---|---|---|
| F1 | 单次空列表/超时即判定断开，无连续失败计数、无宽限期 | `connection.rs:259-297` | 直接中止任务并删文件 | 高 |
| F2 | 探测写在忙碌门**之前**，任何进行中的导入都不受保护 | `connection.rs:42-70` | 让 F1 能打到传输中 | 高 |
| F3 | 压缩窗口无连接维护、管理通道超时会把连接判死且无法自愈 | `modules.rs:253-269`、`connection.rs:335-346` | 压缩白做、报「请先连接」 | 高（仅压缩路径） |
| F4 | 16 KiB 单帧 + 5 s 固定发送超时 + 失败即中止 | `transfer.rs:172,186-192` | 整首从头再来 | 中高 |
| F5 | 30 s 超时按「命令发起时刻」而非「最后进展」计 | `transfer.rs:201,245-263` | 慢链路必超时 | 中高 |
| F6 | 每 20 次轮询重发整个 16 KiB 帧 | `transfer.rs:218-223` | 慢链路自我拥塞 | 中 |
| F7 | 订阅/定时器句柄永久缓存，不随重连失效 | `transfer.rs:66-79,120-141` | 静默收不到 ACK、进度卡死 | 中 |
| F8 | 两条路径可并发 `transport::send`，offset 账本可能错位 | `transfer.rs:169-194` vs `330-348` | 「手环写入位置不匹配」 | 低中 |
| F9 | 失败路径破坏性清理：删刚下完的歌、upload 泄漏、CRC 提前推进 | `netease.rs:214-222`、`transfer.rs:544-564,591` | 无法重试、永久 `busy` | 中 |
| F10 | 转码期进度用源文件字节、结束后归零 | `transcode.rs:303`、`transfer.rs:530-541` | 体感「卡死」 | 高（仅压缩路径） |

**一句话**：压缩把任务从「秒级」拉到「分钟级」，把 F1/F2 的暴露窗口放大了 100 倍；而失败时的清理逻辑又让用户必须从零重来。这就是「不压缩没事、压缩就崩」的全部原因。

---

## 1. 先看平台差异（为什么 PC 一定不中）

| 维度 | 桌面端 | 手机端 | 代码里被写死的假设 |
|---|---|---|---|
| 设备列表查询 | 毫秒级、稳态 | 数百 ms～数秒；后台/锁屏/大量 BLE 写入时可能返回空 | `connection.rs:261` 用 5 s 上限并**把超时当断开** |
| BLE 有效吞吐 | 20–60 KB/s | 1–5 KB/s（含设备写 flash 等待） | `protocol.rs:3` 单帧 16 KiB；`transfer.rs:172` 固定 5 s |
| 链路持续性 | 常连不掉 | 锁屏/切后台/系统回收 BLE 属常态 | 无重连宽限、无任务期保护 |
| 执行引擎 | Cranelift **JIT** | Android JIT；**iOS Pulley64 解释执行** | 纯 CPU 的 MP3 编码耗时差一个数量级 |
| 定时器 | 连续触发 | 后台挂起，回前台集中补触发 | `Instant` 差值一次跨阈值 → 直接判超时 |
| 插件内存 | 128 MiB | 128 MiB | 已用流式读写，此项无问题 |

官方文档原文（[运行环境](https://abox.run/docs/plugin-development/v2/framework/runtime)）：

> Windows / macOS / Linux / Android：使用 **Cranelift 引擎** 以 **JIT 模式** 执行。
> iOS：使用 **Pulley64 引擎** 以 **解释模式（Interpreter）** 执行。
> 每个插件的最大内存占用：**128 MiB**

---

## 2. F1/F2：一次探测失败就判死，而且探测发生在任务进行中

这是三条症状的共同父节点，先说清楚。

### 2.1 探测在忙碌门之前

```rust
// connection.rs:40-70
pub async fn tick() {
    let s = state::snapshot();
    if !s.addr.is_empty() && !s.discovering {
        let check = { /* last.devices.due(Duration::from_secs(2)) → true */ };
        if check {
            device_event().await;                      // ← 每 2 秒，无条件
        }
    }
    let s = state::snapshot();
    if s.active || s.installing || s.discovering || s.manager_busy
        || state::action_busy() || transfer::busy() {
        return;                                        // ← 忙碌门在探测之后
    }
```

`chem-connection` 定时器周期 500 ms（`lib.rs:38`），`RetryClock::due` 保证每 2 s 进一次 `device_event`（`retry.rs:13-15`）。

**结论**：上传中（`s.active`）、下载中（`action_busy`）、压缩中（`s.active`）都**照样**被探测。忙碌门只挡住了后面的 `refresh_background` / `connect_device`，挡不住探测本身。

> 复核修正：我上一轮称 `try_action()` 是死代码、`action_busy()` 恒为 false。**这是错的。** `ui.rs:1639` 有实际调用，`cloud:<id>`、`upload` 等动作全程持锁，所以下载期间 `action_busy()` 为 true，服务分支确实被挡住。漏洞只剩「探测先于忙碌门」这一条，但这一条足以致命。

### 2.2 探测本身零容错

```rust
// connection.rs:259-298
let devices = match bounded(5000, device::get_connected_device_list()).await {
    Ok(devices) => devices,
    Err(error) => {                                  // ① 宿主调用 5 s 超时
        if state::snapshot().connection_epoch == current.connection_epoch {
            disconnect(vec![]);                      // 直接判死，并把设备列表清空
            failed(error);
        }
        return;
    }
};
if devices.iter().any(|device| device.addr == current.addr) {
    /* 只更新列表，顺带在空闲时补一次重连 */
    return;
}
disconnect(devices...); refresh(false).await;         // ② 列表里没有当前设备 / 列表为空
```

三处设计缺陷叠在一起：

1. **超时 = 断开**。`bounded()` 超时返回的是 `"连接请求超时，将自动重试"`（`deadline.rs:14`），语义是「还没拿到结果」，不是「设备不在了」。
2. **没有迟滞**。没有「连续 N 次为空才算断开」，一次异常采样即生效。
3. **一次判定就清空 `connected_devices`**（`invalidate_device` 里 `s.connected_devices = devices`，传入的是 `vec![]`），于是 `device_connected()`、`manager_connected()`、`music_connected()` 连锁为假。

### 2.3 判定之后立刻做破坏性清理

```rust
// connection.rs:321-333
fn disconnect(devices: Vec<(String, String)>) {
    state::with(|s| invalidate_device(s, devices));   // epoch++ / direct=false / Phase::Failed
    crate::cloud_download::cancel(true);
    modules::reset_connection();
    crate::bootstrap::reset();
    crate::magic::disconnected();
    if crate::delivery::busy() { crate::delivery::fail(...); }
    crate::transcode::cancel();                        // 压缩任务当场销毁
    transfer::disconnected();                          // 上传当场失败 + 删临时文件
    ui::rerender();
}
```

`invalidate_device`（`connection.rs:300-319`）还会顺手给正在下载的任务打上不可撤销的取消标记：

```rust
if let Some(job) = s.song_job.as_mut().filter(|j| j.network) {
    job.cancel_requested = true;                       // 只置位，全代码库无清除点
    job.message = "手环已断开，正在停止导入".into();
}
```

---

## 3. 三个症状分别对应哪条链路

### 症状 A：导入到一半提示「手环已断开」

```
connection::tick (500 ms)
 └─ device_event()                          ← 上传中照样跑
     └─ get_connected_device_list() → 空 / 超 5 s
         └─ disconnect()
             └─ transfer::disconnected()
                 └─ fail("手环已断开，导入未完成，请重新连接后重试")   // transfer.rs:82
                     └─ cleanup_upload() → 删临时音频（transfer.rs:604-615）
```

手机端为何高频命中：此刻宿主正在发 16 KiB 大包并等设备写 flash，再并发一次设备列表查询，Android 返回空数组的比例显著上升；`bounded(5000)` 与 BLE 写超时同量级，很容易先触发。

### 症状 B：音乐下载完的一瞬间就中止

下载阶段 `job.network == true`，任何一次假断开都会写入 `cancel_requested`。`netease::download` 的进度回调每 200 ms 检查一次（`netease.rs:581`），下载立刻中断；即便下载刚好已经收完，`ui.rs:522-531` 的状态门也会把它判死：

```rust
// ui.rs:521-536
let current = state::snapshot();
let cancelled = current.song_job.as_ref().is_none_or(|j| j.cancel_requested);
if cancelled
    || current.addr != s.addr
    || current.connection_epoch != s.connection_epoch
    || !current.music_connected()
{
    return Err(if cancelled { "已取消下载" } else { "手环连接已变化，请重新导入" }.into());
}
```

失败返回时 `prepared` 还没被 `into_path()` 取走，`Drop` 立刻删文件：

```rust
// netease.rs:214-222
impl Drop for PreparedCloud {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);       // 刚下完的 MP3 直接没了
            if let Some(parent) = Path::new(&self.path).parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}
```

用户感知：进度条冲到 100% → 卡片瞬间变成失败文案 → 文件已删 → 什么都没导入。

> 复核修正：上一轮我说这里会「页面跳回模块页」。**只有条件成立时才会**：`apply_devices` 的空分支确实会把 `s.tab` 改成 `modules`（`connection.rs:143-166`），但受守卫的下载过程中 `refresh → begin_check` 会被 `action_busy` 挡住（`connection.rs:335-346`），所以「跳 Tab」只在动作结束后的那次 refresh 里、且设备列表仍为空时发生。它不是必然现象，不应作为主要判据。

### 症状 C：压缩路径必失败（用户最确定的一条）

压缩期间 `s.active = true`（`transcode.rs:145-152`），`connection::tick` 的服务分支被挡住 → 管理会话不再保活、也不会重连。此时有两条独立的死法：

**C-1 直接被 `disconnect()` 干掉（最常见）**

压缩是 30 s ～ 数分钟的长任务，假断开落在窗口内的概率与时长成正比。一旦命中，`disconnect()` 调 `transcode::cancel()`（`transcode.rs:246-263`）：

```rust
fn dispose(j: Job, remove_output: bool) { ... }   // remove_output=true → 删输出 + 删源
state::with(|s| {
    s.transcoding = false;
    s.active = false;
    s.status = "已取消压缩".into();
    if let Some(job) = &mut s.song_job { job.message = s.status.clone(); }
});
```

用户看到：压缩到 40%～70% 突然「已取消压缩」，什么都没了。

**C-2 管理通道超时把连接判死，压缩成果被丢弃**

这条是我上一轮没说准的地方，重新推导如下。

```rust
// modules.rs:253-269
fn fail(error: String) {
    link(|l| { l.pending = None; l.rows.clear(); });
    state::with(|s| {
        s.manager_busy = false;
        s.manager_ready = false;      // ← 管理器判死
        s.direct = false;             // ← 直连判死
    });
    ...
    connection::failed(error);        // ← Phase::Failed
}
```

`modules::tick`（25 ms `chem-manager` 定时器，`lib.rs:61`）在 pending 超时时调用它（`modules.rs:363-391`，op 9 → 3 s，其余 10 s）。此刻 `refresh` 被 `s.active` 挡住（`connection.rs:336`），状态**卡在 Failed 无法自愈**。

压缩结束后：

```rust
// transcode.rs:318-337
state::with(|s| { s.transcoding = false; s.active = false; });
match result {
    Ok(_) => {
        if let Err(e) = transfer::start(path.clone(), song, true).await {
            let _ = fs::remove_file(path);          // ← 压缩产物当场删除
            state::error(e);
        }
    }
    ...
}
// transfer.rs:451-453
if !state::snapshot().music_connected() { return Err("请先连接原生音乐模块".into()); }
```

`music_connected()` 要求 `manager_ready && Phase::Ready && direct && 存在可导入模块`（`state.rs:140`），任一为假即失败。用户看到「请先连接原生音乐模块」，而手环其实一直连着——这句文案正是投诉里最令人困惑的一条。

**为什么原样路径不会中**：`transcode::begin` 在 `quality == 0` 时直接同步进入传输（`transcode.rs:65-70`），中间没有窗口：

```rust
if quality == 0 {
    if predecode { return sbc::start(path, song, temporary); }
    return transfer::start(path, song, temporary).await;   // 同一个调用栈，链路是热的
}
```

---

## 4. 传输层本身的问题（F4–F8）

### F4 16 KiB + 5 s + 失败即中止

```rust
// transfer.rs:171-193
let result = connection::bounded(5000, transport::send(addr, protocol::encode(&frame))).await...;
if let Err(e) = result {
    if current {
        let message = format!("发送失败：{e}");
        fail(message.clone());        // ← 直接 fail：cleanup_upload + state::error
        return Err(message);
    }
}
```

`protocol::CHUNK = 16 * 1024`（`protocol.rs:3`）。手机链路上单块常需 3–16 s，标题文案却是「将自动重试」，而代码里**没有任何重试**。

### F5 30 s 是绝对截止，不是空闲计时

```rust
// transfer.rs:201-203
if p.since.elapsed().as_secs() >= if l.command == 1 { 3 } else { 30 } {
    return Some(Err("手环响应超时，请确认启用了新版蓝牙音乐模块".to_string()));
}
```

`since` 只在 `send_pending` 里 `p.polls == 0` 时复位一次（`transfer.rs:181-183`）。设备回 `status == 1`（处理中）直接 `return None`（`transfer.rs:261-263`），回 `-2` 只补发不刷新计时（`transfer.rs:245-255`）。所以 30 s 是**从第一次发送算起的绝对截止**，和设备是否还在写 flash 无关。超时文案还把责任推给模块版本。

### F6 慢链路上的自我拥塞

```rust
// transfer.rs:218-223
if p.polls % 20 == 0 && p.since.elapsed().as_secs() >= u64::from(p.attempts) * 2 {
    p.attempts = p.attempts.saturating_add(1);
    Some(Ok((p.id, p.frame.clone())))          // 重发整个 16 KiB
} else {
    Some(Ok((p.id, protocol::frame(p.id, 0, &[]))))   // 12 字节轮询
}
```

轮询间隔 `25/50/100 ms`（`transfer.rs:206-210`），20 次约 0.5–2 s。手机上一块要 3–6 s 才 ACK，于是每 2 s、4 s、6 s 各重发一整帧，把 1–5 KB/s 的链路塞进重复数据，ACK 更慢，正反馈。PC 上 ACK 100–200 ms，该分支永不触发。

### F7 订阅与定时器句柄永不失效

```rust
// transfer.rs:120-141
if !link(|l| l.registered_addrs.iter().any(|a| a == addr)) { /* 只注册一次 */ }
let has_timer = link(|l| l.timer_id != 0);        // 非 0 就永不重建
if !has_timer { link(|l| l.timer_id = timer::set_interval(25, "music-link")); }

// transfer.rs:66-79
pub fn reset_connection() { link(|l| { l.pending = None; l.list_offset = 0; }); /* 两个缓存都不动 */ }
```

官方文档明确：`set-interval` 持续触发直到 `clear-timer` **或插件实例被销毁**（[Timer 接口](https://abox.run/docs/plugin-development/v2/host-api/timer)）。把 `timer_id` 当永久句柄，与这个生命周期不匹配。手机端锁屏后宿主重建 BLE 链路，旧订阅不再投递，插件却认为「已注册」→ 永久收不到 ACK → 30 s 后超时。

### F8 两条路径可并发发送

`received()` 的 op=3 / op=4 分支都会调 `send_next()`（`transfer.rs:330,346`），`tick()` 也会 `send_pending()`（`transfer.rs:226-229`）。`send_command` 用 `l.pending.is_some()` 做闸门（`transfer.rs:150-152`），但 `received` 路径是**先清掉 pending 再发下一块**，若此刻 `tick` 已取走旧帧在 `await`，就有两个 `transport::send` 同时在飞（代码 `transfer.rs:169-170` 的注释也承认这点）。两帧到达序错乱 → 设备按到达顺序写 → offset 核对失败 → `手环写入位置不匹配`（`transfer.rs:336`）。手机上一次 send 数秒，窗口极大。

---

## 5. 失败路径的次生伤害（F9）

| 位置 | 问题 |
|---|---|
| `transfer.rs:544-564` | `l.upload = Some(...)` 先置位，再 `send_command(3, ...)`；发送失败直接返回 `Err`，**不回滚 upload** → `transfer::busy()` 恒为 true → 之后每次导入都提示「请先完成当前导入」，只能重连恢复 |
| `transfer.rs:591` | CRC 在读块时就累加；现在没炸**只因为发送失败会立刻 `fail()` 中止**。一旦按 F4 放宽超时，同一块会被重读重发 → CRC 重复累加 → 设备端校验失败。改 F4 必须同批改这里（CRC 推迟到 op=4 ACK 确认后提交） |
| `netease.rs:214-222` | 下载完成但状态门失败 → 删除已下载音频，用户必须重新下载 |
| `transcode.rs:334` | 压缩完成但传输入口拒绝 → 删除压缩产物，几十秒到几分钟白算 |
| `state.rs:187-199` | `state::error()` 只改文案，不清 `transcode::JOB`/`transfer::upload` 之外的状态 |

另有 F10（纯体感问题，不影响正确性）：压缩期进度写的是**源文件已读字节**（`transcode.rs:303` `s.sent = j.bytes; s.total = j.size`），压缩完成后 `begin_upload` 又归零（`transfer.rs:531-533`）。手机上压缩慢，用户先看到进度冲到 100% 停住，再归零重来——这正是「卡死」投诉的来源之一。`ui.rs:2074` 的 `mp3_duration::from_path` 还在 UI 事件里同步扫全文件，大文件会额外卡一下界面。

---

## 6. 文案字典：问用户看到哪句话，就能定位到哪一段

| 界面文案 | 失败落在 | 代码 |
|---|---|---|
| `手环已断开，导入未完成，请重新连接后重试` | 上传中 | `transfer.rs:82` |
| `已取消压缩` | 压缩中 | `transcode.rs:257` |
| `手环已断开，正在停止导入` / `已取消下载` | 网易云下载中 | `connection.rs:315-318` / `netease.rs:551` |
| `手环连接已变化，请重新导入` | 下载完、转码前 | `ui.rs:522-531` |
| `请先连接原生音乐模块` | 压缩后入口 | `transfer.rs:452` |
| `请先连接 NoirMusic` | 压缩入口 / 曲库 | `transcode.rs:72` / `transfer.rs:431` |
| `请先完成当前导入` / `请先完成当前操作` | upload 泄漏 | `transcode.rs:75` / `transfer.rs:449` |
| `发送失败：连接请求超时，将自动重试` | 首帧 5 s | `transfer.rs:188` |
| `手环响应超时，请确认启用了新版蓝牙音乐模块` | 传输中 30 s | `transfer.rs:202` |
| `手环写入位置不匹配` | 并发发送 | `transfer.rs:336` |
| `请先在 Astro 连接手环，然后刷新。` | 设备列表为空 | `connection.rs:165` |

建议下个版本把所有 `fail()` 出口都带一个稳定错误码（如 `E-DEV-01`），让用户反馈一句话就能归因。

---

## 7. 与官方文档的三条硬约束

| 文档 | 原文 | 对本项目的影响 |
|---|---|---|
| [Transport 接口](https://abox.run/docs/plugin-development/v2/host-api/transport) | 「`transport` 接口已经出现在 WIT 中，但当前宿主实现仍**接近占位状态**。`send` / `request` 目前**不要当作稳定可用的数据通道**」 | 这条路径本身无稳定性承诺。插件侧只能把失败做成**可恢复**，不能指望「一次成功」 |
| 同上 | `send`「仅发送数据，不等待响应」 | 5 s 上限约束的是**交给宿主**，不是设备 ACK。也因此不能把 send 成功当作写入成功 |
| [运行环境](https://abox.run/docs/plugin-development/v2/framework/runtime) | iOS = Pulley64 **解释执行**；每插件 **128 MiB** | 解释执行直接放大压缩耗时（F3 的窗口）；128 MiB 要求保持流式（现已满足） |
| [Timer 接口](https://abox.run/docs/plugin-development/v2/host-api/timer) | `set-interval` 持续触发直到 `clear-timer` **或插件实例被销毁** | `timer_id` 不能当永久句柄（F7） |

`wit/deps/astrobox-psys-host-v4.wit` 顶部注释另有一条：只有 `async func` 允许阻塞等待宿主。当前所有 `transport::send` 调用都在 async 上下文，**这一项无问题**。

---

## 8. 修复方案

原则：**不碰传输协议本身，只改状态机与文件生命周期**。PC 行为保持不变。

### P0（先做，一次一组，每组跑 `cargo test --locked`）

**P0-1 断开判定加迟滞 + 超时不算断开**

- `connection.rs:259-298`：为 `device_event` 增加连续失败计数（建议 3 次 ≈ 6 s），区分「超时」与「列表为空」两种情况分别计数；单次异常只记录、不清理。
- 真正判定断开时，仍走 `disconnect()`，但 `invalidate_device` 不再把 `connected_devices` 置空，保留地址以支持原地恢复。

**P0-2 任务期间停止轮询**

- `connection.rs:42-70`：把设备轮询移到忙碌门之后；或在 `s.active || transfer::busy() || action_busy() || s.transcoding` 时直接跳过轮询。
- 依据：宿主本来就会投递 `EventType::DeviceAction`（`lib.rs:68-70`），真实断开不依赖轮询发现。

**P0-3 假断开不做破坏性清理**

- `connection.rs:300-319`：改为置 `suspect` 标志，不清 `addr`/`tab`，不置 `cancel_requested`；当前操作跑完后由 `check_pending()` 复核。
- `connection.rs:321-333`：`disconnect()` 里对 `transcode::cancel()` / `transfer::disconnected()` 的调用改为「仅在确认断开时执行」。

**P0-4 压缩结果与下载结果不再被失败路径删除**

- `netease.rs:214-222`：`PreparedCloud` 的连接变化路径不删文件，允许「只重试上传」。
- `transcode.rs:334`：传输入口拒绝时保留产物（改由显式取消/成功提交删除）。

**P0-5 修 CRC 与放宽超时必须同批**

- `transfer.rs:591`：CRC 改成「读块时暂存、op=4 ACK 确认后提交」。
- `transfer.rs:172`：按 op 分级超时（数据块 20 s / 控制帧 8 s / 握手 5 s），**发送超时不再 `fail()`**，交回重发状态机。

### P1

| 改动 | 位置 |
|---|---|
| `reset_connection` / `disconnected` 清空 `registered_addrs` + `timer_id`，用 `connection_epoch` 强制重注册 | `transfer.rs:66-79,120-141` |
| 单飞闸门：同时只允许一个 `transport::send`，多余帧延后给 `tick` | `transfer.rs:169-194` |
| `begin_upload` 在 `send_command` 失败时回滚 `l.upload` | `transfer.rs:544-564` |
| 压缩结束进传输前 `clear_timer` 掉 25 ms 的 `music-link`，传输开始时重建 | `transfer.rs:136-140` |
| `since` 改为 `last_progress`，`status == 1` 与 `-2` 都刷新；超时预算按块大小给 | `transfer.rs:201,245-263` |
| 块大小自适应：16 KiB 超时则减半至 2 KiB，连续快 ACK 再回升 | `transfer.rs:585`（改 offset 账本） |

### P2

- `transcode.rs:303`：转码期进度改用转码百分比，不复用 `s.sent/s.total`。
- `ui.rs:2074`：`mp3_duration::from_path` 的同步全文件扫描移出 UI 线程。
- 上传/压缩期间加「请保持 AstroBox 在前台」提示。

### 需要同步改的测试

`connection.rs:385-412` 的 `disconnect_invalidates_controls_even_while_an_import_is_busy` 目前在断言「导入中也必须立刻清空 + 置取消」。引入迟滞后该断言需要按新语义拆分：一次异常不清空，连续 3 次才清空。

---

## 9. 验证清单

1. `cargo test --locked`：当前 76 项（历史记录）应保持通过；P0-1 会新增迟滞用例。
2. 新增单测：`device_event` 连续 1 次/2 次空列表不改变 `epoch`；第 3 次才改变。
3. 新增单测：`import_cloud_song` 在 `epoch` 变化但文件存在时保留文件。
4. 手机端复现脚本（人工）：
   - 选 24 kbps 压缩，导入一首 3–4 分钟的歌；
   - 中途锁屏 30 s 再回前台；
   - 期望：任务不中止，进度回落后继续；
   - 旧行为：`已取消压缩` / `手环已断开`。
5. 日志埋点建议：在 `device_event` 的三种分支各打一行 `tracing::warn!`（空列表 / 超时 / 确认断开），并在 `fail()` 出口带上错误码。这样现场反馈可以直接归因，不必靠文案猜。

---

## 10. 未决问题

1. 现场无法区分「真实断连」与「采样失败」的比例。需要 P0-2 的埋点数据才能定量。
2. `transport::send` 在 Android 上的实际返回时延没有测量数据；F4 的超时预算目前只能按经验值给。
3. 设备端在收到重复帧（F6 的整帧重发）时的行为未确认：是按 ID 幂等丢弃，还是按到达顺序再写一次？若是后者，F6 也会导致 offset 错位。
4. 压缩期间的 `disconnect()` 会连带杀掉压缩任务——这是有意的设计还是历史遗留？从用户视角看，压缩是纯本地计算，与手环无关，不应被连接变化中断。

## 11. 本地修复记录（2026-11-15）

以上保留原始诊断。以下为实际改动，不能替代手机与手环实测。

| 链路 | 已实现 |
|---|---|
| 设备状态 | 导入/转码期间延后设备列表复核；列表请求超时只记录。空列表需至少 3 次、持续至少 6 秒，且最近 6 秒没有有效协议回复才确认断开；更换连接代次清空旧判定计数。 |
| 宿主发送 | 管理与音乐共享单次发送闸门；排队后重新检查连接代次和请求 ID，避免已过期帧发往新会话。 |
| 超时与重发 | 大帧交给宿主的预算 30 秒、小帧 8 秒；发送异常进入重试。音乐握手无进展 10 秒、其他命令无进展 45 秒，单请求硬上限 180 秒。等待时发小轮询，明确失败/未知 ID 至少隔 2 秒重发，普通整帧重发间隔 12 秒。 |
| 文件与 CRC | 从已确认 offset 读取块；只在匹配 ACK 后提交 offset 和 CRC，重发不重复累计。块从 4 KiB 起步，按响应延迟在 2–16 KiB 内调整。 |
| 成果保留 | 下载完成或主音频上传阶段遇到连接变化，保留文件和元数据；本插件实例内恢复同一设备连接后重新导入，避免重新下载/压缩。显式取消或成功后清理。不是跨插件实例持久化，也不是设备断点续传；附件阶段失败仍按原流程清理。 |
| Magic | 只把 ACTIVE/BOOT_RESIDENT 当作可通信状态；activate 必须成功注册 CONNECT 监听，publish 幂等补注册，失败明确返回。 |
| 手环 10 NFC | 经 IDA 核实 MiWear UI owner 的 loop 与 libuv 入口，首次 UI turn 创建常驻定时器；后续模块交换与管理写操作不再依赖休眠时停止的 LVGL 心跳。禁止在 BLE 接收回调直接执行修改操作。 |

验证：客户端 81 项主机测试通过，含重复读块/CRC 提交、过期发送丢弃、待导入文件保留；Chem 原生链路测试包含停止 LVGL 后的模块交换与幂等写入；Magic 测试接真实 Chem CONNECT 实现验证注册、请求、响应及注销。

仍需设备验证：手机前后台切换、宿主发送返回时延、手动/自动启动后 Magic 通信、深度休眠后的响应。手环 10 NFC 固件为 `o66nfc 3.1.28`，不能据此推断其他手环 10 固件可用。SBC 预读进度与 UI 同步扫描的建议未在本轮全面改写。
