# macOS 透明网络：Apple machine 实测与原生实现研究

日期：2026-09-05。分支：`codex/macos-transparent-network`。
研究基线：从 `main` fast-forward 到远端 `d170767`，原工作区干净。

## 当前结论与完成边界

**Apple container machine 已直接跑通现有 Linux 透明网络；原生 Mac 的透明捕获和
真实 Treer 服务数据链路也已跑通。** 原生版本目前是分支中的可复现实验，并非可以
替换 Linux sandbox 的生产模式。已实测发现半关闭、共享 DNS 缓存及进程隔离差异；
本分支没有启用新的 macOS Controller 网络模式。

| 层次 | 结果 | 证据范围 |
| --- | --- | --- |
| Apple machine 的 rootless user/net/mount namespace、TUN | 通过 | 实际 guest 用户，无 sudo |
| guest 裸 TCP、虚拟 DNS、身份、拒绝、HTTPS | 6/6 通过 | 实际 `sandbox-exec` + 隔离 SOCKS fixture |
| guest 两个私有 loopback、Unix bridge、publish | 3/3 通过 | 实际 Linux sandbox，无新注册服务 |
| Controller 网络处理 | 13 项通过 | 原有 Rust focused tests |
| Policy / PostgreSQL | 13 项通过 | 隔离测试 schema，不修改现有 workspace Policy |
| traffic / PostgreSQL | 7 项通过 | 独立于捕获 fixture 的计数/持久化测试 |
| 两个 Proxy + 真实 NATS | 修复后通过 | 双向二进制数据、方向字节计数、游标、断连清理 |
| Mac 非 root `utun` | `EPERM` | 实际内核调用；未修改路由/DNS/PF |
| Mac 已签名 NE transparent proxy | 6/6 通过 | 裸 TCP、虚拟 DNS、按 PID 身份、fixture 拒绝、保留地址、正常 TLS |
| Mac 实际 Agent → 两个 Proxy → Apple guest | 通过 | Host PID 映射、真实 Policy allow/deny、NATS relay、二进制 payload |
| Mac 保留地址 → 实际 Controller API | 通过 | `192.0.2.1:<api-port>/api/health` 返回 200 |
| 实际 PostgreSQL relay ledger | 通过 | 两个方向精确 12 / 18 payload bytes，各 1 frame |
| Mac 客户端先 `shutdown(SHUT_WR)` 再接收 | 失败，已定位上游 | 账本收到 12 / 18，但客户端回包为 0；签名 helper 提前全关闭 |

完整复现入口：[network-lab](../../scripts/network-lab/README.md)。原始结果位于
忽略目录 `output/network-research/`，不提交二进制、依赖和第三方源码。

## 1. Apple container machine

实测环境为 Apple silicon `arm64`、macOS `27.0` build `26A5421a`、Apple
`container 1.2.2`。既有 machine `treer` 正在运行，8 CPU / 16 GiB，guest 是
Linux `6.18.15` aarch64。guest 用户 uid 502 / gid 20；`/dev/net/tun` 为
`0660 root:dialout`，该用户可读写。内核包含 `CONFIG_USER_NS=y`、`CONFIG_NET_NS=y`、
`CONFIG_TUN=y`，rootless `unshare --user --map-current-user --net --mount
--keep-caps --fork` 成功。

既有运行进程明确使用 `TREER_NETWORK_MODE=transparent`。本次探针调用
`/opt/treer/bin/treer-agent-server`，其版本为 `0.1.11 (1e07080)`。这验证的是
**当前已安装 guest runtime**，不是声称已将 guest 更新成 `d170767`。
没有重启既有 Host/Controller，没有复制 Mac enrollment 配置，也没有重新注册机器。

Linux 数据路径是：

```text
普通 Agent TCP / DNS
  → 每 Agent 独立 user + net + mount namespace
  → tun2proxy 虚拟 DNS / TUN
  → 父进程创建并传入的 host-network socket
  → Controller SOCKS5（用户名携带 Agent ID）
  → 现有 Proxy Open / Policy / Direct 或 Relay
```

捕获测试清除了大小写 HTTP(S)/ALL/NO_PROXY。`probe_client.py` 仅调用普通 socket
或 `exec` 系统 curl，不知道 SOCKS 的存在。虚拟域名 `echo.treer.invalid` 在隔离
resolver 中解析，SOCKS 收到原始域名；每次启动收到独立 Agent ID；允许请求的两个
方向分别为 50 / 75 字节；拒绝请求表现为 EOF；真实 HTTPS 正常验证证书并收到页面。
保留地址 `192.0.2.1` 也被捕获，但该项的目标是 fixture，不是既有 Controller API。

另外两个 sandbox 同时绑定 `127.0.0.1:18761`，分别经自己的 Unix service socket
返回 `AGENT_0` / `AGENT_1`；一个动态 host-loopback publish 端口能进入第一个 namespace。

**本机 guest 不需要适配透明网络。** 镜像显式补上 `util-linux` 依赖，skill 增加
TUN 访问与 rootless unshare 检查，防止依赖基础镜像恰好带有命令。
如果别的 machine 不可用，按失败层排查：缺命令安装 `util-linux`；缺设备检查
内核 TUN 与 guest 设备节点；权限错误检查 guest 设备 owner/group；`unshare EPERM`
检查 user namespace/LSM 限制。内核缺功能时更换支持的 Apple machine kernel。
不要默认改为 privileged、关闭整机保护或悄悄退回 `proxy-env`。
本次没有重建镜像，新增依赖的完整 image build 尚未验证。

## 2. 参考 Tailscale 后的选择

参考源码放在忽略的 `.references/`：

- Tailscale：`5201273aec737d6372ab7423c31c04ca3ca2a0c2`。
- mitmproxy_rs：`58bea7f7e0b7b00c7d91b9997c83299ae3e0922e`。
- 本机安装的 Tailscale 为 `1.102.3`；本次没有改动其配置或停用它。

[Tailscale 官方 macOS 变体说明](https://tailscale.com/docs/concepts/macos-variants)
区分 GUI Network/System Extension 和开源 `tailscaled` 的内核 `utun` 方式。
这说明 CLI 版本可以避开自行开发 GUI extension 的负担，但并不意味着普通用户可以
无权限创建 TUN，也不意味着 TUN 自带每个 Agent 的身份。

对应实现值得复用的设计：

- [BSD/macOS router](https://github.com/tailscale/tailscale/blob/5201273aec737d6372ab7423c31c04ca3ca2a0c2/wgengine/router/osrouter/router_userspace_bsd.go)：
  自动接口、路由差量更新、只清理自己的状态。
- [Darwin socket routing](https://github.com/tailscale/tailscale/blob/5201273aec737d6372ab7423c31c04ca3ca2a0c2/net/netns/netns_darwin.go)：
  将自身 outbound socket 绑定到合适接口，避免套回隧道，并处理默认接口变化。
- [Darwin DNS](https://github.com/tailscale/tailscale/blob/5201273aec737d6372ab7423c31c04ca3ca2a0c2/net/dns/manager_darwin.go)：
  `/etc/resolver/<suffix>` 的 split DNS 和自有文件清理。
- [utun 权限诊断](https://github.com/tailscale/tailscale/blob/5201273aec737d6372ab7423c31c04ca3ca2a0c2/net/tstun/tun_macos.go)：
  非 root tunnel 创建失败是明确的支持边界。

**建议先保留 Treer 的身份、Policy、WebSocket、NATS 数据路径，只替换 OS 捕获层。**
直接加入完整 tailscaled/tsnet 并不能自动截获任意现有 Mac 子进程，还会引入另一套
节点注册、密钥和 ACL 生命周期。Tailscale 的设备身份也不能替代同机多个 Treer Agent
的 workload 身份。是否以后引入 WireGuard/DERP，应由跨地域 payload 性能测量决定。

## 3. 原生 Mac 候选与实测障碍

| 方案 | 优点 | 与 Linux 的差异/成本 |
| --- | --- | --- |
| `utun` + 已有 tun2proxy | 可复用当前 Rust TUN/TCP 栈；CLI 形态简单 | 需要特权、路由/DNS维护；包中无 PID；必须解决 daemon 绕行、全机流量范围与身份查找 |
| `NETransparentProxyProvider` | OS 提供 TCP/UDP flow 与来源审计元数据；原生按进程筛选 | 签名/授权；没有 Linux net namespace；DNS、子进程、失败行为仍要专门实现 |
| DYLD 拦截或仅代理环境 | 小原型容易 | SIP/静态程序/不使用代理的客户端会绕过，不作为透明模式 |
| Apple Linux machine | 现有实现已通过，私有 loopback 完整 | Agent 运行 Linux 程序，不能替代需要 macOS SDK/Keychain 的 native Agent |

本机 `utun_probe.c` 实际调用 `PF_SYSTEM/SYSPROTO_CONTROL`、`CTLIOCGINFO`、
`connect`，返回 `Operation not permitted (errno=1, euid=502)`。
`sudo -n true` 也要求密码。探针未添加默认路由、未修改 DNS、未改 PF。
即便获得 root 并成功打开 utun，也只证明设备可创建，仍需完成按 Agent 身份的透明捕获。

[Apple NETransparentProxyProvider](https://developer.apple.com/documentation/networkextension/netransparentproxyprovider)
及 [NEFlowMetaData](https://developer.apple.com/documentation/networkextension/neflowmetadata)
提供较合适的按连接入口。[mitmproxy Local Capture](https://docs.mitmproxy.org/stable/concepts/modes/#local-capture)
已经用这些 API 支持 macOS 的按 PID/进程名捕获。

为了减少初次签名和系统集成负担，本次安装了官方 PyPI 的 `mitmproxy_rs==0.12.11`
及 `mitmproxy-macos==0.12.11` 到独立 venv。只调用底层透明转发库，不运行 mitmproxy
的 HTTP/TLS 解密代理。已安装的 `Mitmproxy Redirector.app` 通过
`codesign --verify --deep --strict`，Team ID `S8XHQB96PW`，扩展权限为
`app-proxy-provider-systemextension`。

用户在系统设置启用扩展后，实际状态为：

```text
org.mitmproxy.macos-redirector.network-extension (2.0/1)
[activated enabled]
```

### 3.1 原生捕获与 DNS

`capture_probe.py --macos` 的 6 项全部通过。普通 Python socket 和系统 curl 均
清除了所有代理环境变量；系统 flow metadata 提供正确 PID，捕获 helper 自动查表
转换为对应的 SOCKS Agent username。HTTPS 不解密、不安装 TLS 根证书，正常验证
`example.com` 证书。系统自带 Tailscale 同时保持启用。

实测两处需要处理的差异：

- Python / PyO3 callback 必须保留强 task 引用，否则等待 Rust stream read 的 task
  会被回收，出现 `Task was destroyed but it is pending!`。`MacCapture` 保留活动 task，
  在关闭 adapter 时取消并等待清理。
- 不能照搬 Linux 的全域名 synthetic DNS。本次 DNS UDP flow 有原始进程 PID，
  但回答进入 Mac **共享**缓存；即使 TTL 0，转发进程也可能立即解析到虚拟地址，
  导致循环失败。现在只对专用 `*.treer.invalid` 实验名称返回虚拟 IPv4，公网 DNS
  保留真实响应，公网转发使用独立 DNS resolver。测试后系统 `example.com` 解析正常。
  对已缓存的公网 A 记录可能没有 DNS flow，HTTPS SOCKS 目标表现为真实 IP；
  **不能据此声称公网域名 Policy 与 Linux 等价**，也不能用反查或共享 IP 猜域名。

### 3.2 实际 Treer 端到端实验

`live_macos.py` 启动两个从当前分支构建的真实 Proxy、两个临时 Mac Host/Controller、
独立 NATS 容器及测试 PostgreSQL 中的一次性数据库。Proxy 只监听 loopback，实验
禁用登录认证，不验证 enrollment/凭据安全；没有读取或改写现有机器的 enrollment。
源 workload 是通过实际 Treer API 创建的 `kind=command` Agent；捕获注册使用
**Host 返回的 PID 和 Agent ID**，不依赖 workload 自报身份。启动 gate 为协作式探针，
不是原子的安全保证。

```text
Mac Agent 普通 socket（无代理环境）
  → Apple NE flow PID → 实验捕获 adapter
  → 源 Mac Controller SOCKS（真实 Agent ID）
  → Proxy A：Agent 归属检查 + durable Policy
  → NATS → Proxy B → 目标 Mac Controller
  → Apple machine treer 的临时 host-network TCP 服务
```

两个 Agent 访问**同一个**注册虚拟域名。数据库 Policy 使用 enforce 模式，
`network.connect` 默认 deny，只允许第一个 Agent ID。第一个收到含 `00` / `ff`
的二进制回包，第二个触发实际 Controller 日志 `policy_denied`；不会把连接失败误判成
Policy 成功。源 12 字节、返回 18 字节，独立于 Python 计数，从数据库
`traffic_usage_hourly` 查询得到精确 12 / 18，各 1 frame、无跨 Proxy 重复计数。
这些是 **machine 维度的 relay ledger**，不是按 Agent 存储的公网账本。
同时，源 Agent 通过 `192.0.2.1` 实际访问本地 Controller `/api/health`，返回 200。

此实验有意复用当前 `proxy-env` Controller 的**已注册虚拟主机** Open/Policy/Relay
路径。公网 Direct bypass 并未因此消失；不能把这个成功描述成所有 native egress
都已经强制执行 Policy。目标 guest 是额外临时 TCP 服务，由第二个 Mac Controller
连接；没有将现有 guest Host 接入测试 workspace，也没有验证反向 guest → Mac Agent
服务或 macOS 私有 loopback。Linux namespace/Unix bridge 的实测另见第 1 节。

NATS 使用独立临时容器，因为当前 cluster KV bucket 名不是仅通过 subject prefix
隔离。早期启动检查曾连接现有开发 NATS 并看到旧 projection decode warning，随后
切换独立 broker；该次唯一实验 event stream 已删除。最终成功运行的数据、Policy、
路由和流量全部来自一次性实验环境。

### 3.3 已实测的半关闭缺陷

额外以 `live_macos.py --half-close` 测试：客户端发出 12 字节后
`shutdown(SHUT_WR)`，服务等 EOF 再返回 18 字节。Treer 两个方向的 relay ledger
正常计数，但 native 客户端收到 0 字节，adapter 报 `Server has been shut down.`。
普通帧定长收发通过，不能掩盖该失败。

从上游源码判断，实测现象与
[FlowExtensions.swift](https://github.com/mitmproxy/mitmproxy_rs/blob/58bea7f7e0b7b00c7d91b9997c83299ae3e0922e/mitmproxy-macos/redirector/network-extension/FlowExtensions.swift)
的处理一致：在 outbound EOF 后调用 `closeConnection`，同时 cancel IPC connection、关闭 flow
读写两侧。修复需让两个方向各自完成，只在双侧结束或异常时全关闭，并覆盖延迟响应、
backpressure 和取消。Python adapter 无法恢复已被系统扩展关闭的客户端 socket；
需要修改并签发 helper，或使用上游已修复且签名的版本。本次没有替换签名包，也没有
将实验声明为完整 TCP 行为等价。

测试结束关闭活动 redirector、停止临时 Agent/Host/Controller/Proxy/guest 服务，
删除一次性数据库和 NATS 容器。签名 app 与获准的 system-extension 注册仍留在系统
中，可在系统设置管理；Tailscale、全机路由、DNS 配置及 PF 未改动。

## 4. 达到功能要求的后续落地边界

本次已经证明可接入既有 Treer 数据路径。后续产品化采用单个机器级捕获 helper，
Controller 注册 workload，helper
将流量接入现有 SOCKS/Open 路径。避免每个 Agent 启动一份系统扩展配置。

1. **身份与进程生命周期。** 使用 OS flow audit token 与 Host 进程开始标识关联
   Agent。不能只凭应用名、客户端自报环境变量或 PID 永久缓存。Controller/Host/helper
   自身必须明确绕行；Agent 后代需要原子注册或系统级进程事件，覆盖快速 spawn/exec、
   daemonize、父进程退出、PID 重用。上游已签名 helper 只传 PID/路径，并无注册 ACK；
   当前 probe 的固定等待仅用于验证，不能作为生产保证。
2. **DNS。** 保留 workspace 原始 hostname 才能复用路由与域名 Policy。普通 libc
   查找可能经 mDNSResponder，不能假定一定携带原 Agent 的 PID。本机已验证 DNS flow PID，但共享缓存已导致
   公网 synthetic DNS 回路；需要 split DNS 或受控虚拟 DNS，处理 AAAA、缓存、`.local`、IP 直连和多 workspace 同名；避免把
   系统进程的 DNS 查询错误算到 Agent。公网按域名授权不能仅凭 reverse DNS 或 SNI 猜测。
3. **Policy 与失败。** 捕获的 Agent 流必须走与 Linux transparent 相同的 Open
   授权，不能误用 `proxy-env` 的公网 Direct bypass。原生 backend/注册失效时应阻止
   managed workload 的未授权 egress 或停掉该 workload；上游 helper 控制链断开会
   cancelProxy，不能据此承诺 fail-closed。本机已通过保留地址的实际 API；服务入站隔离仍待实现。
4. **服务与隔离。** native Mac 没有当前 Linux 的每 Agent 私有网络 namespace。
   两个原生进程绑定同一 host-loopback port 会冲突；NE 的出站捕获不会自动解决入站
   AIS/App 端口隔离。需要受管端口分配与 Unix bridge，或明确要求 VM 才获得完整
   loopback 隔离。不能将“出站透明”描述成 Linux 全部隔离行为等价。
5. **流量统计。** 现有 `NetworkRuntime` 的 Direct 只在本机 copy，没有公网计数
   上报；Proxy 的 relay ledger 已有方向性计数。所有平台补 Controller 侧 meter，
   再设计带 workload/stream/方向/累计序号的幂等上报，避免 reconnect 或多个 region
   重复计费。字节口径应明确是 TCP payload，而非 TUN IP packet 含重传/包头。
6. **协议覆盖。** 当前 Linux 产品基线是 TCP + 虚拟 DNS；不能因为 NE 能收到 UDP
   就声称 Treer 已支持 UDP/QUIC Policy。半关闭已发现上游缺陷，须先修复；IPv6、QUIC 回退、非 DNS UDP、防绕行、
   大流量 backpressure、睡眠/唤醒、Wi-Fi 切换、Tailscale 共存应成为 native 验收项。
7. **多 region。** 捕获层无需理解 region，继续发给本地 Controller，由 Proxy/NATS
   处理归属、Policy 与路由。当前本地 NATS 实验验证了两个 Proxy 的协议行为，不等于
   验证了跨地理 region 的延迟、带宽、分区恢复或容量。跨区域 relay 成本需单独测量。

## 5. 本次顺带修复的跨 Proxy 缺陷

真实启用 `TREER_TEST_NATS_URL` 后，原有
`nats_cluster_routes_projection_commands_terminal_and_network` 在等待 Cursor 时失败，
最终读到 lease 超时后的 Closed。原因是 `terminal_ready` 更新了 Controller 所在
Proxy 的 session；跨 Proxy 时实际 browser session 在另一节点，epoch 丢失。

现在由 `handle_cluster_session_delivery` 在浏览器 session 所属 Proxy 消费 Ready
中的 epoch，后续输出带正确 Cursor，不改变 wire schema。增加无需 NATS 的回归测试；
原真实 NATS 用例增加游标等待上限、双向含非文本字节的 payload 和 9/10 字节方向计数。
修复后完整通过，断连仍关闭 terminal 并 reset network stream。

## 6. 检查记录

```sh
cargo test -p treer-agent-server network
TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test \
  cargo test -p treer-proxy network
TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test \
  cargo test -p treer-proxy policy
TREER_TEST_DATABASE_URL=postgres://treer:treer@127.0.0.1:55432/treer_test \
  cargo test -p treer-proxy traffic
cargo test -p treer-proxy terminal
TREER_TEST_NATS_URL=nats://127.0.0.1:4222 \
  cargo test -p treer-proxy nats_cluster_routes_projection_commands_terminal_and_network -- --nocapture
```

未设置 `TREER_TEST_NATS_URL` 时，该 NATS 用例会提前返回并显示 `ok`，不计作真实
跨 Proxy 验证。本次专门设置了该变量，并实际经历失败、修复、通过。数据库使用已存在
的 `treer-postgres-test` 和独立测试 schema；NATS 测试创建唯一 workspace/路由前缀。
原生手动验证命令见 network-lab README，结果分别为 `macos-capture.json`、
`macos-live.json` 和单独保留失败证据的 `macos-half-close.json`。另外执行了 focused
Clippy、Rust fmt、Ruff、Python 编译检查和文档链接检查。
未运行 `just check`、未部署 Canary/生产、未执行跨地理 region 压测。

## 7. 体验等价评估：哪些可以补齐，哪些属于平台边界

本节为实测后的可行性评估，**不是新增通过的测试记录**。对照对象首先是当前
Treer Linux TCP/DNS 数据路径，而非 Linux 内核的全部网络功能，也非恶意多租户隔离。

### 7.1 原生 Mac 与 Linux 的差距

| 能力或体验 | 当前差距 | 等价判断与需要完成的工作 |
| --- | --- | --- |
| 常规 TCP、HTTP、TLS | 捕获与普通二进制收发通过，但不是正式 Controller backend | 可做功能等价；接入正式启动、停止、恢复与错误状态，覆盖长连接、大传输和反压 |
| TCP 半关闭 | 签名 helper 在单方向 EOF 后全关闭 | 明确的实现修复项；Apple 提供独立读写关闭 API，需修复 Swift/IPC 状态机并重新签名实测 |
| 单个 Agent 的自动身份 | Host PID 与 Agent ID 映射已通过 | 可做；完整传递 OS audit token/进程实例身份，避免只缓存可复用 PID |
| 子进程自动继承身份 | 当前只注册等待中的根 PID；无注册 ACK | 难点之一；覆盖 fork/exec/spawn、脱离父进程、PID 重用、事件丢失；需进程事件与待判定 flow 协调，轮询进程树不够 |
| 所有普通 TCP 的 Policy | 只证明了注册虚拟域名；当前 Mac Controller 仍为 proxy-env | 可复用相同 Open/Policy 引擎；必须建立 native 捕获模式，所有受管外连都先授权，不能保留公网 bypass |
| workspace 域名 | 仅实验后缀通过 | 可做用户体验等价；正式 split DNS、缓存生命周期、workspace 区分、地址稳定性以及重启恢复仍待实现 |
| 公网原始域名 | 共享 DNS 缓存可使 flow 只剩 IP | 有条件等价；应传递 Apple remoteHostname，但它只覆盖 connect-by-name API，不能恢复所有 getaddrinfo 后按 IP connect 的原始意图 |
| helper 失效时阻断 | 当前 helper 控制链断开会停止代理；部分错误直接放行 | 可设计更严格故障行为，但尚不能承诺；extension 存活时明确拒绝受管流，extension 自身崩溃/停用还需要独立保护与注入故障验证 |
| 每 Agent 私有 localhost 与同端口绑定 | native 进程共用 Mac 的端口空间 | 无法仅用透明代理原样提供 Linux netns 语义；受管 App 可通过端口分配、Unix socket、逻辑服务名实现接近的产品体验，任意硬编码程序仍有差异 |
| 入站服务、AIS/App、publish | native 原型主要验证出站；未实现 Agent 私有服务桥 | 可做受管服务等价；需要端口所有权、入站身份/Policy、未发布服务可见性与双向访问测试 |
| 流量统计 | relay 已复用真实账本；Direct 和 Agent 维度未补齐 | 平台中立的实现工作；共享 meter、统一 payload 口径、幂等上报、崩溃恢复、重连去重 |
| 多 Proxy / 多 region | 两个本地 Proxy + NATS 已通过 | 可共享相同架构；地理距离、分区、拥塞、故障切换和重复计量尚未验证，不能从本地通过推出容量等价 |
| IPv6、非 DNS UDP、QUIC、ICMP | 未建立完整跨平台支持合同 | 先明确 Linux 产品基线也只有 TCP + 虚拟 DNS；新增协议需共同扩展，不能仅靠 NE 支持 UDP 就宣布完成 |
| 安装、更新、回滚 | 当前借用上游签名 app，尚无 Treer 自有 helper 生命周期 | 能做到一次授权后日常使用顺畅；签名/entitlement、批准、升级迁移、卸载清理是 Mac 特有的维护成本 |
| 睡眠、换网、VPN 共存 | 只验证了 Tailscale 同时启用时的短时实验 | 需要测睡眠/唤醒、接口切换、DNS 变更、多 extension 次序、断网恢复；不能保证现有 TCP 在网络失效后无损续接 |
| 性能与资源成本 | Python + Rust + Swift + IPC 原型，无容量基准 | 可优化为机器级 helper 和共享 Rust 数据路径；吞吐、延迟、CPU、内存、连接数需与同硬件 Linux guest 对照 |

Apple 的 [NEAppProxyFlow](https://developer.apple.com/documentation/networkextension/neappproxyflow)
分别提供 `closeReadWithError` 和 `closeWriteWithError`。因此半关闭具备修复基础，
“当前签名二进制有 bug”不能推出“macOS 不支持 TCP 半关闭”；反过来，API 存在也
不等于修复版本已通过测试。

本机 Apple SDK 的 `NEAppProxyFlow.h` 明确说明：
[remoteHostname](https://developer.apple.com/documentation/networkextension/neappproxyflow/remotehostname)
用于 Network.framework、NSURLSession 等按名称连接的 flow，字段可空。当前 helper
未将它传入 Treer。这是可以补齐的元数据缺失，但 `getaddrinfo → connect(IP)`、
预缓存、自带 resolver、DoH/DoT 等仍需明确域名识别边界。不能通过反查或 TLS SNI
把推测提升为可信域名；Linux 虚拟 DNS 对完全不使用受管 resolver 的客户端也没有
恢复原始域名意图的保证。

`NETransparentProxyProvider.h` 另明确：返回 false 会直接放行；该 provider 的
DNS/proxy settings 不替代系统对应设置。设计拒绝行为与 DNS 方案时必须尊重这个
合同，不能只修改 Python adapter。进程生命周期可评估
[Endpoint Security](https://developer.apple.com/documentation/endpointsecurity)，但它有
独立的授权和部署要求，通知事件本身也不是无竞态的 workload 隔离保证。

Linux 的 [network namespace](https://man7.org/linux/man-pages/man7/network_namespaces.7.html)
隔离的是设备、协议栈、路由和端口空间。原生 Mac 的出站 flow proxy 不会为两个
任意程序各自创造 `127.0.0.1:3000`。为受管服务分配不同实际端口，再用统一虚拟
服务名隐藏差异可改善体验，但程序自己观察 bind/localhost 时仍不等价。
即便 Linux，发布到同一 Host 的端口也共用 Host 端口空间；可重复的是私有 namespace
内的端口，而非两个服务同时占用同一个 host-publish 地址。

### 7.2 Apple container machine 是否就是 Linux

**对 guest 内运行的 Treer Host/Agent，可以按 Linux 平台实现；对整个 Mac 使用
体验，不能推导为与一台常驻 Linux 服务器完全相同。** Apple 官方说明 container
machine 运行在自己的轻量 VM 中，提供持久 Linux 环境和独立网络；这不是以 macOS
socket 模拟 Linux syscall。[Apple container machine 说明](https://developer.apple.com/videos/play/wwdc2026/389/)

| 层次 | 可以得出的结论 | 仍需区分或验证 |
| --- | --- | --- |
| guest 内网络实现 | 实际 Linux 内核、相同 Linux Treer sandbox，不经过 Mac NE helper | 依赖内核配置、TUN 权限、user namespace；当前 treer 已实测具备 |
| Agent 私有 loopback | 已实测两个 namespace 同端口、Unix bridge 与 publish | guest 的 host-loopback 是 VM 自己的 localhost，不是 Mac localhost |
| Policy/身份/region 协议 | 运行相同 Linux Controller 代码，无需另造 Apple 协议 | guest 安装版 1e07080 与研究基线 d170767 不同；仍需统一版本做完整回归 |
| 虚拟网络外部路径 | 从 Mac 经当前 VM IP 访问临时 guest 服务已通过 | VM IP、NAT/桥接、外部入站、MTU、IPv6、VPN 与宿主生命周期仍需验收 |
| 文件系统与工具链 | guest 自有磁盘上的程序是 Linux 程序 | 共享 Mac 目录经 virtiofs；大小写、文件事件、锁、权限与 IO 性能需单独测，不能假定等于 ext4 |
| CPU 与系统集成 | 当前 guest 为 Linux aarch64 | 不是 x86_64 Linux；不能直接运行依赖 macOS SDK、Xcode、Keychain 或原生 GUI API 的程序 |
| 生命周期与可用性 | 持久 VM 可重复启动 | Mac 睡眠/重启、VM 启停与资源配额会影响运行；未做长稳、故障恢复或地理 region 测试 |
| 隔离与信任 | VM 隔开 guest 与宿主内核；guest 内保留 Linux namespace | 共享目录仍暴露被共享的数据；同 guest 的 Treer Agent 当前仍非恶意多租户安全沙箱 |

当前 guest 的 6 项捕获与 3 项服务用例构成直接证据；它们不等于完整已注册 guest
Host 的 Policy/账本/版本升级/断连恢复端到端验收。本次实际 Policy/ledger 集成使用
两个临时 Mac Controller，目标是 guest 临时服务，不能混为另一项测试。

### 7.3 Linux 基线本身也有的缺口

- Direct payload 目前没有中央计量；relay ledger 按机器方向聚合，不是完整每 Agent
  的全部流量账本。周期 flush 的持久性、幂等重试和异常丢量还需要产品级合同。
- 网络 Policy 在 Open 时授权。缓存 TTL 为 5 秒；当前没有因 Policy 变更重新评估并
  撤销所有既有连接的机制。新增连接拒绝不等于已连接的长流立即断开。
- TCP + 虚拟 DNS 不等于 UDP/QUIC/ICMP、原始包和全部内核 socket 行为的支持承诺。
- 网络 namespace 不等于完整文件/凭据安全隔离。当前支持可信或大体可信的机器与
  workspace 成员，详见[安全模型](../security.md)。
- 两节点协议和计数验证不替代真实 region 延迟、带宽、分区恢复和容量测试。

### 7.4 建议的交付边界与验收次序

1. **Linux/Apple guest 保持统一实现。** 对齐二进制版本，补实际 guest Host ↔ 另一台
   Host 的双向服务、身份、Policy、账本和重连验收；增加 guest 启停与 Mac 唤醒测试。
2. **native Mac 先交付正式出站与受管服务体验。** 自有签名 helper、半关闭状态机、
   audit token/域名元数据、单个机器级 helper、注册 ACK、子进程生命周期、统一 Open
   授权及可见的状态/错误，不用 netns 等价来描述这个交付物。
3. **共同补齐 Policy 与统计合同。** 明确 Direct/relay、Agent 维度、存量连接撤权、
   离线/故障时行为和多 Proxy 幂等计量。
4. **验证故障与长期运行。** 注入 helper/Controller/Proxy 崩溃、注册事件延迟与
   丢失、PID 重用、共享缓存碰撞；测试大传输/长连接、睡眠换网、Tailscale 共存，
   再执行跨地理 region 的容量与恢复测试。

若需求包括“任意未修改程序都拥有独立 localhost、可重复硬编码端口和隔离网络栈”，
应把该工作负载放入 Linux guest。需要 Xcode/Keychain 等 Mac 能力的工作负载留在
native Mac，沿用相同的 Treer 身份、服务名、Policy 和数据协议，并明确 OS 隔离差异。
