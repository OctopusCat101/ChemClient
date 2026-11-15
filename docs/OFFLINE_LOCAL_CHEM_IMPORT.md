# 离线可用：本地 `.chem` 导入不再依赖 ChemCloud

日期：2026-11-15。反馈：「连接不上云端时，导入 .chem 按钮点不动」。

## 根因

本地 `.chem` 导入本来不需要云端，但插件把它的前置状态绑在了云端清单上：

1. `bootstrap.rs::start()` 发起 `cloud_download::catalog()`；结果由
   `catalog_ready()` 回调。
2. 旧版 `catalog_ready()` 在 `Err(e)` 分支只写 `cloud_notice` 然后
   **`return Ok(())`**，于是 `Bootstrap::take_ready()` 永远等不到
   `catalog`，`prepare_ready()` 不执行。
3. `prepare_ready()` 才负责写入 `s.target`、`s.firmware_sha256`、
   `s.cloud_modules`、`s.native_delivery`。它们全为空/false。
4. `ui.rs` 的「导入 .chem」按钮条件含 `!s.native_delivery`，
   `connection_blocked("module-import")` 也要求 `manager_connected() &&
   native_delivery` → 按钮禁用。
5. 即使强行点击，`delivery.rs::start()` 还会检查 `s.target.is_empty()`，
   并用 `s.target` / `s.firmware_sha256` 校验 `.chem` 收据
   （`Package::open`），离线时必然失败。

另外 `modules.rs` op=9 的响应里把 `native_delivery` 写成
`!firmware_sha256.is_empty() && capabilities & 1 != 0`，把设备能力位又
和云端派生的哈希绑了一次。

## 改动（插件 1.7.35）

- `bootstrap.rs`
  - `Bootstrap` 新增 `catalog_failed`：区分「清单还在下载」与「清单已明确失败」。
    `take_ready()` 只在 device_info 到达且 catalog 成功或已失败时返回；
    失败时给出**空 Catalog**，让 `cloud::identify_target()` 走内置的
    `KNOWN_TARGETS` / `data/targets.local.json` 精确身份兜底。
  - `catalog_ready(Err)` 不再提前返回，而是标记失败后继续
    `prepare_ready()`；`start()` 中「请求根本没发起成功」的分支同样标记。
  - `prepare_ready()` 里 `native_delivery` 改为只看设备上报的能力位
    `resident_capabilities & 1`。
  - 离线时的 `cloud_notice` 明确写出「云端清单不可用，已改用本地精确身份；
    可直接导入 .chem」。
- `modules.rs`：op=9 的 `native_delivery` 同样解耦云端哈希。
- `ui.rs`：把按钮禁用条件抽成 `chem_import_blocked()`，逻辑不变；
  新增单测覆盖「离线 + 清单读取中」仍可点击、「无原生传输能力」仍禁用。
- 版本 `1.7.34` → `1.7.35`。

**没有放宽安全校验**：`.chem` 收据仍按内置精确身份里的
`target = xiaomi-band-10-nfc-3.1.28` 与
`firmware_sha256 = fb7264…8845` 校验（`Package::open`），设备端也会再校验一次。
未知设备且云端不可用时识别会失败，仍然 fail-closed。

## 离线时哪些功能可用

| 功能 | 离线 |
|---|---|
| 导入本地 `.chem`、模块列表、启用/停用/删除 | 可用（需设备端报告原生传输能力） |
| 资源包 `.magic` 导入、模块控制 | 可用 |
| 云端模块更新（`module-install:*`）、安装器 `.face` 下载 | 不可用，`cloud_modules` 为空，按钮禁用 |
| 云端安装器检查 | 不可用，界面显示说明而不阻断连接 |

## 验证

- `cargo test --target x86_64-pc-windows-gnu`：**75 passed / 0 failed**，
  含新增 `failed_catalog_still_resolves_builtin_identity_for_local_chem` 与
  `offline_catalog_does_not_disable_local_chem_import`；原
  `session_snapshot_connects_without_catalog_and_ignores_stale_replies`
  的断言已改为「能力位决定 `native_delivery`」，不再假设无 catalog 必禁用。
- `python build.py --abp`：`PASS: API 4 / WASI 2 component, manifest, PNG
  and resource layout`；ABP 内 4 个文件与 dist 逐字节一致。

产物（插件版本 `1.7.35`）：

| 文件 | 大小 | SHA-256 |
|---|---|---|
| `dist/ChemClient.abp` | 603,286 B | `33e6ee4fd0acd125371113dbcc3b976da4e550f236884676c45792cc70417e83` |
| `dist/chemclient.wasm` | 1,629,824 B | `ac9ca0e8ca58da523e4250e8611a8512f98c662dda75b446c7a2675cbf3eafb9` |

改动仅在本机工作区，**未 commit、未 push**；未在 AstroBox 宿主实机点击验证。
构建时有一条既有的 `class` / `version_name` 未使用警告，与本次改动无关。
