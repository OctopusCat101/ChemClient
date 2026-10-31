# ChemClient 协作

本项目独立构建和发布，不依赖 Chem/NoirMusic 的本地目录。源码、WIT 和解码器分别保存在 src/、wit/、third_party/；构建缓存为 out/，上架资源为 dist/，根目录 index.txt 保持 dist。

提交前运行 `python scripts/check-repository.py` 和 `python build.py --check`。修改 Rust 源码后按 README 构建，保留 API 4/WASI 2 约束。公开插件仓库不保存设备固件、签名私钥、本机路径或日志；保留第三方许可。
