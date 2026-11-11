# Modem-Reset 2.0

针对 **中兴 ZTE F612** 光猫（192.168.1.1）和 **小米路由器**（192.168.31.1）的重启工具。
网络掉线时双击托盘图标就能救场，不再需要装 Python。

## 现状

- 设备型号：ZTE F612
- Web 服务：ZTE 自研嵌入式 web server（`Server: Mini web server`）
- 页面编码：GB2312（程序按字节抓 ASCII 字段，不需要 GB 编码表）
- 导航机制：所有后台页面统一走 `getpage.gch?pid=XXXX`
- 登录：`POST /`，字段 `action=login`、`Frm_Logintoken`、`Username`、`Password`
- 重启：`POST /getpage.gch?pid=1002&nextpage=manager_dev_conf_t_user.gch`，
  字段 `IF_ACTION=devrestart`、`flag=1`，另需本次会话的 `_SESSION_TOKEN`（每次重新获取）
- 路由器：`POST /cgi-bin/luci/api/xqsystem/login` 拿 `stok`，再 `POST .../api/xqsystem/reboot`

## 构建

需要 Rust 工具链（`rustup` + MSVC）。产物是单个 exe，**不依赖运行时、不依赖 Python**。

```powershell
.\build.ps1          # 产出 dist\ModemReset.exe + dist\modem.ini
.\build.ps1 -Run     # 构建完直接启动托盘
```

`build.ps1` 还会按 `CARGO_HOME` / `RUSTUP_HOME` 做路径重映射，把这两个目录在产物里显示成
`/cargo` 和 `/rustup`，避免本机路径被 panic 位置字符串带进 exe。直接 `cargo build` 不经过
这一步，功能不受影响，只是产物里会保留本机绝对路径。

体积 **0.67 MB**（687,104 字节；`opt-level="z"` + LTO + strip + `panic="abort"`，
HTTP 只编 http 不带 TLS，正则用 `regex-lite`）。其中图标资源约 **22 KB**。

体积主要花在这几处，都是刻意选过的：

| 部分 | 体积 | 说明 |
| --- | --- | --- |
| `ureq` 3（HTTP） | ≈ 225 KB | 已关 TLS/gzip，`http` / `ureq-proto` 一起编进来；换手写客户端还能再省一截，但要自己兜超时/重定向/chunked |
| std + 静态 CRT | ≈ 200 KB | 地板价，再压只能上 nightly `build-std` |
| 我们自己的代码 + `windows-sys` | ≈ 100 KB | 托盘、面板、DPI、ini、光猫/路由逻辑 |
| `regex-lite` | ≈ 55 KB | 用完整 `regex` 的话是 1.1 MB |
| 图标 + 版本信息 | 22 KB | 见下 |

> `ureq` 2.x 会连带拉进 `url` → `idna` → `icu_*`（Unicode 归一化表），仅这部分就
> 占 ~80 KB 只读数据。升到 3.5 后这条依赖链整个消失，exe 从 742,400 降到
> 687,104 字节。相应地 4xx/5xx 用 `http_status_as_error(false)` 显式保留为「响应」
> （光猫重启时常见 502 和空响应，要交给调用方判断），重定向仍限 5 跳。

> 早期版本用过完整的 `regex` crate：仅它一个就编进来 1.1 MB（自带 Unicode 表和
> DFA）。我们的 pattern 全是 ASCII 关键字，换成 `regex-lite` 后 exe 从 1.92 MB 降到
> 1.06 MB，两者对同一批 pattern 做过差分测试（9 个 pattern × 18 组输入，结果一致）。
> 副作用：`regex-lite` 没有 `regex::escape`，`zte.rs` 里自己实现了一个。

CRT 静态链接（`.cargo/config.toml` 里的 `+crt-static`）：产物不依赖
`VCRUNTIME140.dll` / VC++ 运行库，导入表里 10 个 DLL 全部由 Windows 自带，
干净装好的 Win10/11 拷过去双击就能跑。代价约 +90 KB。

高 DPI：进程启动即声明 **Per-Monitor V2** DPI 感知（`SetProcessDpiAwarenessContext`，
失败时依次退回 8.1 的 shcore 接口和 Vista 的 `SetProcessDPIAware`），所以
125%/150%/200% 缩放下不会被系统位图拉伸；面板尺寸、按钮和字体按所在显示器的
DPI 换算，拖到另一块缩放不同的屏幕上会收到 `WM_DPICHANGED` 重新布局；
托盘图标按 `SM_CXSMICON` 的实际像素尺寸渲染。

图标：`src/raster.rs` 用纯 Rust 画出「电源符号」，同一份代码被两处复用 ——
`build.rs` 生成 16/24/32/48/64/128/256 七档的 `app.ico` 并用 `rc.exe` 编进 exe
（资源管理器、任务栏、Alt+Tab 看到的都是它），`icon.rs` 在运行时生成同款 HICON
给托盘。改图标只需要改 `raster.rs`，两边不会走样。

`.ico` 里 16/24/32 存 32bpp BMP，48 及以上存 **PNG 帧**（ICO 从 Vista 起允许，
Win7+ 全档位可用）。这一项很划算：256px 的裸 DIB 是 270 KB，PNG 后只剩 6 KB，
整个图标资源从 365 KB 降到 22 KB。`png` crate 只在 `build.rs` 里用，
不进 exe，所以运行时零代价。

> 图标资源需要 Windows SDK 的 `rc.exe`（装了 VS Build Tools / Windows SDK 就有）。
> `embed-resource` 会自己从注册表找 SDK 路径，不必加进 `PATH`。
>
> 想改图标：只动 `src/raster.rs`，然后跑
>
> ```powershell
> cargo run --release --example preview
> powershell -ExecutionPolicy Bypass -File examples\preview.ps1
> ```
>
> 会把 16/24/32/48/64/128/256（外加 20/40）各档导出到 `target/icon-preview/`，
> 并拼成放大 8 倍的对比图 `sheet.png`，小尺寸有没有糊掉一眼就能看出来。

## 运行环境依赖

**不需要随包分发任何 DLL**，也不要求装 .NET / Python / VC++ 运行库。
`+crt-static` 打开后，exe 的导入表只剩 10 个 Windows 自带 DLL（共 191 个符号）：

| DLL | 符号数 | 用途 |
| --- | --- | --- |
| `kernel32.dll` | 118 | Rust std 的线程/文件/控制台/堆，以及 CRT 内部（静态链接后并进来） |
| `user32.dll` | 31 | 托盘、菜单、窗口、消息框、DPI |
| `ws2_32.dll` | 18 | TCP 探测设备是否恢复（`wait_online`）+ HTTP |
| `gdi32.dll` | 7 | 图标 DIB、字体 |
| `ntdll.dll` | 5 | std 的匿名管道、文件 IO |
| `advapi32.dll` | 5 | 开机自启的注册表读写 |
| `api-ms-win-core-synch-l1-2-0.dll` | 3 | `WaitOnAddress` 等 std 同步原语 |
| `api-ms-win-shcore-scaling-l1-1-1.dll` | 2 | DPI 兜底路径 |
| `bcryptprimitives.dll` | 1 | `ProcessPrng`（std 给 HashMap 播种） |
| `shell32.dll` | 1 | `Shell_NotifyIconW` |

导入是**静态解析**（没有 delay-load），缺任何一个 DLL 进程都起不来
（`0xc0000135`），不会跑一半才崩。

实际门槛是系统版本而不是 DLL：`ProcessPrng`、`GetDpiForSystem` 要 Win10，
`SetProcessDpiAwarenessContext` 要 Win10 1703+，`WaitOnAddress` 要 Win8+。
所以支持范围是 **Windows 10 / 11 x64**（Win7 不支持；这点比 1.x 的 Python 版窄）。

## 配置

配置是 `modem.ini`，放在 exe 同目录。格式约定：

- `[section]` 分段，`key = value` 赋值
- `;` 或 `#` **整行**才是注释（不支持行尾注释，URL 里的 `= ? ;` 会被误伤）
- 只按**第一个** `=` 切分，所以 `url` 不用转义
- 查找不区分大小写，但 `[reboot.fields]` 的键名保留原样（那是要发给光猫的字段名）
- 环境变量优先级最高：`ONT_PASS` / `ROUTER_PASS` / `ONT_HOST` / `ONT_REBOOT_URL` …

```ini
[ont]
host      = 192.168.1.1
username  = useradmin
password  = 你的光猫密码
timeout   = 8
retries   = 3

[router]
host      = 192.168.31.1
password  = 小米路由管理密码

[reboot]
url       = /getpage.gch?pid=1002&nextpage=manager_dev_conf_t_user.gch
method    = POST
confirmed = yes        ; 安全闸门，必须显式改 yes

[reboot.fields]
IF_ACTION = devrestart
flag      = 1
```

密码目前**明文**存放在 ini 里（和 1.x 时期一致）。如果要避免落盘，就用环境变量：

```powershell
setx ONT_PASS "光猫密码"      # 之后新启动的进程才生效
setx ROUTER_PASS "路由密码"
```

> `modem.ini` 已写进 `.gitignore`，别把它提交上去。

## 用法

**日常：双击 `ModemReset.exe`** —— 托盘常驻，再双击一次会弹出三按钮面板：

```
[ 重启路由器（小米）  ]
[ 重启光猫（ZTE F612）]
[       关闭          ]
```

右键托盘图标是完整菜单：

```
上次：路由器已重新上线 19:42:10
────────────────────────
重启路由器（小米）
重启光猫（ZTE F612）
────────────────────────
打开配置文件
打开日志
开机自启  √
────────────────────────
退出
```

**命令行**（给快捷方式、计划任务、看门狗用）：

```powershell
ModemReset.exe ont                    # 光猫：预演，只打印将要发的请求
ModemReset.exe ont --execute          # 真正重启光猫
ModemReset.exe router --execute -y    # 重启路由器，跳过交互确认
ModemReset.exe ont --execute -c D:\tools\modem.ini
```

`-c/--config` 对托盘模式同样有效：`ModemReset.exe -c D:\tools\modem.ini`（不带命令）启动后，
托盘重启和菜单里的「打开配置文件」都会用这份 ini。这种启动方式不会再挂控制台，
所以关掉命令行窗口不会把托盘一起结束。

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 2 | 设备不可达 / 读不到重启页 |
| 3 | 登录失败（密码错或账号被锁） |
| 4 | 尚未配置重启接口 |
| 5 | 缺少密码，或 `confirmed` 未确认 |
| 6 | 命令已发出，但等待时间内未恢复 |

## 安全约定

重启有三道闸门，缺一不可：

1. 必须显式 `--execute`，否则只打印请求内容
2. `modem.ini` 里 `[reboot] confirmed = yes`
3. 托盘点击 / 命令行默认都有二次确认（`--yes` 可跳过，仅供托盘与计划任务）

其它安全细节：

- `_SESSION_TOKEN` 与登录会话绑定，程序每次执行前先 GET 重启页重新获取，**不写死在配置里**
- 登录失败次数过多时，光猫会弹锁定倒计时页；程序识别到 `setDisable` 会立刻中止，不重试
- 日志写 `modem-reset.log`（与 exe 同目录），超过 1 MB 自动轮转一份 `.log.1`

## 目录结构

```
Modem-Reset/
  Cargo.toml
  .cargo/config.toml    静态链接 CRT（去掉 VCRUNTIME140 依赖）
  build.rs              生成 app.ico（大档 PNG）+ app.rc，把图标和版本信息编进 exe
  build.ps1             一键构建，产物进 dist\
  modem.ini.example     配置模板
  src/
    main.rs             入口、命令行、重启流程
    config.rs           modem.ini 解析
    httpc.rs            带 Cookie / 重试的 HTTP 客户端（ureq 3 薄封装）
    zte.rs              光猫登录、表单解析、token 抓取
    xiaomi.rs           小米路由器登录与重启
    tray.rs             托盘、菜单、三按钮面板、DPI 布局
    raster.rs           纯 Rust 图标光栅化 + 单档 DIB 封装（构建/运行时共用）
    icon.rs             把画好的像素转成运行时 HICON
    autostart.rs        开机自启（HKCU\...\Run）
    sys.rs              消息框、控制台附着、DPI 感知、打开文件
    log.rs              日志与滚动
  examples/
    preview.rs          图标预览：把各档尺寸导出成裸像素，调画法时用
```

## 网络拓扑

```
运营商 CGN  100.64.0.1
   ↓
100.64.0.10           ZTE F612 光猫（WAN，CGN 内网地址）
   ↓ 192.168.1.0/24
192.168.31.1          小米路由器（WAN=192.168.1.x, LAN=192.168.31.0/24）
   ↓
本机                   192.168.31.100
```
