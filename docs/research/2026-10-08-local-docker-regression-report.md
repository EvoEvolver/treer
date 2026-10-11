# 新版 Treer 本地 Docker 回归测试

日期：2026-10-08。本文保留初测和后续修复记录，最新修复结果见文末。初测结论：**Agent 通信、私有转发、Managed App 部署及恢复的主要链路通过；安装和 CLI 有四处确定的问题。** 初测阶段没有修改产品实现。

## 环境和方法

- Rust 源码：`9b77581a46e2deccf34ac8611357708d5a7bddce`，版本 `0.1.13`。使用根 Dockerfile 重新构建 release Proxy、Host、Controller 和薄 CLI；安装后验证二进制显示 `9b77581`。
- 平台：本地 Docker Desktop，Linux ARM64；Docker `29.7.2`。
- 独立 PostgreSQL `17-alpine`、NATS `2.14.4` JetStream、两个 Proxy、一个 Web 容器、三台非 root Linux 测试机器。A/B 分别连接不同 Proxy，C 验证补齐依赖后的全新安装。
- Web 使用当前工作树构建，包含原有未提交的 `web/package.json` / `web/pnpm-lock.yaml` 依赖变化。这些文件没有被本次测试修改。
- 启用真实认证、邀请注册和 Core Messages；没有使用 `--disable-auth`。按[嵌入式 Treer skill](../../skills/treer/SKILL.md)核实 Agent 身份、消息确认和恢复语义。
- 容器与网络前缀：`treer-e2e-1008-a3car0`。宿主机监听绑定到 `127.0.0.1`；公网访问模型通过本地 Proxy 的 HTTP Host 路由和 TCP 端口验证，没有接入真实公网 DNS/TLS。
- 未启动 updater，未访问 Railway/生产环境，未操作已有 `treer-docker-demo`。

## 已通过的实际链路

| 能力 | 观察结果 |
| --- | --- |
| 中心服务构建 | 当前 Rust release 镜像、Web frozen-lockfile 安装及生产构建成功 |
| 认证 | 匿名访问被拒绝；管理员邀请、用户注册、密码登录、创建 workspace 成功 |
| 安装与注册 | 安装脚本只安装二进制；补齐 `procps` 后，普通 Linux 用户全新安装、connect、上线成功；未设置网络模式时 plain 默认可用 |
| 部分安装恢复 | 最初缺少 `ps` 的两台机器，补齐依赖并运行 `service --workspace … repair` 后复用已保存的注册身份上线 |
| 薄 CLI | Agent 内 `whoami`、帮助和 skill 正常；跨机器 exec 保留空格、中文、字面 `$()` 和业务 argv 中的 `--url` |
| 身份分离 | Agent 模式缺少凭据时拒绝 operator 回退；profile 启动进程有 Agent workload 凭据，operator 凭据值为空 |
| Profile | 创建、读取、修改及通过 server ID 跨机器 launch 成功；执行参数保持原样 |
| 跨 Proxy 通信 | 两机 Agent 发现、远程执行、prompt 和终端输出读取经 NATS 正常 |
| Core Message | 管道 stdin 多行中文正文、发送幂等、未确认重复投递、ack、reply 的 context edge 正常 |
| 私有服务 | Agent 注册自有 HTTP/TCP 端口；B 使用 `service forward` 连接 A；HTTP、1 MiB 二进制回传和 TCP half-close 正常 |
| HTTP 公布后端 | 绕过失败的 CLI、用已认证 Proxy API 创建 ingress 后，生成的服务域名实际转发正常 |
| 公共 TCP | 显式 `--tcp --access public` 分配端口并完整回传 1 MiB；unpublish 终止已有连接，新连接无法传输数据 |
| App 部署 | Agent A 在机器 B 创建 public/private App；平台生成稳定 URL；HTTP 根路径、相对资源、redirect、中文 POST 正常 |
| App 鉴权 | private App 匿名访问 303；workspace 登录授权、callback 和独立 ingress cookie 后访问 200；重复使用授权 code 返回 401 |
| WebSocket | public App 经不同 Proxy 的 NATS 路由完成 101 upgrade 并回传消息 |
| App 生命周期 | stop/start/restart、杀死进程后的自动拉起正常；service ID 和公开 URL 保持稳定；delete 清除 App、自有 service 和 ingress |
| Controller 热重启 | Host 和 Agent 保持运行，Agent PID / started_at 不变；重复 restore 返回 attached，不重复启动 |
| Agent 冷恢复 | 停止、启动机器容器，再执行 `treer-agent-server restore --workspace …`；运行中的 Agent 保留 ID，进程重新启动，注册服务重新可达 |
| 停止与失败语义 | 主动停止及成功退出的 Agent 不恢复；不存在的启动程序明确报告 failed；反复 restore 保持同一正常进程 |
| App 冷恢复 | 机器 B 冷启动后 public App 恢复并保持原 service ID / URL；主动停止的 private App 仍是 stopped；恢复后只有一个 public App 实例 |
| Proxy 重建 | 两个 Proxy 容器重建后，三台机器自动重连；已发送消息可读，App PID / URL 不变，HTTP 仍可访问 |
| 实际浏览器 | Chromium 对真实 Docker Web/Proxy 登录成功，Agent/App 页面显示实测对象，无 pageerror |
| 退役入口 | App API 对自定义 hostname 返回 `400 platform_domain_required`，没有创建额外部署 |

## 四处需要修正的问题

### 1. CLI 无法发现新安装保存的 Controller 配置

在安装并连接后的宿主机运行：

```sh
treer --workspace "$WORKSPACE_ID" message --help
```

得到 `local operator credential is missing`。显式指定 `--url` 也不能自动解决凭据查找。

根因：[CLI 的 config_path](../../crates/treer-cli/src/main.rs)按 workspace ID 查找 `<workspace>-controller.json`，而[服务安装](../../crates/treer-agent-server/src/service.rs)按 server ID 写入 `<server>-controller.json`（约 1415 行）。这是配置命名契约不一致。

建议：按配置内容中的 workspace 和机器安装身份发现安装；没有匹配或有多个匹配时明确报错。不要另建一份重复的身份配置。测试后续使用读取真实安装配置并显式提供 URL / operator 凭据的临时包装器；因此 Agent/App 测试通过不能掩盖此缺陷。

### 2. HTTP service publish/unpublish 的 CLI 路径错误

在具有完整身份的 managed Agent 内，以下两条命令都返回 `cli_failed: invalid JSON response`：

```sh
treer service publish "$SERVICE_ID" --access public
treer service unpublish "$SERVICE_ID"
```

[CLI 命令解析器](../../crates/treer-agent-server/src/cli_command.rs)请求 `api/ingresses`（约 880 行及 unpublish 分支），[本地 API](../../crates/treer-agent-server/src/local_api.rs)实际提供 `/api/publish` 和 `/api/publish/{ingress_id}`（约 394 行）。404 的空正文又被统一包装成 JSON 解码错误。

建议：统一内部路由，再验证完整 register → publish → HTTP 访问 → unpublish 链路，同时保留真实 HTTP 状态错误。Proxy API 公布和 Managed App 的生成域名都已实测可用；问题位于 CLI 到本地 Controller 的入口。

### 3. Operator 调用 status 被误判为不完整 Agent 身份

显式提供正确本地 operator 凭据后，`treer status` 仍报：

```text
managed agent identity is incomplete; TREER_AGENT_ID and TREER_SERVER_ID must both be set
```

[cli_session 入口](../../crates/treer-agent-server/src/local_api.rs)无论 Agent/operator 都注入 `TREER_SERVER_ID`（约 169 行）；[status](../../crates/treer-agent-server/src/cli_command.rs)把仅有 server ID 解释成残缺 Agent 身份（约 1011 行）。全新机器 C 和热重启后的机器 A 都重现。

建议：区分机器执行上下文和认证主体；只有认证为 Agent 时才构造 Agent 的 self。Operator 的 status 可以返回机器信息及空 Agent self。热重启本身经独立 PID 和 inventory 检查已通过，此错误不应被归类为热重启失败。

### 4. 精简 Linux 缺少 ps 时，connect 在消费注册 key 后失败

精简 Debian 有 curl、Python、nohup、hostname，但没有 `ps`。安装脚本成功，`connect` 完成注册并保存配置、甚至已经启动 Host 后才报：

```text
machine enrollment was saved but service activation failed
failed to inspect the nohup Host process with ps
No such file or directory (os error 2)
```

原因：[nohup PID 身份记录](../../crates/treer-agent-server/src/service.rs)调用 `ps -p … -o lstart=`（约 1810 行），前置依赖检查没有覆盖它。首次失败还可能留下运行中的 Host 而没有完整 PID 管理记录。

建议：保持依赖要求简单，明确要求 `procps`，在消费 enrollment key 和启动 Host 前检查所有必要命令，给出安装指引；不要为了这个问题增加一套复杂平台服务。补齐依赖后 repair 和全新 connect 已通过。

## 另外两处一致性问题

- Profile `launch --machine docker-b` 返回 `server_not_found`，使用 `srv_…` 成功。当前 skill 示例确实要求 server ID，但它与 agent/app/exec 接受机器名的体验不一致；建议统一解析或在帮助里明确参数必须是 ID。
- [安装脚本](../../crates/treer-proxy/src/api/machines.rs)仍检查 unshare 并打印透明网络依赖提示；canary machine Dockerfile 仍默认 `TREER_NETWORK_MODE=proxy-env`，旧 canary 脚本也引用退役的 virtual-host 流程。这些内容需跟随 plain/显式服务方向清理。当前 `message --help` 仍列出已明确拒绝的 `--body-file`；管道 stdin 不加该参数已通过。

## 验证范围和证据

这次覆盖真实 Linux 容器进程、数据库、两 Proxy 的 NATS 网络及浏览器，没有用 mock API 替代功能验证。冷恢复通过停止整个机器容器杀死 Host/Controller/Agent，再启动容器并手动 restore；它不等于物理机器断电或 OS 开机服务测试。

未覆盖 Linux x86_64、原生 macOS/Windows、真实公网 DNS/TLS/防火墙、systemd/launchd 开机启动、原生移动端、真实 provider CLI/LLM 配置，以及 updater 部署。应用使用测试 HTTP/WebSocket 服务，没有宣称验证了所有 `apps/` 集成。未运行 `just check`。

临时夹具、分阶段结果及浏览器截图保存在 `/tmp/treer-docker-e2e.a3car0/`；`session.json` 仅含这次测试环境的会话，权限 0600。原始结果包含早期测试断言错误，最终判断以上表和四个确认问题为准：HTTP Host 使用实际头路由，private ingress 按完整授权流程验证，TCP 撤销按拒绝流量验证，凭据分离按凭据值验证。

初测结束后停止本次创建的八个容器，保留其状态、镜像和日志便于复现；已有容器不受影响。初测建议先修复问题 1–3 的 CLI 契约，再补安装依赖前置检查，并把安装 → Agent → 显式服务 → App → 冷恢复的本地 Docker 流程纳入可重复执行的回归入口。

## 修复与最终复测

四个确定缺陷和上述一致性问题均已修复：

- 薄 CLI 按配置内容中的 workspace / install hostname 发现实际 `<server>-controller.json`；单一安装无需 flags，多匹配要求显式选择。Agent 模式仍不回退 operator 凭据。
- HTTP service publish/list/unpublish 统一走 Controller `/api/publish`；非 JSON 的 HTTP 失败保留真实状态码。
- Controller CLI 会话只为 Agent 注入 Agent/server 身份，清除继承的 server ID；operator `status` 正常返回机器信息及空 `self`。
- nohup 在消费 enrollment key、启动 Host 前检查必要程序及 `ps` 进程身份；Linux 缺依赖提示安装 `procps`。
- Profile launch 与其他命令一致，接受唯一机器名或 server ID。
- 安装脚本删除 unshare/透明网络提示；Canary 镜像补 procps，默认 plain，测试改为显式 forward 和平台生成域名。消息帮助隐藏旧 `--body-file`；兼容 `--body-file -`，真实文件路径继续拒绝，推荐管道 stdin。

复测又发现并修复了一个初测未覆盖的 App 恢复缺陷：**App 创建或重启后 15 秒内冷启动机器，旧 reconciler 会在防重复启动窗口内跳过 App，且没有后续重试，因此永久 pending。** 原实现的无人介入回归稳定超时；修复后保留 15 秒窗口，期间重读持久化意图和运行身份，窗口结束后继续恢复。并发恢复仍通过原有数据库原子 claim 保证只启动一个实例；等待期间 stop 不会被覆盖，已删除的记录不会被重新创建。

最终使用 `9b77581` 基础上的本次工作树修复重新构建 Linux ARM64 release 镜像 `treer-local-e2e:fixed`，镜像 ID 为 `sha256:4086983cca5ab27ace4c84188bd45e92f3adbb10bb66a5e05d05e3a0396245aa`。这是未提交工作树测试镜像，不是正式发布。

[可重复执行的 Docker 入口](../../scripts/docker-core-e2e.mjs)最终 **19 项检查全部通过，执行期间没有人工补救**。覆盖真实认证、两台非 root 机器安装、缺少 ps 后复用同一个 key、直接使用安装配置的宿主机 CLI、Agent 身份、跨 Proxy exec/消息、机器名 Profile、HTTP/TCP 私有转发（含 1 MiB 和 half-close）、HTTP CLI 发布/访问/撤销、实际 Canary shell 转发命令、public/private App 部署及鉴权、App stop/start/restart/崩溃恢复、Controller 热恢复、机器冷恢复、停止/完成/无效启动指令语义、稳定 App ID/service/URL、单实例和删除清理。

```sh
docker build -t treer-local-e2e:fixed .
node scripts/docker-core-e2e.mjs
```

其余定向验证也通过：

- `cargo test -p treer-cli -p treer-agent-server`：104 个测试通过；两个依赖 sibling binaries 的默认 ignored 测试随后显式运行通过。
- `cargo test -p treer-cli --test controller -- --ignored`，以及 Controller 的 `recovery_real_host_cold_restart` ignored 测试：真实进程认证、stdin、取消及冷恢复通过。
- Proxy installer 两个测试通过；独立本地 PostgreSQL 上三个 `app_reconcile_` 测试和一个稳定 App 所有权测试通过，包含无新 snapshot 的并发重试和等待期间 stop。
- 三个受影响 Rust crate 的 all-target Clippy（warnings denied）、Rust format、shell/Node 语法、文档链接和 diff whitespace 检查通过。

最终证据目录为 `/var/folders/07/fl6q41055fvd6m_5g58rjjh00000gn/T/treer-core-e2e-NkPaX0/`，包含 `results.json`、容器日志及列出精确资源名的 `resources.json`。容器前缀 `treer-core-nkpax0`。最终测试的六个容器和独立定向测试数据库均已停止，保留状态便于检查；原有 `treer-docker-demo` 保持运行。

本轮没有重新执行初测的浏览器、WebSocket 和公共 TCP 场景；初测证据保留在上文。未运行完整 Canary 镜像构建、Railway Canary、全量 Proxy 数据库测试或 `just check`；没有部署生产。跨平台、物理断电、OS 开机启动及真实公网 DNS/TLS 的范围限制不变。原有 Web 依赖改动及 external-Agent 计划文件保持原样；本轮没有实现 external-Agent 架构。
