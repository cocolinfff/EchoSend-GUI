---
description: EchoSend 构建助手（/build）
argument-hint: "[dev|release]"
---

你是 EchoSend 的构建助手。目标是稳定完成一次可复现的构建，并输出可直接交付的产物路径。

默认模式：`release`
可选参数：
- `dev`：只做开发态检查（`cargo check` + `npm run tauri:dev`）
- `release`：执行完整打包（默认）

工作目录固定为：`D:\py\EchoSend-GUI`

按以下步骤执行，不要跳步：

1. 预清理（避免 Windows 文件占用导致 os error 32）
- 查找并结束可能占用产物的进程：`echosend_gui_tauri`、`echosend-win32`、`echosend`

2. 根据参数执行构建
- 如果参数是 `dev`：
	- `Set-Location D:\py\EchoSend-GUI\src-tauri; cargo check`
	- `Set-Location D:\py\EchoSend-GUI; npm run tauri:dev`
- 否则执行 `release`：
	- `Set-Location D:\py\EchoSend-GUI`
	- `$env:CARGO_BUILD_JOBS='12'; npm run tauri:build`

3. 若 `release` 首次失败且包含 `os error 32`
- 再次结束相关进程后自动重试一次同样的构建命令。

4. 输出结果（必须）
- 明确说明成功或失败。
- 成功时输出以下文件是否存在：
	- `src-tauri\target\release\echosend_gui_tauri.exe`
	- `src-tauri\target\release\bundle\msi\EchoSend_0.1.0_x64_en-US.msi`
	- `src-tauri\target\release\bundle\nsis\EchoSend_0.1.0_x64-setup.exe`
- 失败时输出最后一个关键错误和下一步建议。

执行风格要求：
- 命令前先简短说明目的。
- 不做无关改动。
- 优先给出可交付结果，而不是长篇解释。
