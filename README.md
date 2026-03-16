# EchoSend GUI (Tauri + Rust)

EchoSend 的桌面图形客户端，基于 Tauri 2 + Rust 后端 + 原生 WebView 前端实现。

当前版本已从旧版 Python/tkinter 方案迁移到 Tauri 技术栈，支持 Windows 开发与打包发布。

## 功能概览

- 内核发现与调用
  - 自动发现当前目录、资源目录或 PATH 中的 EchoSend kernel。
  - GUI 调用 kernel 执行状态查询、消息发送、文件发送、拉取文件等命令。

- 手动检查更新与下载更新
  - 支持检查当前 kernel 版本与最新 release 版本。
  - 更新流程为手动触发，用户确认后才下载。
  - 下载策略优先使用 GitHub latest 直链，失败后回退到 API 资产选择。

- Daemon 管理
  - 一键启动和停止 daemon。
  - 实时采集 daemon stdout/stderr 日志并在 UI 展示。

- 历史记录与状态
  - 拉取并展示消息/文件历史。
  - 支持文件状态展示、打开文件、目录定位、失败重拉。
  - 顶部显示 peers 数量。

- 文件/目录发送
  - 支持多文件发送。
  - 目录发送时自动打包 zip 后发送。
  - 支持拖拽发送。

- 托盘与窗口行为
  - 支持最小化到托盘。
  - 支持关闭到托盘或直接退出（可配置）。
  - 托盘菜单支持显示主窗口与退出。

- 系统集成 (Windows)
  - 注册开机自启。
  - 注册右键菜单（文件/文件夹发送，文件夹自动 zip）。

## 技术栈

- 桌面框架: Tauri 2
- 后端: Rust
- 前端: HTML + CSS + JavaScript
- 关键依赖:
  - tauri
  - reqwest (blocking + rustls)
  - serde / serde_json
  - zip
  - walkdir
  - wait-timeout

## 目录结构

```text
EchoSend-GUI/
├─ ui/                       # 前端页面与脚本
│  ├─ index.html
│  └─ app.js
├─ src-tauri/
│  ├─ src/main.rs            # Rust 后端命令与系统集成逻辑
│  ├─ Cargo.toml
│  └─ tauri.conf.json
├─ setup-tauri-windows.ps1   # Windows 环境初始化脚本
├─ package.json
└─ README.md
```

## 开发运行

```powershell
npm install
npm run tauri:dev
```

## 构建发布

推荐使用 Claude Code 命令：

```text
/build
```

可选：

```text
/build dev
/build release
```

手动命令（等价）：

```powershell
$env:CARGO_BUILD_JOBS='12'
npm run tauri:build
```

常见产物路径:

- src-tauri/target/release/echosend_gui_tauri.exe
- src-tauri/target/release/bundle/msi/EchoSend_0.1.0_x64_en-US.msi
- src-tauri/target/release/bundle/nsis/EchoSend_0.1.0_x64-setup.exe

## 常见问题

- 打包时报错 os error 32 (文件被占用)
  - 先关闭正在运行的 echosend_gui_tauri 或相关进程后重试构建。

- 点击检查更新时界面卡顿
  - 当前实现已将检查更新放到后台线程，若网络较慢会显示 Checking 状态。

- kernel 未找到
  - 将 kernel 可执行文件放到应用可执行文件同目录，或确保 PATH 可访问。

## 说明

- 本项目主实现已是 Tauri 版本。
- 根目录中的旧 Python 文件用于历史兼容参考，不是当前主构建链路。
