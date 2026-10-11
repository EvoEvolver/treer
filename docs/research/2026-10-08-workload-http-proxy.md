# Agent/App 独立 HTTP 代理入口

状态：已实现，2026-10-08 本地验证。行为契约见
[Operator guide](../operator-guide.md#workload-identity) 和
[Architecture](../architecture.md#explicit-service-transport-and-cli)。

## 实现范围

- Controller 为每个新启动的 managed Agent / App runtime 分配一个独立的
  `127.0.0.1` HTTP listener，同一 Controller 进程管理所有入口。
- listener 固定关联 workload 身份；不查询 PID，也不接受请求头选择身份。
- 启动前注入 `http_proxy`、`https_proxy`、大小写兼容变量、`no_proxy` 和
  `TREER_HTTP_PROXY`，普通代码及子进程直接继承。默认绕过 localhost。
- `http://SERVICE_ID.treer/path` 在代理内解析为已注册服务，不改系统 DNS，
  不要求自定义域名或重新发布服务。使用稳定 service ID，不以 URL 端口覆盖注册目标。
- 每个 HTTP 请求向现有本地 API 获取服务 audience 的短期令牌，再打开现有
  `service.connect` 通道；两个步骤都沿用 Proxy 身份验证与 Policy。
- 目标收到 `X-Treer-Identity-Token`；调用方没有应用 Authorization 时也注入
  Bearer Authorization。已有应用认证头保留；原始 workload / operator / machine
  凭据和伪造 Treer 身份头不透传到目标。
- 其他 HTTP 目标直接连接，不注入 Treer 身份。请求/响应流式转发，支持 WebSocket
  upgrade、CONNECT 和双向 half-close，不缓存整个业务正文。
- Host 的不透明 JSON metadata 保存入口端口。热 Controller 重启为存活进程重新
  绑定原端口；冷恢复创建新入口并重新注入环境；停止/退出关闭入口及连接。
  启动失败回收未运行 workload 的入口，重绑定冲突明确报错。

实现位于 [workload_proxy.rs](../../crates/treer-agent-server/src/workload_proxy.rs)，
生命周期由 [controller.rs](../../crates/treer-agent-server/src/controller.rs) 管理。
没有增加 Host 产品语义或新的中心服务器接口。

## 使用与边界

```sh
treer service list
# 将 svc_ACTUAL_ID 替换为完整的实际 service ID：
curl http://svc_ACTUAL_ID.treer/path
```

代理管理令牌签发，应用代码无需读取 session key 或处理刷新。
需要识别身份的接收服务仍须验证令牌。Managed App 使用其 backing runtime 的
Agent 身份；稳定 App/service ID 与运行实例身份是不同概念。

HTTPS CONNECT 保持 TLS 不透明，不安装证书、不注入 TLS 内的 HTTP 头。
访问公网 App 的 HTTPS 域名不会自动完成入口登录；自动 HTTP 身份使用 service ID 地址。
注册的 SSH/TLS 服务可用 CONNECT 或既有显式端口转发，原生 SSH 认证及 host-key
校验仍由 SSH 完成；本次没有增加 SSH daemon。

入口只绑定本机 loopback，不是沙箱。能够访问别的入口的本地代码可以使用该入口身份；
外部隔离环境自行安排入口可达性。忽略 proxy 环境变量的客户端需要显式配置代理。
旧版本已运行的 workload 需要重新启动才能收到新环境，单独热更 Controller 无法修改
存活进程的环境。

## 验证

- Controller：100 项测试通过；另行构建二进制并运行真实 Host 冷恢复测试，通过。
- CLI：5 项 session 测试及真实 Controller 的认证/parser/stdin/cancel 集成测试通过。
- Proxy：独立本地 PostgreSQL 支持的 `service` 过滤测试 12 项通过、1 项显式
  public-TCP 端口分配测试保持 ignored；`identity` 过滤测试 10 项通过。
  包含服务 scope、audience、过期、签名持久化及运行连接的 Policy 撤销。
- `cargo clippy -p treer-agent-server -p treer-cli --all-targets -- -D warnings`、
  Rust format、文档链接和脚本语法检查通过。
- 最终 Docker 镜像重新从当前源码构建，23 项回归检查全部通过：真实安装/认证、
  两 Proxy + NATS、跨机器 Agent 与 App 身份、每次新签令牌、凭据头清理、
  1 MiB CONNECT/half-close、校验证书的本地 HTTPS、WebSocket、Agent/App 停止
  回收、App crash recovery、热重启端口不变及冷恢复后的 Agent/App 调用。

```sh
docker build -t treer-local-e2e:workload-proxy .
TREER_E2E_IMAGE=treer-local-e2e:workload-proxy node scripts/docker-core-e2e.mjs
```

最终证据目录：
`/var/folders/07/fl6q41055fvd6m_5g58rjjh00000gn/T/treer-core-e2e-DvgQhl/`。
`results.json` 记录 23 项结果，`resources.json` 记录本轮容器及 network。
测试创建的容器均已停止，原有 `treer-docker-demo` 未改动。

这不是 release/production 验证；没有执行 `just check`、部署或推送。
Docker 覆盖 Linux ARM64；本机 Rust 测试覆盖 macOS ARM64。
没有验证物理重启、OS 开机服务、Windows、真实公网 DNS/TLS 或 provider CLI。

## 2026-10-09：服务名称地址

在独立代理入口上增加 `http://NAME.treer/path`，HTTP 与 CONNECT 都支持。
名称使用已有 workspace 内唯一、大小写不敏感的服务名称，不增加自定义域名、
DNS 配置或服务 ID 格式。名称地址接受 1–63 个 ASCII 字母、数字及内部连字符；
其他显示名称继续使用完整 service ID 地址。

每个新请求重新解析名称，不缓存映射。HTTP 签发令牌后，用返回的 canonical
audience ID 建立连接，避免改名或名称复用使令牌与连接目标分离。
CONNECT 在中心 Proxy 解析名称后按稳定 ID 检查 Policy，运行连接仍支持撤权。
改名后旧名称的新请求返回 404，新名称与原 ID 可用；已建立连接不因改名迁移。
此变更只扩展 Agent 身份的 service connect 入口，human forwarding 仍要求 ID。

验证结果：

- Controller 的 `workload_proxy` 过滤测试 3 项通过，覆盖名称、改名、稳定
  audience/连接目标以及 CONNECT；缺失名称 404，无效名称地址 400。
- PostgreSQL-backed Proxy `service` 过滤测试 12 项通过、1 项既有测试 ignored；
  包含按大写名称连接、Policy 使用稳定 ID、撤权关闭连接。
  `agent_identity_tokens_use_the_canonical_service_audience` 单独运行通过。
- Agent Server 与 Proxy 的 all-target Clippy、Rust format、文档链接、脚本语法
  及 diff whitespace 检查通过；未执行 `just check`。
- 从当前源码构建 Docker 镜像后，24 项回归全部通过。新增名称/大小写/改名检查，
  CONNECT 同时验证名称与 ID；App 身份调用、WebSocket、热重启与冷恢复也使用
  名称地址验证。安装、通信、部署与恢复原有检查全部通过。

```sh
docker build -t treer-local-e2e:service-names .
TREER_E2E_IMAGE=treer-local-e2e:service-names node scripts/docker-core-e2e.mjs
```

镜像：`sha256:0d29297dd8e96df8a3a9c682cb4c7b11a10c23e02299a643b6a74b8eb9af9599`。
证据：`/var/folders/07/fl6q41055fvd6m_5g58rjjh00000gn/T/treer-core-e2e-bDNMW5/`，
其中 `results.json` 记录全部 24 项结果，`resources.json` 记录测试资源。
本轮 Docker 回归容器与测试数据库均已停止，原有 `treer-docker-demo` 未改动。
平台与公网验证边界同上；未部署、提交或推送。
