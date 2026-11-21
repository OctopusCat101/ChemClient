# ChemClient Agent 指令

本仓库是 AstroBox 插件。开发前先读取
`../ChemCloud/docs/AGENT_HANDBOOK.md`；无法访问兄弟仓库时，继续读取本文件、
`README.md`、`docs/UI_RENDERING.md`、`docs/OFFLINE_LOCAL_CHEM_IMPORT.md`、
`docs/MOBILE_MUSIC_IMPORT_DIAGNOSIS.md` 和当前代码。

关键边界：

- 保持 API 4 / WASI 2、`wit-bindgen 0.60.0` 和 AstroBox 2.2.6 Queue/Watchface
  接口。
- 启动时自动发现设备、精确身份和 Chem 服务。连接成功后隐藏连接面板，直接展示
  模块列表；不恢复设备页、多步骤连接或重复刷新按钮。
- `.chem` 直接走 `com.chem.manager` 原生导入。Chem 安装器 `.face` 只由用户点击
  「安装」触发，交给 Queue，成功上传后切换表盘，再等待用户在手环点「启动」。
- 云端不可用时，本地 `.chem` 导入仍可用；未知设备继续 fail-closed。
- 主色为黑底浅薄荷绿 Material Design 3。模块行保持名称加右侧开关、删除、更新；
  纯图标固定尺寸、无动画、无缩放、无闪烁。点击模块进入该模块独立上传器。
- 更新卡片标题必须单行；安装按钮固定约 80 px，窄屏可换行，进度在卡片内。
- UI 节点必须使用页面作用域和业务 ID，异步提示绑定 `NoticeOwner`，旧页事件和
  旧连接 epoch 的回复必须丢弃。
- 音乐上传任务只在 NoirMusic 页面显示，进度在对应歌曲下；不得跳到首页或其他模块。

常规验证：

```powershell
cargo test --locked --target x86_64-pc-windows-msvc -- --test-threads=1
python build.py --abp
python tests/check_network_runtime.py --wasmtime <WASMTIME_48>
```

版本变更同步 `Cargo.toml`、`Cargo.lock`、根 `manifest.json` 和
`dist/manifest.json`，并重新生成 `dist/chemclient.wasm`。`.abp` 保留本地。
不得提交 token、cookies、测试歌曲、构建缓存或 `out/`。

提交前查看 `git status` 和 diff，保留用户已有改动，只暂存本次文件并运行
`git diff --check`。用户未明确要求时不要推送。远端有更新时 fetch 后普通合并，
不使用 force push。
