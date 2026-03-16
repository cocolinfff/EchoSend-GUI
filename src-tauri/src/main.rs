#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::VecDeque,
    env,
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, LOCATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};
use wait_timeout::ChildExt;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::ZipArchive;

const GITHUB_API: &str = "https://api.github.com/repos/cocolinfff/EchoSend/releases/latest";
const GITHUB_LATEST: &str = "https://github.com/cocolinfff/EchoSend/releases/latest";
const GITHUB_LATEST_DOWNLOAD: &str = "https://github.com/cocolinfff/EchoSend/releases/latest/download";
const MAX_LOG_LINES: usize = 800;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StartupConfig {
    daemon_args: Vec<String>,
    auto_start_daemon: bool,
    refresh_seconds: u64,
    launch_minimized_to_tray: bool,
    close_to_tray: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            daemon_args: vec![],
            auto_start_daemon: false,
            refresh_seconds: 3,
            launch_minimized_to_tray: false,
            close_to_tray: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct KernelInfo {
    exists: bool,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
struct AppSnapshot {
    kernel: KernelInfo,
    kernel_version: Option<String>,
    daemon_running: bool,
    startup_config: StartupConfig,
    peers: Option<u32>,
    autostart_enabled: bool,
    context_menu_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HistoryPayload {
    messages: Vec<Value>,
    files: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct UpdateInfo {
    current_version: Option<String>,
    latest_version: Option<String>,
    has_update: bool,
}

struct AppState {
    daemon: Mutex<Option<Child>>,
    logs: Arc<Mutex<VecDeque<String>>>,
    startup: Arc<Mutex<StartupConfig>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            daemon: Mutex::new(None),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            startup: Arc::new(Mutex::new(StartupConfig::default())),
        }
    }
}

fn push_log(logs: &Arc<Mutex<VecDeque<String>>>, line: impl Into<String>) {
    let mut guard = logs.lock().expect("logs lock poisoned");
    guard.push_back(line.into());
    while guard.len() > MAX_LOG_LINES {
        guard.pop_front();
    }
}

fn app_config_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("app config dir error: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("create config dir failed: {e}"))?;
    Ok(dir.join("startup-config.json"))
}

fn kernel_storage_path(_app: &AppHandle) -> Result<PathBuf, String> {
    let dir = if let Ok(exe) = env::current_exe() {
        exe.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "cannot resolve executable directory".to_string())?
    } else {
        env::current_dir().map_err(|e| format!("current dir error: {e}"))?
    };

    fs::create_dir_all(&dir).map_err(|e| format!("create kernel dir failed: {e}"))?;
    #[cfg(target_os = "windows")]
    let file_name = "echosend.exe";
    #[cfg(not(target_os = "windows"))]
    let file_name = "echosend";
    Ok(dir.join(file_name))
}

fn which(binary_name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var)
        .map(|p| p.join(binary_name))
        .find(|candidate| candidate.is_file())
}

fn find_kernel(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let current_exe = env::current_exe().ok();

    if let Ok(path) = kernel_storage_path(app) {
        candidates.push(path);
    }

    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("echosend.exe"));
            candidates.push(dir.join("echosend-win64.exe"));
            candidates.push(dir.join("echosend-win32.exe"));
            candidates.push(dir.join("echosend"));
        }
    }

    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join("echosend.exe"));
        candidates.push(resource_dir.join("echosend-win64.exe"));
        candidates.push(resource_dir.join("echosend-win32.exe"));
        candidates.push(resource_dir.join("echosend"));
    }

    if let Some(p) = candidates.into_iter().find(|p| {
        if !p.is_file() {
            return false;
        }
        if let Some(current) = &current_exe {
            if let (Ok(a), Ok(b)) = (fs::canonicalize(p), fs::canonicalize(current)) {
                if a == b {
                    return false;
                }
            }
        }
        true
    }) {
        return Some(p);
    }

    which("echosend.exe").or_else(|| which("echosend"))
}

fn run_kernel(app: &AppHandle, args: &[String], timeout_secs: u64) -> Result<String, String> {
    let kernel = find_kernel(app).ok_or_else(|| "kernel not found".to_string())?;
    let mut cmd = Command::new(kernel);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

    let timeout = Duration::from_secs(timeout_secs.max(1));
    match child.wait_timeout(timeout).map_err(|e| e.to_string())? {
        Some(_status) => {
            let output = child.wait_with_output().map_err(|e| e.to_string())?;
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                Err(if !stderr.is_empty() { stderr } else { stdout })
            }
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err("kernel command timeout".to_string())
        }
    }
}

fn choose_asset_name(release: &Value) -> Option<(String, String)> {
    let assets = release.get("assets")?.as_array()?;
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    let mut best_score: i32 = -1;
    let mut best: Option<(String, String)> = None;

    for item in assets {
        let name = item.get("name")?.as_str()?.to_lowercase();
        let url = item.get("browser_download_url")?.as_str()?.to_string();

        #[cfg(target_os = "windows")]
        if !(name.ends_with(".exe") || name.ends_with(".zip")) {
            continue;
        }
        #[cfg(not(target_os = "windows"))]
        if name.ends_with(".exe") {
            continue;
        }

        let mut score = 0;
        if name.contains("echosend") {
            score += 20;
        }
        if name.contains(os) {
            score += 30;
        }
        if os == "windows" && (name.contains("win")) {
            score += 18;
        }
        if arch == "x86_64" && (name.contains("x64") || name.contains("amd64") || name.contains("64")) {
            score += 12;
        }
        if arch == "x86" && (name.contains("x86") || name.contains("32")) {
            score += 12;
        }
        if arch == "aarch64" && (name.contains("arm64") || name.contains("aarch64")) {
            score += 12;
        }
        if name.ends_with(".zip") {
            score += 3;
        }

        if score > best_score {
            best_score = score;
            best = Some((name, url));
        }
    }

    best
}

fn normalize_version(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Accept formats like: "v1.2.3", "1.2.3", "EchoSend 1.2.3", "EchoSend version v1.2.3".
    for token in trimmed.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '(' || c == ')' || c == ':') {
        let t = token.trim().trim_matches('"').trim_matches('`').trim();
        if t.is_empty() {
            continue;
        }
        let t = t.trim_start_matches('v').trim_start_matches('V');
        if t.chars().any(|c| c.is_ascii_digit()) && t.contains('.') {
            let cleaned = t
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-' && c != '+')
                .to_string();
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }

    String::new()
}

fn current_kernel_version(app: &AppHandle) -> Option<String> {
    let out = run_kernel(app, &["--version".to_string()], 4).ok()?;
    let first = out.lines().next().unwrap_or("").trim();
    let ver = normalize_version(first);
    if ver.is_empty() {
        None
    } else {
        Some(ver)
    }
}

fn latest_release_tag() -> Option<String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;

    let resp = client
        .get(GITHUB_LATEST)
        .header(USER_AGENT, "EchoSend-Tauri")
        .send()
        .ok()?;

    let location = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let marker = "/releases/tag/";
    let idx = location.find(marker)?;
    let tag = &location[idx + marker.len()..];
    let tag = tag.trim_matches('/');
    let ver = normalize_version(tag);

    if ver.is_empty() {
        None
    } else {
        Some(ver)
    }
}

fn direct_asset_candidates() -> Vec<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    let mut names = Vec::new();

    if os == "windows" {
        if arch == "x86_64" {
            names.extend([
                "echosend-win64.exe",
                "echosend-windows-64.exe",
                "echosend-x64.exe",
                "echosend.exe",
                "echosend-win64.zip",
                "echosend-windows-64.zip",
            ]);
        } else if arch == "x86" {
            names.extend([
                "echosend-win32.exe",
                "echosend-windows-32.exe",
                "echosend-x86.exe",
                "echosend-win32.zip",
                "echosend-windows-32.zip",
            ]);
        } else if arch == "aarch64" {
            names.extend([
                "echosend-win-arm64.exe",
                "echosend-windows-arm64.exe",
                "echosend-win-arm64.zip",
            ]);
        }
    } else if os == "linux" {
        if arch == "x86_64" {
            names.extend([
                "echosend-linux-amd64",
                "echosend-linux-x64",
                "echosend-linux-64",
                "echosend-linux-amd64.zip",
            ]);
        } else if arch == "aarch64" {
            names.extend([
                "echosend-linux-arm64",
                "echosend-linux-aarch64",
                "echosend-linux-arm64.zip",
            ]);
        }
    } else if os == "macos" {
        if arch == "x86_64" {
            names.extend([
                "echosend-macos-amd64",
                "echosend-darwin-amd64",
                "echosend-macos-amd64.zip",
            ]);
        } else if arch == "aarch64" {
            names.extend([
                "echosend-macos-arm64",
                "echosend-darwin-arm64",
                "echosend-macos-arm64.zip",
            ]);
        }
    }

    names
        .into_iter()
        .map(|name| format!("{GITHUB_LATEST_DOWNLOAD}/{name}"))
        .collect()
}

fn read_startup_config(app: &AppHandle) -> StartupConfig {
    let cfg_file = match app_config_file(app) {
        Ok(v) => v,
        Err(_) => return StartupConfig::default(),
    };
    let text = match fs::read_to_string(cfg_file) {
        Ok(v) => v,
        Err(_) => return StartupConfig::default(),
    };
    serde_json::from_str::<StartupConfig>(&text).unwrap_or_default()
}

fn write_startup_config(app: &AppHandle, cfg: &StartupConfig) -> Result<(), String> {
    let cfg_file = app_config_file(app)?;
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(cfg_file, text).map_err(|e| e.to_string())
}

fn parse_peers(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.to_lowercase().starts_with("peers") {
            trimmed
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse::<u32>().ok())
        } else {
            None
        }
    })
}

fn write_kernel_binary(dst: &Path, file_name: &str, bytes: &[u8]) -> Result<(), String> {
    if file_name.ends_with(".zip") {
        let reader = std::io::Cursor::new(bytes);
        let mut archive = ZipArchive::new(reader).map_err(|e| format!("open zip failed: {e}"))?;

        #[cfg(target_os = "windows")]
        let candidates = ["echosend.exe", "echosend-win64.exe", "echosend-win32.exe"];
        #[cfg(not(target_os = "windows"))]
        let candidates = ["echosend", "echosend-linux", "echosend-macos"];

        let mut found: Option<usize> = None;
        for idx in 0..archive.len() {
            let file = archive
                .by_index(idx)
                .map_err(|e| format!("read zip entry failed: {e}"))?;
            let lower = file.name().to_lowercase();
            if candidates.iter().any(|n| lower.ends_with(n)) {
                found = Some(idx);
                break;
            }
        }

        let idx = found.ok_or_else(|| "no kernel binary found in zip asset".to_string())?;
        let mut kernel_entry = archive
            .by_index(idx)
            .map_err(|e| format!("open kernel entry failed: {e}"))?;

        let mut out = fs::File::create(dst).map_err(|e| e.to_string())?;
        std::io::copy(&mut kernel_entry, &mut out).map_err(|e| e.to_string())?;
        return Ok(());
    }

    fs::write(dst, bytes).map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
fn run_reg(args: &[&str]) -> Result<(), String> {
    let output = Command::new("reg")
        .args(args)
        .output()
        .map_err(|e| format!("reg command failed: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(target_os = "windows")]
fn set_autostart(enable: bool) -> Result<(), String> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    if enable {
        let exe = env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?;
        let value = format!("\"{}\" --minimized", exe.display());
        run_reg(&["add", key, "/v", "EchoSend", "/t", "REG_SZ", "/d", &value, "/f"])
    } else {
        run_reg(&["delete", key, "/v", "EchoSend", "/f"])
    }
}

#[cfg(not(target_os = "windows"))]
fn set_autostart(_enable: bool) -> Result<(), String> {
    Err("autostart registration currently implemented for Windows only".to_string())
}

#[cfg(target_os = "windows")]
fn is_autostart_enabled() -> bool {
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "EchoSend",
        ])
        .output();
    output.map(|o| o.status.success()).unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn is_autostart_enabled() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn set_context_menu(enable: bool) -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?;
    let cmd = format!("\"{}\" --send-path \"%1\"", exe.display());

    let file_root = r"HKCU\Software\Classes\*\shell\EchoSend";
    let dir_root = r"HKCU\Software\Classes\Directory\shell\EchoSend";

    if enable {
        run_reg(&["add", file_root, "/ve", "/d", "Send by EchoSend", "/f"])?;
        run_reg(&[
            "add",
            &format!("{}\\command", file_root),
            "/ve",
            "/d",
            &cmd,
            "/f",
        ])?;
        run_reg(&["add", dir_root, "/ve", "/d", "Send by EchoSend (zip)", "/f"])?;
        run_reg(&[
            "add",
            &format!("{}\\command", dir_root),
            "/ve",
            "/d",
            &cmd,
            "/f",
        ])
    } else {
        let _ = run_reg(&["delete", file_root, "/f"]);
        let _ = run_reg(&["delete", dir_root, "/f"]);
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn set_context_menu(_enable: bool) -> Result<(), String> {
    Err("context menu registration currently implemented for Windows only".to_string())
}

#[cfg(target_os = "windows")]
fn is_context_menu_enabled() -> bool {
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Classes\*\shell\EchoSend",
        ])
        .output();
    output.map(|o| o.status.success()).unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn is_context_menu_enabled() -> bool {
    false
}

fn zip_directory(input: &Path) -> Result<PathBuf, String> {
    let parent = env::temp_dir();
    let file_name = input
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("echosend-folder");
    let archive = parent.join(format!("{file_name}.zip"));

    let file = fs::File::create(&archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for entry in WalkDir::new(input) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let rel = path.strip_prefix(input).map_err(|e| e.to_string())?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        if path.is_dir() {
            if !rel_str.is_empty() {
                zip.add_directory(rel_str, opts).map_err(|e| e.to_string())?;
            }
            continue;
        }

        zip.start_file(rel_str, opts).map_err(|e| e.to_string())?;
        let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        zip.write_all(&buf).map_err(|e| e.to_string())?;
    }

    zip.finish().map_err(|e| e.to_string())?;
    Ok(archive)
}

fn send_path_via_kernel(app: &AppHandle, raw_path: &Path) -> Result<(), String> {
    let send_path = if raw_path.is_dir() {
        zip_directory(raw_path)?
    } else {
        raw_path.to_path_buf()
    };

    run_kernel(
        app,
        &["--send".to_string(), "-f".to_string(), send_path.display().to_string()],
        30,
    )
    .map(|_| ())
}

fn terminate_daemon_process(app: &AppHandle, state: &AppState) -> bool {
    let mut guard = match state.daemon.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };

    if let Some(child) = guard.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
        push_log(&state.logs, "[GUI] daemon stopped");
        app.emit("daemon-status", "stopped").ok();
        *guard = None;
        true
    } else {
        false
    }
}

#[tauri::command]
fn get_snapshot(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let kernel = find_kernel(&app);
    let kernel_version = if kernel.is_some() {
        current_kernel_version(&app)
    } else {
        None
    };
    let daemon_running = state
        .daemon
        .lock()
        .map_err(|_| "daemon lock poisoned".to_string())?
        .as_ref()
        .map(|p| p.id() > 0)
        .unwrap_or(false);

    let cfg = state
        .startup
        .lock()
        .map_err(|_| "startup lock poisoned".to_string())?
        .clone();

    let peers = run_kernel(&app, &["status".to_string()], 5)
        .ok()
        .and_then(|out| parse_peers(&out));

    Ok(AppSnapshot {
        kernel: KernelInfo {
            exists: kernel.is_some(),
            path: kernel
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "".to_string()),
        },
        kernel_version,
        daemon_running,
        startup_config: cfg,
        peers,
        autostart_enabled: is_autostart_enabled(),
        context_menu_enabled: is_context_menu_enabled(),
    })
}

#[tauri::command]
fn get_daemon_logs(state: State<'_, AppState>, limit: Option<usize>) -> Result<Vec<String>, String> {
    let limit = limit.unwrap_or(200).max(1);
    let logs = state.logs.lock().map_err(|_| "logs lock poisoned".to_string())?;
    let len = logs.len();
    let start = len.saturating_sub(limit);
    Ok(logs.iter().skip(start).cloned().collect())
}

#[tauri::command]
fn download_latest_kernel(app: AppHandle) -> Result<String, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let dst = kernel_storage_path(&app)?;
    let tmp = dst.with_extension("tmp");

    for url in direct_asset_candidates() {
        let resp = client
            .get(&url)
            .header(USER_AGENT, "EchoSend-Tauri")
            .send();

        let Ok(resp) = resp else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }

        let bytes = match resp.bytes() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let name = url.rsplit('/').next().unwrap_or("echosend.bin").to_string();
        write_kernel_binary(&tmp, &name, &bytes)?;
        fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dst).map_err(|e| e.to_string())?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dst, perms).map_err(|e| e.to_string())?;
        }

        let latest = latest_release_tag().unwrap_or_else(|| "latest".to_string());
        return Ok(format!("kernel updated to {latest} (direct link)"));
    }

    let token = env::var("GITHUB_TOKEN").ok();

    let mut req = client
        .get(GITHUB_API)
        .header(USER_AGENT, "EchoSend-Tauri");
    if let Some(t) = token {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }

    let release: Value = req
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let (name, url) = choose_asset_name(&release).ok_or_else(|| {
        "no compatible kernel asset found, please check release asset names".to_string()
    })?;

    let bytes = client
        .get(url)
        .header(USER_AGENT, "EchoSend-Tauri")
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .bytes()
        .map_err(|e| e.to_string())?;

    write_kernel_binary(&tmp, &name, &bytes)?;
    fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dst).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dst, perms).map_err(|e| e.to_string())?;
    }

    Ok(format!("kernel updated to {tag}"))
}

#[tauri::command]
async fn check_kernel_update(app: AppHandle) -> Result<UpdateInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let current = current_kernel_version(&app);
        let latest = latest_release_tag();

        let has_update = match (&current, &latest) {
            (Some(c), Some(l)) => c != l,
            (None, Some(_)) => true,
            _ => false,
        };

        Ok(UpdateInfo {
            current_version: current,
            latest_version: latest,
            has_update,
        })
    })
    .await
    .map_err(|e| format!("check update task failed: {e}"))?
}

#[tauri::command]
fn start_daemon(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    {
        let mut guard = state
            .daemon
            .lock()
            .map_err(|_| "daemon lock poisoned".to_string())?;
        if let Some(existing) = guard.as_mut() {
            match existing.try_wait() {
                Ok(None) => return Ok("daemon already running".to_string()),
                Ok(Some(_)) | Err(_) => {
                    *guard = None;
                }
            }
        }
    }

    let kernel = find_kernel(&app).ok_or_else(|| "kernel not found".to_string())?;
    let cfg = state
        .startup
        .lock()
        .map_err(|_| "startup lock poisoned".to_string())?
        .clone();

    let mut args = vec!["daemon".to_string()];
    args.extend(cfg.daemon_args);

    let mut cmd = Command::new(kernel);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let mut child = cmd.spawn().map_err(|e| format!("start daemon failed: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let logs = state.logs.clone();

    push_log(&logs, "[GUI] daemon start requested");

    if let Some(out) = stdout {
        let logs_clone = logs.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(out);
            for line in reader.lines().map_while(Result::ok) {
                push_log(&logs_clone, line);
            }
        });
    }

    if let Some(err) = stderr {
        let logs_clone = logs.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(err);
            for line in reader.lines().map_while(Result::ok) {
                push_log(&logs_clone, line);
            }
        });
    }

    *state
        .daemon
        .lock()
        .map_err(|_| "daemon lock poisoned".to_string())? = Some(child);

    app.emit("daemon-status", "running").ok();
    Ok("daemon started".to_string())
}

#[tauri::command]
fn stop_daemon(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    if terminate_daemon_process(&app, state.inner()) {
        Ok("daemon stopped".to_string())
    } else {
        Ok("daemon is not running".to_string())
    }
}

#[tauri::command]
fn get_history(app: AppHandle) -> Result<HistoryPayload, String> {
    let out = run_kernel(&app, &["--history".to_string(), "--json".to_string()], 12)?;
    let parsed: Value = serde_json::from_str(&out).map_err(|e| format!("invalid history json: {e}"))?;

    if let Some(data) = parsed.get("data") {
        let messages = data
            .get("messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let files = data
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        return Ok(HistoryPayload { messages, files });
    }

    let messages = parsed
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let files = parsed
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(HistoryPayload { messages, files })
}

#[tauri::command]
fn get_peers(app: AppHandle) -> Result<Option<u32>, String> {
    let out = run_kernel(&app, &["status".to_string()], 5)?;
    Ok(parse_peers(&out))
}

#[tauri::command]
fn send_message(app: AppHandle, text: String) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    run_kernel(
        &app,
        &["--send".to_string(), "-m".to_string(), text],
        10,
    )
    .map(|_| ())
}

#[tauri::command]
fn send_files(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    for raw in paths {
        let path = PathBuf::from(raw);
        if !path.exists() {
            continue;
        }
        send_path_via_kernel(&app, &path)?;
    }
    Ok(())
}

#[tauri::command]
fn pull_file(app: AppHandle, file_hash: String) -> Result<(), String> {
    if file_hash.trim().is_empty() {
        return Ok(());
    }
    run_kernel(&app, &["--pull".to_string(), file_hash], 20).map(|_| ())
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err("path not found".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", &p.display().to_string()])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(&p)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&p)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[tauri::command]
fn reveal_in_folder(path: String) -> Result<(), String> {
    let p = PathBuf::from(path);
    if !p.exists() {
        return Err("path not found".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("explorer")
            .args(["/select,", &p.display().to_string()])
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        open_path(
            p.parent()
                .map(|v| v.display().to_string())
                .unwrap_or_else(|| p.display().to_string()),
        )
    }
}

#[tauri::command]
fn get_startup_config(state: State<'_, AppState>) -> Result<StartupConfig, String> {
    Ok(state
        .startup
        .lock()
        .map_err(|_| "startup lock poisoned".to_string())?
        .clone())
}

#[tauri::command]
fn save_startup_config(app: AppHandle, state: State<'_, AppState>, config: StartupConfig) -> Result<(), String> {
    write_startup_config(&app, &config)?;
    *state
        .startup
        .lock()
        .map_err(|_| "startup lock poisoned".to_string())? = config;
    Ok(())
}

#[tauri::command]
fn set_autostart_enabled(enable: bool) -> Result<(), String> {
    set_autostart(enable)
}

#[tauri::command]
fn set_context_menu_enabled(enable: bool) -> Result<(), String> {
    set_context_menu(enable)
}

#[tauri::command]
fn hide_to_tray(window: tauri::Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[tauri::command]
fn show_main_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())
}

#[tauri::command]
fn quit_app(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let _ = terminate_daemon_process(&app, state.inner());
    app.exit(0);
    Ok(())
}

fn setup_tray(app: &mut tauri::App) -> Result<(), String> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>).map_err(|e| e.to_string())?;
    let menu = Menu::with_items(app, &[&show, &quit]).map_err(|e| e.to_string())?;

    TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "quit" => {
                let state = app.state::<AppState>();
                let _ = terminate_daemon_process(app, state.inner());
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(win) = tray.app_handle().get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
        })
        .icon(
            app.default_window_icon()
                .ok_or_else(|| "missing default icon".to_string())?
                .clone(),
        )
        .build(app)
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn init_context_send(app: AppHandle) -> bool {
    let mut args = env::args();
    while let Some(arg) = args.next() {
        if arg == "--send-path" {
            if let Some(path) = args.next() {
                let path_buf = PathBuf::from(path);
                if path_buf.exists() {
                    let _ = send_path_via_kernel(&app, &path_buf);
                    return true;
                }
            }
            return false;
        }
    }
    false
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
            if let Err(e) = setup_tray(app) {
                eprintln!("tray setup skipped: {e}");
            }

            let handle = app.handle().clone();
            let state = handle.state::<AppState>();
            let cfg = read_startup_config(&handle);
            *state
                .startup
                .lock()
                .map_err(|_| "startup lock poisoned".to_string())? = cfg.clone();

            // Only hide on explicit --minimized launches (e.g. autostart),
            // so manual double-click always opens a visible window.
            let minimized = env::args().any(|a| a == "--minimized");
            if minimized {
                if let Some(win) = handle.get_webview_window("main") {
                    let _ = win.hide();
                }
            }

            if init_context_send(handle.clone()) {
                handle.exit(0);
                return Ok(());
            }

            if cfg.auto_start_daemon {
                let _ = start_daemon(handle.clone(), state);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let close_to_tray = window
                    .app_handle()
                    .state::<AppState>()
                    .startup
                    .lock()
                    .map(|c| c.close_to_tray)
                    .unwrap_or(true);

                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    let state = window.app_handle().state::<AppState>();
                    let _ = terminate_daemon_process(&window.app_handle(), state.inner());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            check_kernel_update,
            get_daemon_logs,
            download_latest_kernel,
            start_daemon,
            stop_daemon,
            get_history,
            get_peers,
            send_message,
            send_files,
            pull_file,
            open_path,
            reveal_in_folder,
            get_startup_config,
            save_startup_config,
            set_autostart_enabled,
            set_context_menu_enabled,
            hide_to_tray,
            show_main_window,
            quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri app");
}
