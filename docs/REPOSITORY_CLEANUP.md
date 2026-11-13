# 发布目录整理 · 2026-11-13

ChemClient 1.7.32 的发布文件位于 dist/，根 index.txt 指向 dist。插件仓库独立构建，包含源码、WIT 与第三方许可；不发布固件、私钥、日志或构建缓存。

本地安装包为 dist/ChemClient.abp，供 Astro 手动导入；插件源读取 dist/manifest.json、chemclient.wasm、icon.png 及声明的 cloud.json。ABP 与 out/ 构建目录均被 Git 忽略。

已移除 GitHub Actions 自动工作流，构建、格式检查和回归按需在本地运行。临时参考工程、测试音频产物和下载的测试运行时已移出项目；可重复使用的回归测试源码保留。

发布与跨项目提交统一由相邻 ChemCloud 的 release.py / workspace.py 管理，不把上传工具放回 Chem。

官方 Astro 源已经登记本仓库地址；其聚合索引每四小时刷新一次，推送成功与官方缓存更新是两个独立状态。查看插件页面底部版本号可确认实际运行的版本。
