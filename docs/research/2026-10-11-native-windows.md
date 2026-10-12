# 原生 Windows 支持调查与建议

- 日期：2026-10-11
- 基线：`bd8a24d`，Agent HTTP 主入口与地址发现已提交。
- 状态：第一版实现已写入工作树；原生 Windows 验收与发布尚未执行。
- 目标：普通 Windows 用户可以安装、连接 workspace、运行 Agent/App、转发服务，
  并在机器重启后通过恢复命令重新启动保存的运行意图。

## 建议范围

第一版选择 Windows 11 x64，正式构建使用 `x86_64-pc-windows-msvc`。
保持现有 Proxy、认证、HTTP proxy、服务转发和 Host/Controller 分工。
用户自行安装 Agent、Node/Python 等依赖，用原生命令配置 launch profile。
Windows ARM64、开机无人登录运行、自动登录启动和系统服务可以后续独立评估。
进程管理不提供沙箱或互不信任用户之间的隔离。

## 初始基线证据

在 macOS 上安装了 Rust 的 `x86_64-pc-windows-gnu` 标准库并运行交叉检查：

| 检查 | 结果与含义 |
| --- | --- |
| `cargo check --target x86_64-pc-windows-gnu -p treer-agent-runtime` | 通过；现有 `portable-pty 0.9` 的 Windows ConPTY 后端可用于这一层 |
| 同目标检查 `treer-agent-host` | 编译失败：无条件导入 `tokio::net::{UnixListener, UnixStream}` |
| 同目标检查 `treer-cli` | TLS 依赖 `ring` 构建失败，当前 Mac 缺少 `x86_64-w64-mingw32-gcc`；尚未到达 CLI 完整编译，不代表 CLI 的 Windows 实现已通过 |

这些是交叉编译检查，不是原生 Windows 运行证据。还需要 Windows MSVC runner
构建三个可执行文件，并在 Windows 上验证生命周期、网络和安装。

## 源码缺口与最小改造

| 边界 | 当前情况 | 建议 |
| --- | --- | --- |
| Host IPC | [Host](../../crates/treer-agent-host/src/main.rs) 和 [HostClient](../../crates/treer-agent-server/src/host_client.rs) 直接使用 Unix socket | 封装平台本地字节流：Unix 保留 socket，Windows 使用 Tokio Named Pipe；复用 Host framing、请求 ID、事件流和幂等语义 |
| 用户目录与身份 | [service.rs](../../crates/treer-agent-server/src/service.rs) 使用 HOME/XDG、`id -u`、Unix socket 路径；[CLI](../../crates/treer-cli/src/main.rs) 也依赖 HOME/XDG 找配置 | Windows 默认使用 `%LOCALAPPDATA%\Treer`，CLI/Controller 共享路径规则；运行身份使用用户 SID；保留显式目录覆盖 |
| 后台运行 | [service_platform.rs](../../crates/treer-agent-server/src/service_platform.rs) 的其他平台实现基本返回 unsupported | 增加 Windows 用户后台进程模式；Host 负责 Controller 重启。前台模式用于诊断；默认后台模式不要求管理员权限 |
| 重复运行与恢复 | Unix 使用 socket 探测和 `ps` 的启动时间识别 PID | Named Pipe 探测、用户范围的单实例机制、进程句柄及创建时间；恢复入口复用现有持久化 launch/desired state |
| 默认 shell | [controller_output.rs](../../crates/treer-agent-server/src/controller_output.rs) 最终回退 `/bin/sh` | Windows 选择明确存在的 PowerShell 或 `ComSpec`；profile 保持 executable + argv，不把用户参数拼成 shell 字符串 |
| 终止进程 | PTY 子进程使用 `portable-pty` 的 kill；Windows 后端调用 `TerminateProcess` | 原生验证 ConPTY 关闭及子进程清理；必要时由 Host 为每个 workload 持有 Job Object，覆盖非控制台后代进程 |
| 安装与 PATH | Proxy 只有 [install.sh](../../crates/treer-proxy/src/api/machines.rs)，产物仅 Linux/macOS | 提供 `/install.ps1`，安装三份 `.exe` 到用户目录，校验下载，更新用户 PATH，并解释当前 shell/新 shell 的生效区别 |
| 更新 | `StagedExecutable::install` 直接 rename 到现有二进制；Windows 没有更新验证 | 单独设计版本目录和激活指针，或停进程后由更新 helper 替换。未经原生验证的旧路径应明确报不支持 |
| 发布与测试 | [release workflow](../../.github/workflows/build-release-artifacts.yml) 和 [发布脚本](../../scripts/release-r2.mjs) 固定四个平台 | 增加原生 Windows MSVC job、`.exe` 产物、manifest/校验和/下载映射；Unix 测试 fixture 增加 Windows 等价实现 |

### IPC 与最基础隔离

建议 pipe 名包含用户 SID 的摘要和稳定 machine ID，避免不同用户或 workspace 冲突。
配置显式 DACL，仅允许所需用户及系统账户，并拒绝远程 pipe 客户端。
不要沿用 Named Pipe 的默认 ACL；微软文档说明其默认权限还包含 Everyone/匿名读取。
首次创建和重复运行必须返回明确的占用错误；客户端处理 pipe busy 和连接重试。
Windows 的配置、workload credential 和缓存写入需要检查 ACL 与原子替换行为。
这些措施延续当前本地用户边界，同一用户下的进程仍属于共享信任环境。

### 进程所有权与恢复

后台 Host 不应依赖启动它的 PowerShell/终端持续存在；标准输入输出接到日志或空设备。
Windows 创建标志与 ConPTY 组合需要原生测试，不能把 `nohup` 名称直接搬过去。
后台 PID 文件必须配合进程创建时间验证，避免 PID 重用导致误杀。
Job Object 的句柄应由稳定 Host 持有，不能归 Controller 所有；Controller 替换时
不得因关闭句柄而杀掉 Agent。Job Object 在这里仅用于进程树清理。

重启后的基础路径继续使用现有命令：

```powershell
treer-agent-server.exe restore --workspace WORKSPACE_ID
treer.exe runtime restore --all
```

第一条恢复本机 Host/Controller 并执行本机协调；第二条用于已有 Controller 下的
workspace 恢复协调。需要保留 Agent/App ID、主 HTTP 服务 ID、显式 stop 意图和
幂等行为。恢复保存的启动命令，不承诺恢复进程内存或提供商的会话状态。
Agent 的启动脚本负责启动 HTTP 服务和重新执行主入口注册。

需要自动启动时，可后加当前用户的登录计划任务，运行相同恢复入口。
交互式 token 任务要求用户已登录；不要把登录后启动描述为无人登录的开机启动。
系统级 Windows Service 属于另一种部署模式，需要单独处理账户、用户环境与权限。

### HTTP 通信直接复用

HTTP proxy、`.treer` 解析、token 注入和 workspace 路由继续使用现有实现。
Windows 上示例使用 `curl.exe`，避免 Windows PowerShell 的 `curl` 别名：

```powershell
treer.exe service register --primary --protocol http --name reviewer-api --port 4180
$agentAddress = treer.exe agent address reviewer
curl.exe "$agentAddress/openapi.json"
```

HTTP 客户端仍需使用注入的 proxy 环境或显式 proxy 配置。纯 TCP 转发继续走现有
`treer service forward`；SSH 等原生协议的目标认证仍由应用和用户负责。

## 实施顺序与验收

1. **跑通原生进程链路。** 增加本地 IPC 与平台路径适配，三个二进制通过 MSVC
   编译；PowerShell 启动前台 Host/Controller，验证 enroll、CLI 认证、exec、PTY
   输入输出/resize、cwd 边界和凭据文件权限。测试空格、中文路径和 argv 原样传递。
2. **后台运行与手动恢复。** 实现用户后台 Host、进程身份与重复启动检查；关闭
   安装终端不影响工作负载，Controller 替换保留 Agent，Host 冷重启按保存意图恢复
   Agent/App。验证 stop 不复活、并发 restore 不重复启动和子进程不残留。
3. **网络与安装发布。** 真正的 Windows 机器连接测试 Proxy，注册主 HTTP 服务，
   另一台机器用 `agent address` + HTTP 调用；验证身份、权限拒绝、WebSocket、
   TCP 半关闭、App 部署、端口转发与公共入口。补 PowerShell installer、PATH、
   `.exe` 下载和 Windows release gate。只有验证过的更新路径才能开放。

可选的登录自动恢复放在基础版本之后；无需为它提前引入 Windows 系统服务。
初次调查仅建立上述基线；用户随后批准实施。当前没有发布 Windows 产物。

## 第一版实施记录

- `treer-platform` 统一 Unix socket / Windows Named Pipe 字节流、用户路径、
  DACL、文件替换、进程创建时间、用户范围 mutex 和 Job Object。Named Pipe
  限当前 SID/SYSTEM，拒绝远端；探测断开不会终止监听循环。
- Windows 默认 `detached` 后台 Host，记录 PID + 创建时间并验证停止目标；
  不依赖启动终端、不注册系统服务/登录任务。Host 与每个 workload 持有生命周期
  Job，Controller 热替换保留 workload；默认交互 shell 为 `ComSpec`。
- 用户状态放在 `%LOCALAPPDATA%\Treer`；敏感状态采用私有 ACL 和原子替换。
  CLI 仍是 argv/流客户端，没有 Windows 业务解析分叉。
- Proxy 提供 `/install.ps1` 与平台 `SHA256SUMS` 下载。安装器先验证三份产物
  校验和及构建身份，再安装 `.exe` 并更新当前/用户 PATH；拒绝替换运行中的安装。
  Windows 原地更新明确关闭，使用 stop → reinstall → restore。
- 发布流程增加 `windows-x86_64` 原生 MSVC job；采集、候选验证、公共下载验证、
  R2 manifest 接受第五个平台。公开 artifact key 不加后缀，Windows 安装时添加
  `.exe`。既有 fallback release 需要同时提供 `SHA256SUMS-windows-x86_64`。
- 原生 CI 包含 Named Pipe 重连/独占、文件替换/PID 误杀保护、ConPTY
  argv/输入/resize/stop、CLI/Controller 认证、后台 Host 重复启动、热替换保留
  进程、冷启动及安装/PATH/错误校验和测试。

本机是 macOS，不把 Windows 目标编译当作运行验收。真实 Windows 上的 enroll、
Agent/App 冷恢复、HTTP proxy 跨机通信/公网服务和异常退出后的子进程清理仍须验证。
在 Windows runner 和这些原生验收通过前，不应宣称完整支持或直接推广产物。

### 已完成的本机验证

- Mac 上三个机器二进制构建通过；Host、runtime、平台适配、服务管理测试通过。
- 真实 Host 热替换测试保留 workload PID 和 PTY epoch；真实 Controller/CLI
  认证、stdin、cancel 回归通过；恢复协调仍保留 stop 和身份撤销语义。
- `x86_64-pc-windows-gnu` 上平台适配、runtime、Host、Controller、CLI 的全部
  test target 通过编译与 `clippy -D warnings`。Mac 的 Zig C 工具链用于 TLS 依赖
  交叉检查；正式产物仍必须使用原生 MSVC runner。
- Windows installer 的 Proxy 模板/PowerShell quoting 单测、五平台产物/校验和
  与 release manifest Node 测试通过；frontend TypeScript 和文档链接检查通过。
- PostgreSQL 集成测试没有运行，本次没有修改数据库持久化或权限事务逻辑。

## 官方资料

- [Microsoft ConPTY session lifecycle](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session)
- [Tokio Windows Named Pipe API](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/index.html)
- [Microsoft Named Pipe permissions](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- [Microsoft process creation time](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes)
- [Microsoft kernel object namespaces](https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces)
- [Microsoft Job Object lifecycle](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- [Microsoft scheduled task logon types](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/ne-taskschd-task_logon_type)
- [Microsoft task security contexts](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks)
