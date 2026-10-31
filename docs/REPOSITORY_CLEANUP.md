# ChemClient 仓库整理 · 2026-10-31

项目保留原 Git 历史并迁为独立 ChemClient 目录，不再作为 NoirMusic 内嵌仓库。插件不依赖 Chem SDK；运行协议与现有 NoirMusic 保持兼容。

新目录 release 构建成功，API 4 / WASI 2 组件、manifest、PNG、additional_files 及根 index.txt 检查通过。正式发布目录 dist/ 仅保留 manifest.json、chemclient.wasm、icon.png；没有重新生成 ABP，旧本地安装包已移到忽略的 out/。

发布仓库：https://github.com/OctopusCat101/ChemClient 。当前修改只在本机保存，不自动推送 GitHub 或申请上架。

源码卫生检查、协作说明和 CI 已更新；不携带固件、私钥、个人路径或编译缓存。原生项目的正式私钥按所有者要求在私有 Chem 仓库保存，不进入此插件仓库。

## 当前发布文件 SHA256

- `manifest.json`：`6833fd39d605686ba345334d82d592abf4eb71c0a90759ed87fcc4f37cde69b4`
- `chemclient.wasm`：`bba9bc2eb04b4439ab1e00b474fd3fb38fc6c4fd19ab2bf655241d8474a2c252`
- `icon.png`：`b06bfe99dd71e7ce36a4ad22cb86e739fdf4050b86a6279020cb1c00a19f2d7f`
