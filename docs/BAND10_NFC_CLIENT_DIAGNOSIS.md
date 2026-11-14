# ChemClient 连接小米手环 10 NFC 卡住的诊断

日期：2026-11-14。现象：AstroBox 的 ChemClient 插件连上手环后持续显示
「暂未收到 Chem 服务回复，正在重新连接…」。

结论：**插件的设备检测确实没有适配 Band 10，但这句报错是误导；即使把插件
改对，当前 o66nfc 的 Chem 框架仍然不会应答**。三个问题是叠加的。

---

## 1. 报错来自哪里

`src/modules.rs::tick()`：op=9 管理探测发出后 3 秒未收到 CPC2 响应，
调用 `fail("暂未收到 Chem 服务回复，正在重新连接…")`（原 `modules.rs:391`）。
`fail()` 会先问 `bootstrap::probe_failed()` 是否进入重试态；只有返回 false
时才由 `connection::failed()` 把这条消息显示成 `Phase::Failed`。

## 2. 根因 A：设备识别只认 ChemCloud 里已登记的 target

`src/bootstrap.rs::identify()` 解析 `System.DeviceInfo`（field 2 = 固件版本，
field 4 = 型号），然后在 `cloud::Catalog.targets` 里按
`firmware == … && device_ids contains model` 查找。云端清单来自
`cloud.json` 指向的 ChemCloud `index.json`。

线上 `index.json` 当前只有一个 target：`xiaomi-band-9-3.1.35`
（firmware `3.1.35`，device_ids 为 `n66tc` / `M2346B1` 等）。因此
o66nfc 的 `3.1.28` 必然得到 `Err("暂不支持此手环固件：o66nfc 3.1.28")`。
插件源码与测试里也从未出现过 Band 10 的 target 字符串。

## 3. 根因 B：识别失败让报错被换成「服务无回复」

`bootstrap::start()` 的顺序是：发起 catalog 下载 → `modules::connect_device()`
（立刻发出 op=9 探测）→ 请求 DeviceInfo → `prepare_ready()`。
`prepare_ready()` 要同时拿到 DeviceInfo 和异步下载回来的 catalog 才做识别；
识别失败时直接返回 Err，**不会执行 `wait()`**，于是 `BOOT.waiting` 一直是
`None`。

3 秒后 op=9 超时，`fail()` 调 `probe_failed()`，因为 `waiting` 为 None 而返回
false，于是误导性的「暂未收到 Chem 服务回复」被写到界面上，覆盖了
`catalog_ready()` 原本已经显示的真实错误。换句话说，这句提示既可能表示
服务没起，也可能只是设备没被认出来。

## 4. 根因 C：设备端根本没有管理通道（决定性）

即使识别通过，当前 o66nfc 的 Supervisor 也不会应答插件：

- `manager/service/chem_supervisor_l2.c:6` 只在
  `CHEM_SUP_BAND9_BOOTSTRAP` 下引入 `chem_l2_probe_config.h`；NFC 构建
  （`scripts/build_chem_supervisor.sh:54` 定义
  `CHEM_SUP_BAND10_NFC_BOOTSTRAP=1`）走 else 分支，
  `CHEM_SUP_HAS_L2 = 0`、`CHEM_SUP_HAS_OWNER_PUMP = 0`，整个 L2 传输与
  owner pump 被裁掉。
- `manager/target/band9/chem_manager_bridge_band9.c:5` 的实现整体在
  `#if defined(CHEM_SUP_BAND9_BOOTSTRAP)` 内。NFC 构建编译到 `:137` 的
  空实现 `chem_manager_bridge_start() { return -38; }`，**不订阅 protobuf
  20/8 的 `com.chem.manager`**。该文件仍由 `build_chem_supervisor.sh:168`
  无条件编入所有 target。
- `manager/service/chem_supervisor_platform.c:1189` 在 native install 成功后
  调用它，返回值只写进 `sup_trace`，不视为错误。

插件发送的是 `protocol::encode_package("com.chem.manager", CPC2)`
（`src/modules.rs:11`、`:228`），设备端没有接收者，探测必然超时。
这也解释了为什么上一轮 `.face` 实机「正常启动、无崩溃」却仍连不上：
Supervisor 常驻了，但管理桥从未启动。

---

## 5. 本轮插件改动

| 文件 | 改动 |
|---|---|
| `src/cloud.rs` | 新增 `KnownTarget` / `KNOWN_TARGETS`，内置 `xiaomi-band-10-nfc-3.1.28` 的精确身份（firmware `3.1.28`，`firmware_sha256 = fb726472…d8845`，5 个 device_ids）；`local_targets()` 支持 `data/targets.local.json` 覆盖；`identify_target()` 云端优先、本地回退；`Target.local_only` 标记；`validate()` 拒绝云端清单设置 `local_only` |
| `src/bootstrap.rs` | 识别改走 `identify_target()`；`identification_error` 记录真实原因；`probe_failed()` 在识别失败时保留精确错误而不再报「服务无回复」；`local_only` target 跳过云端安装器检查与下载，并在界面说明 ChemCloud 未登记 |
| 测试 | 新增 4 项：o66nfc 三种型号写法可识别、身份仍精确（错固件/错型号被拒）、云端 target 优先级不变、`local_only` 不得出现在云端清单 |

内置 device_ids：`o66nfc`、`O66NFC`、`miwear.watch.o66nfc`、
`miwear.watch.O66NFC`、`xiaomi.watch.o66nfc`、`M2456B1`。前几个来自固件
字符串（`miwear.watch.o66nfc` 是 `vela_ap.bin` 里实际存在的）；
`M2456B1` 来自实机 `System.DeviceInfo.model` 上报，见第 6 节。
匹配用 `eq_ignore_ascii_case`，因此 `m2456b1` 同样有效。

如果以后遇到其它硬件型号，又不想重新编译插件，可在插件数据目录放
`data/targets.local.json` 追加，格式见
`docs/targets.local.example.json`。本地条目只影响识别，不产生云端下载项。
按仓库证据规范，内置列表只收录设备实际报告过的字符串，不猜测相邻型号。

## 6. 实机反馈与 v1.7.34

用户装了带管理桥的 `.face` 后反馈「这次连接上了」，界面变成
「暂不支持此手环固件：**M2456B1 3.1.28**」。两条信息都很关键：

- `identification_error` 的修复生效了——真实原因不再被「暂未收到 Chem
  服务回复」覆盖，所以能直接读到设备上报值。
- 固件版本 `3.1.28` 与内置 target 完全一致，但 `System.DeviceInfo.model`
  报的是**硬件型号 `M2456B1`**，不是 `o66nfc` 这类代号。手环 9 的清单里
  同样有 `M2346B1` / `M2345B1`，属于同一模式，是 v1.7.33 的遗漏。

v1.7.34 把 `M2456B1` 加入内置 device_ids，并补了针对性单测
（`reported_hardware_model_m2456b1_3_1_28_is_supported`），同时校验
`3.1.35 + M2456B1` 与 `3.1.28 + M2456B2` 仍被拒绝，避免放宽精确身份门禁。

需要注意：**识别修好之前，不能据此断言管理桥已经通过真机验证**。
`prepare_ready()` 只需要 catalog + DeviceInfo 就会做识别，不要求 op 9
成功；而 `probe_failed()` 在识别失败时会保留真实错误，所以即使探测仍超时，
界面也会显示这条型号错误。装上 v1.7.34 后，若模块列表/设备信息能读出来，
才说明 20/8 订阅与 CPC2 回复真的通了。

## 7. Chem 侧：管理桥已补齐（同日后续工作）

同日在 `D:\chem\Chem` 补上了设备端管理通道，详见该仓库
`docs/BAND10_NFC_3128_MANAGER_BRIDGE.md`：

- 新增 `manager/target/band10_nfc/chem_manager_bridge_band10_nfc.c`，
  订阅 protobuf 20/8 并回复 20/9；心跳用 target 专属的 `lv_timer_create`。
- 四个入口用跨固件反汇编等价性恢复：subscribe `0x0C453D4D`、
  reply `0x0C4F5207`、timer create `0x0C327D6D`、timer del `0x0C327ECD`；
  注册表 head `0x200B39C0`。注册表 helper `0x0C4539A8` 与手环 9
  `0x0C47A788` **前 32 字节完全一致**，因此 event / response descriptor
  布局是共享的，不是平移猜测。
- 修正构建脚本链接列表硬编码 `chem_manager_bridge_band9.o`（会静默链入
  遗留的 stub 对象）；build ID 递增到 `0x26111401`，强制安装器重新加载
  ——否则 `/dev/chem` 已存在时会直接跳过加载。
- 产出 `dist/Chem-Band10NFC-3.1.28-ManagerBridge-20261114.face`。

**仍未完成**：L2 / owner pump（NFC 仍是 `CHEM_SUP_HAS_L2=0`）。心跳是
LVGL timer，屏幕熄灭时可能不响应；`native_delivery` / bulk upload 仍不可
用，目前只到「探测 + 模块列表 + 只读查询」。插件的「导入 .chem」按钮在
该能力就绪前保持禁用，界面提示也已据此修正。

实机顺序：装新版 `.face` → **重启手环**（不能省） → 点 Run →
**保持屏幕常亮** → 打开 ChemClient。若提示
`Different framework active. Reboot, then Run this face.`，按重启步骤再来一次。

## 8. 验证

- `cargo test --target x86_64-pc-windows-gnu --locked`：**73 passed / 0 failed**
  （v1.7.34 新增 `reported_hardware_model_m2456b1_3_1_28_is_supported`）
- `scripts/check-repository.py`：PASS（86 files）
- 发行构建：`python build.py --abp` → `PASS: API 4 / WASI 2 component`
- 产物：`dist/ChemClient.abp`（**v1.7.34**，
  sha256 `a796713e…b95e`）、`dist/chemclient.wasm`（
  sha256 `049a2560…f3e`）；wasm 内已确认编入
  `M2456B1`、`xiaomi-band-10-nfc-3.1.28`、`miwear.watch.o66nfc`、
  `data/targets.local.json` 与新提示文字

改动仅在本机 `D:\chem\ChemClient`，未提交、未推送。
