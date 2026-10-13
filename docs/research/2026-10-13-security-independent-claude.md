# 独立安全审查（Claude）：2026-10-13

范围：对 2026-10-12 五份审查所列“已修复”项做对抗式复核，并审查此后新增面。方法：读源码，
在私有 `CARGO_TARGET_DIR`（`/Users/dooms/treer-security-claude-target`）、独立 PostgreSQL
容器（`127.0.0.1:55499`）和进程内假后端中写一次性复现/回归测试。没有探测线上 canary、用户机器或
真实第三方服务，没有读取或打印真实凭据。分支 `security-independent-claude`，worktree
`/Users/dooms/treer-security-claude`。

状态栏：**已修** = 分支内已有修复和回归测试，等待 core-dev / release-eng 评审后合并；**记录** = 仅记录，
未改代码。改动大小：S（<50 行）、M（<300 行）、L（更大或需设计）。

## 状态更新（评审后）

本文其余部分记录的是审查时（2026-10-13 上午）观察到的事实；下表记录合并状态（本地 main，未 push/部署）。

| 项 | 状态 |
| --- | --- |
| P1-01 `--disable-auth` 监听保护 | 已合并（15862b2） |
| P2-01 不跟随重定向 | 已合并（f6f6eaf） |
| P1-02 A 常量时间 + decoy | 已合并（383bdc7） |
| P1-02 B 账号/管理员预算 | 已合并（5e5a7be、53391db）；按 leader 决定为“按账号、已知限制如实披露、本机 reset 恢复” |
| P1-03 Provider 不可 raw 公开 TCP | 已合并（3fd654e、c3942b1，文案 f4f49a6） |
| N13 下划线头 | 已合并（ee8e41b，端到端回归 db197ea） |
| N14 链路本地/元数据目标 | 已合并（9d0572e） |
| Proxy 不返回明文远程安装命令（N15 的 Unix/Proxy 部分） | 已合并（e3b3ef1） |
| N15 Windows 安装器 HTTPS | 源码已批准，待 release-eng 合并；原生 Windows 执行待门禁 |
| P1-04 路径规则包含性 | core-dev 评审“创建继承”设计中（未合并）；operator-guide 已有说明 |
| N12 防回滚、W1 nginx CSP、N10/N11 | 仅方案，等待 owner 分配 |

## 结论

本轮发现 3 个 P1（均已在分支修复）、1 个 P2 已修，其余 P2/文档项见下。已有的认证、凭据、
路径规则、Soul/会话导入、消息监视设计整体扎实；未发现 P0。最大的剩余风险不是单个缺陷，而是
几条“信任模型写在文档里、部署者容易忽略”的前提（见 P1-04、P2 的 N4/N5）。

| ID | 级别 | 标题 | 状态 | 大小 |
| --- | --- | --- | --- | --- |
| P1-01 | P1 | `--disable-auth` 可在非回环地址上启动 | 已修（0ca40a5） | S |
| P1-02 | P1 | 密码/管理员登录无尝试限额，管理员密码非常量时间比较，账号存在性时序泄露 | 已修（88214cc） | M |
| P1-03 | P1 | 活动 Policy Provider 可经 raw public TCP 匿名暴露 | 已修（84bdc56） | S |
| P1-04 | P1（文档/语义） | 路径规则的 Deny 不是包含边界：未放置节点、子 Agent、Tree 管理权 | 记录 + 文档建议 | S（文档） |
| P2-01 | P2 | Controller 本机服务抓取跟随重定向 | 已修（0961f0a） | S |
| P2-02 | P2 | 其余 P2 项（见下） | 记录 | S–M |

## P1

### P1-01 `--disable-auth` 没有监听地址保护（已修）

- 类别：认证 / 启动环境边界。
- 位置：`crates/treer-proxy/src/main.rs`（`disable_auth`、`listen_address`）；
  `crates/treer-proxy/src/auth/machines.rs::authenticate_machine`、`auth/handlers/middleware.rs::require_admin`。
- 影响：`TREER_DISABLE_AUTH=true` 时 `authenticate_machine` 返回 `server_id=None, workspace_id=None`
  的会话（`allows_server` 对任何工作区/机器都为真），`require_admin` 直接放行，浏览器会话被替换为
  `local` 用户。Proxy 在设置 `PORT` 时默认监听 `0.0.0.0`，operator-guide 的示例也监听 `0.0.0.0`。
  一个误拷贝的环境变量因此等于远程未认证的平台管理员和任意机器身份，可下发 `machine.exec`。
- 本地复现：不需要网络；`require_loopback_for_disabled_auth` 单测覆盖回环/非回环/显式放行。
- 修法：非回环监听地址必须显式 `--allow-unauthenticated-listen`
  （`TREER_ALLOW_UNAUTHENTICATED_LISTEN`），否则启动失败；operator-guide 示例和说明同步。
- 部署侧核对（只读，由 leader/release-eng 做）：确认生产与 canary 环境没有设置 `TREER_DISABLE_AUTH`。

### P1-02 登录无限额（已修；core 审查 #6 的登录部分）

- 类别：认证 / 暴力破解、账号枚举。
- 位置：`auth/accounts.rs::login_with_client`、`admin_login`；`schema.sql`。
- 影响：登录和管理员登录可无限次猜密码；管理员会话是平台管理员。管理员密码使用 `!=` 比较；
  未知邮箱立即返回而已知邮箱执行 Argon2，响应时间可枚举账号。
- 本地复现：`sign_in_attempts_are_limited_per_account_and_administrator`（隔离 PG）。
- 修法：新表 `login_attempts`；先原子递增计数再做哈希（并发猜测不会超限）；每账号/管理员 15 分钟
  10 次，之后 `429 login_rate_limited`；成功清零；多 Proxy 通过 PostgreSQL 共享。未知账号对 decoy
  哈希验证；管理员比较改为常量时间。`security.md` 增加 Supported 声明并写明局限。
- 已知局限（已写入文档）：按账号而非按 IP，攻击者可使已知账号锁定到窗口结束；失败计数行
  可被随机邮箱填充（无全局预算）；不覆盖 machine/OAuth 端点和 Argon2 的全局 CPU 预算（见 N2）。
- 评审：认证边界 + `schema.sql`，需 core-dev 与 release-eng。

### P1-03 Policy Provider 可经 raw public TCP 暴露（已修）

- 类别：访问控制 / 文档保证与实现不一致。
- 位置：`api/network/public_tcp.rs::publish_public_tcp_service`、
  `agent_publish_public_tcp_service`；`api/policy_provider.rs`（选择 Provider）。
- 影响：`security.md` 与 operator-guide 声明活动 Provider 不能公开，但只有 HTTP ingress 路由、
  App access 路由和 Provider 选择（仅查 HTTP）做了检查。启用 `TREER_PUBLIC_TCP_PORT_RANGE` 的部署中，
  `treer service publish <provider-service> --tcp --access public` 把 Policy App 匿名暴露到互联网：
  bundle 内容（规则、组与成员映射）、诊断路由及管理接口（受 token 保护但不再受工作区会话保护）。
  Agent 路径（`ingress.create` 授权）同样可达。
- 本地复现：`raw_public_tcp_is_refused_for_the_active_policy_provider_service`。
- 修法：发布时拒绝；选择 Provider 时拒绝已启用 raw TCP 的服务；每个被接受的公开 TCP 连接再次检查
  （关闭“先发布后选择”和并发竞态）。
- 部署侧核对（只读）：是否配置了公开 TCP 端口范围；`public_tcp_services` 中是否有 enabled 的
  Provider 服务。

### P1-04 路径规则的 Deny 不是包含边界（记录；建议文档先行）

- 类别：授权语义 / 文档。
- 位置：`policy_tree.rs::enrich`、`policy_path_match.rs::selector_matches`、
  `api/agents/agent.rs::execute_agent_create`、`api/workload_tree.rs::authorize_tree`。
- 事实（均由源码核对）：
  1. 路径选择器要求 `placed`；未放置节点（含无放置记录的 legacy Agent）不匹配任何路径选择器，
     于是也不受针对 `/untrusted/**` 的 Deny 约束，落入默认（默认是 Allow）。
  2. 无 `folder_id` 创建的 Agent 放在根目录（`/name`），不继承创建者的路径。受限 Agent 只要
     `agent.create` 未被拒绝，就能创建不在其受限子树内的子 Agent，该子 Agent 不受主体路径 Deny 约束。
  3. 能执行 `agent.tree.manage` 的主体可以把自己或他人移动/重命名进入有 Allow 的子树；默认
     `agent.tree.manage` 对工作区成员和 Agent 为 Allow。
- 影响：operator 以为“把某团队放在 `/untrusted` 并 Deny”就是隔离；实际只有同时 Deny
  `agent.create`、`agent.tree.manage`（以及可创建 App 的动作）并采用 enforce + 显式 Deny 默认时
  才成立。`operator-guide` 只提示“整理受保护子树前先审查规则”，没有说明这些前提。
- 本地复现思路：在 `policy_path_tests.rs` 的两 Proxy 夹具里，装入 Deny `agent.prompt`
  主体 `/untrusted/*` 的 bundle；创建根目录子 Agent 或移动节点后重新评估，decision 变为 Allow。
- 修法：S（文档）。在 `security.md` “Policy And Rollout” 与 operator-guide 路径规则段落加入：
  路径规则是基于当前 Tree 位置的授权，不是容器；使用默认 Deny，并显式限制
  `agent.create`/`app.create`/`agent.tree.manage`；子 Agent 不继承路径。L（可选的产品改动）：
  创建时默认继承调用者 folder，或提供“被包含主体”规则条件。
- 状态：本文档为记录；建议 core-dev 决定是否同时修改默认行为。

## P2

- **P2-01（已修，0961f0a）** Controller `fetch_service_json`（`treer-agent-server/src/proxy.rs`）使用
  reqwest 默认重定向策略。本机 Provider/服务可 302 到本机其他端口或其他主机，让 Controller 代发请求
  并把 JSON 返回给 Proxy。已改为 `redirect(Policy::none())`，回归测试覆盖不会触达重定向目标。
- **N1** `deploy/updater/updater.py::authorize` 用 `!=` 比较 Bearer token（非常量时间）。updater 在
  Compose 内网监听 `0.0.0.0:7420` 且挂载 `docker.sock`（等价宿主 root）。改 `hmac.compare_digest`，
  S，归 release-eng；记录。
- **N2** `authenticate_machine` 对每个机器请求执行 Argon2 `verify_password`，对已知 `srv_` ID 的未认证请求
  就是 CPU 放大器；与 P1-02 的全局预算问题同源。建议对高熵机器凭据改用 `fast_secret_hash`
  （与 Agent 凭据一致）并保留迁移。M。
- **N3** `random_signing_key`（`identity.rs`）用两个 UUIDv4 拼出 Ed25519 种子，固定的版本/变体位使有效熵约 244 位，
  仍足够，但应改用 `OsRng`。S，卫生项。
- **N4** Policy App 管理 token 文件位于机器工作区根（`.treer-policy-admin-token`，0600，同账户）。
  文档已写明不防同账户进程；但同机的所有受管 Agent 运行在该账户并共享工作区根，等于它们都能读
  管理 token（当 Policy 默认允许 `machine.exec`/文件读时尤甚）。建议在文档“Administration security”首段
  明确“同机受管 Agent 可读取该 token，除非 Provider 在独立账户/容器中运行”，并默认建议独立账户。S（文档）。
- **N5** Compose 默认发布 `8787`、`3000` 到所有接口且默认公共 URL 为 `http://127.0.0.1:...`，Proxy 只
  `warn`；NATS 无认证（文档已说明）。建议 `compose.yaml` 默认绑定 `127.0.0.1` 并在文档要求 TLS 终止。S。
- **N6** `launchers/agent/scripts/install.mjs` 的 `npm ci --omit=dev` 未加 `--ignore-scripts`；锁文件中只有
  `esbuild`（可选优化）和 `fsevents` 有安装脚本。依赖由锁文件完整性固定，所以风险限于已固定版本的
  脚本；建议加 `--ignore-scripts` 并验证 `tsx` 仍可运行（未在本地验证，需要联网安装）。S。
- **N7** `apps/email-endpoint/email_endpoint.py`：`deliver()` 与 `notify()` 持有进程锁时调用 Core/邮件网络请求，
  慢后端会阻塞全部 API；`admit_address` 中硬编码了 `proxy` 模式的批准收件人地址（个人邮箱）。
  应改为配置项。S。
- **N8** `apply_agent_names` 仍允许无凭据 legacy 清单注册未知 Agent ID，且按 ID 不校验 `deleted_agents`；
  同工作区另一台机器可上报同一 legacy ID（文档已承认 legacy Agent 不是持久绑定证明）。
  建议设置迁移截止并在之后拒绝无凭据 ID。M。
- **N9** Controller 上传：暂存文件使用默认 umask（0644 典型），上传 registry 无数量/总量/TTL 限制
  （core 审查 #6/#10 已记录）；目录在 `begin_upload` 解析后、`commit` 前被换成符号链接属同账户竞态，
  与文档一致。S–M。
- **N10** Unix Host socket 目录信任：`treer-agent-server/src/service.rs::fit_unix_socket_path` 在路径过长时回落到
  `/tmp/treer-<uid>/`，macOS 在 `TMPDIR` 退化为 `/tmp` 时同样如此；`prepare_socket` 只做 `create_dir_all`，
  不校验已存在目录的属主/权限，`bind` 后才 `chmod 0600`，`connect_local`/`local_endpoint_active` 不校验
  socket 属主。多用户机器上，另一个本地用户可预先创建 `/tmp/treer-<uid>` 并换上自己的 socket，让
  Controller 把命令（包含工作负载凭据环境）交给伪造的 Host。影响限多用户机器（文档的支持层级之外）且依赖长路径
  回落。修法：创建 0700 目录、校验目录和 socket 的属主与无组/其他写权限（`treer-platform` 为 `unsafe_code = deny`，
  需用 `MetadataExt::uid()` 与 `treer-agent-server` 已有的 libc 做比较，或从调用方传入 uid）。M，归 core-dev。
- **N11** 会话 cookie（`treer_session`、ingress 会话）没有 `__Host-` 前缀。若 Proxy 与 Apps 位于同一可注册域，
  任何成员运行的 App 可通过 `Domain=` 抛 cookie（cookie tossing）：`cookie_value` 取第一个匹配，受害者已有的
  host-only cookie 通常更早，所以主要影响“未登录/刚登出”的浏览器造成登录固定。文档已声明 App 是受信任工作区代码；
  建议部署上让 Apps 使用与 Proxy/Console 不同的可注册域（现有示例 `apps.treer.ai` 与控制面同域），并在
  `secure_cookies` 下改用 `__Host-` 前缀。M（需要迁移期同时接受旧名）。
- **N12** Controller 热更新不拒绝回滚：已签名的旧 channel/manifest 会被接受（文档已列为 Unsupported）。
  持久化“已接受的最高版本”并拒绝更低版本是 S–M，且比任何其他 P2 对应的攻击面更直接（分发域被攻陷后仍能降级到有已知漏洞的
  已签名版本）。建议排期。
- **N13（已修，待评审）** 隧道/ingress/工作负载代理只剥离以 `x-treer-` 开头的请求头；CGI/WSGI 风格服务器把
  `X_Treer_Identity_Token` 与 `X-Treer-Identity-Token` 映射为同一变量，客户端可借下划线写法夹带伪造头
  （WSGI 重复值会被逗号拼接）。现有 App 都在 Core 校验 token，所以主要是校验失败/混淆而非绕过；但对任何信任这些头的
  WSGI/CGI App 是风险。修法：`treer_protocol::is_treer_header_name` 折叠下划线和大小写，三处剥离统一使用；测试覆盖。S，
  涉及 Proxy 路由与 Controller 代理，需 core-dev 评审。
- **W1** 自托管 Compose 的 `web/default.conf.template`（nginx）只下发 `frame-ancestors/object-src/base-uri`，没有脚本 CSP
  （Worker 部署有 nonce CSP；2026-10-12 web 审查已披露）。`index.html` 仅有一段内联主题脚本，可在构建后计算其 SHA-256，
  以 `script-src 'self' 'sha256-…'` + 精确 `connect-src`（Proxy http(s)/ws(s) 源）作为静态 nginx CSP；需要浏览器实测
  WebSocket 源匹配，故未在本轮改动。归 web-dev/release-eng，S–M。
- **N14（已修，待评审）** Controller `connect_destination` 对 Proxy 下发的任意 `host:port` 直接 `TcpStream::connect`：
  成员注册（人类路径只做非空校验）的服务可以指向 `169.254.169.254` 等云元数据端点并公开发布，把机器的云身份变成
  可访问服务；在 Policy 拒绝 `machine.exec` 但允许创建服务的 enforce 配置下这是真实升级路径。修法：先解析再连接已检查
  的地址（防 DNS 重绑），拒绝链路本地/元数据地址，机器本地 `TREER_ALLOW_LINK_LOCAL_SERVICES` 显式放行；私有网络
  目标仍允许，文档把“创建服务=网络可达权限”写明。S，需 core-dev 评审；未覆盖 DNS 指向私网的 SSRF（设计允许）。
- **N15（Windows 部分已修，未在 Windows 上运行）** `scripts/install.ps1` 接受任意 `http` 的 `-ArtifactBase`；校验和与二进制同源，
  网络攻击者可同时替换两者并获得代码执行。已改为仅允许回环地址使用 HTTP，并拒绝 URL 内嵌凭据（风格与
  `release_update::normalize_base_url` 一致）。Unix 侧：Proxy 生成的 `curl -fsSL <Proxy URL>/install.sh | sh` 在
  `TREER_PROXY_PUBLIC_URL` 为 http 时同样可被中间人利用，目前仅在启动时 `warn`。建议 Proxy 在公共 URL 为非回环 http 时
  不展示安装命令（或在 UI 显式警告），归 core-dev/web-dev，S。PowerShell 变更未在 Windows 上执行，仅通过 node 安装脚本测试与 Proxy 渲染测试。

## N12 方案页：Controller 热更新防回滚（可交给 core-dev）

**问题。** `release_update::download` 验证 channel/manifest 的 Ed25519 签名和二进制摘要，但不比较
版本。拿到任何一份旧的、仍然有效的签名（过期缓存、被攻陷或被回放的分发域）就能让机器降级到已知漏洞版本。
`service.rs::update` 也不记录“已接受的最高版本”。

**设计。**

1. 状态文件：与 `release-signing-key.pub.pem` 同目录的 `release-state.json`（0600，属主校验，原子写：
   临时文件 + `fsync` + `rename`），内容 `{"schema":1,"channels":{"<channel>":{"key_id":"…","version":"v0.1.13","accepted_at":"…"}}}`。
   按 channel 分别记录；`key_id` 变化（运维替换公钥）视为新信任根，从空状态开始并打印提示。
2. 版本比较：解析 `vMAJOR.MINOR.PATCH[-PRERELEASE]`（与 `scripts/release-r2.mjs` 的校验一致），按 SemVer 优先级比较，
   不引入字符串比较。无法解析则拒绝。
3. 检查点：在 `download` 内、`validate_channel` 之后、获取 manifest 之前比较 channel 版本；低于已记录版本直接失败，
   不下载二进制。相等允许（重装/修复），更高允许。
4. 提交点：仅在 `activate_controllers` 成功后写入新版本；回滚路径（激活失败）不更新状态。更新流程持有对状态文件的
   `flock`，防止并发更新互相覆盖。
5. 受控降级：本机运维显式 `treer update --allow-downgrade vX.Y.Z`（精确目标版本，不是布尔开关），降级成功后
   把状态重置为该版本；在审计/stdout 中输出。不接受环境变量或远端指令触发。
6. 状态损坏或不可读：失败关闭，提示运维手工检查/删除状态文件（等同信任根重置，需要本机访问）。

**可选的发布侧加强（release-eng，M）。** 在 channel JSON 加入签名过的 `issued_at` 与 `expires_at`
（schema_version 2，客户端同时支持 1），让“冻结”（持续提供旧但未降级的版本）也有上限；需要定期重签 channel。

**测试（`release_update_tests.rs` 夹具已有本地签名与假分发服务）。** 旧 channel 被拒绝且二进制端点 hit=0；相等/更高被接受；
激活失败不推进状态；并发两个更新只有一个推进；状态损坏失败关闭；`--allow-downgrade` 仅匹配精确版本；公钥更换重置；
预发布版本排序。

**文档。** 把 `security.md` Unsupported 中“不执行新鲜度/最高版本”改写为：回滚在本机持久状态下被拒绝，freshness
仍不保证（若不做发布侧加强）；`operator-guide` 增加降级与状态重置步骤。

**大小与归属。** 客户端 M（见下方“评审修订”；原“约 150 行”低估了发行身份与恢复 journal），归 core-dev；发布侧可选加强归 release-eng；Windows 就地更新仍不在范围内。

**评审修订（core-dev，2026-10-13）。** 只比较 SemVer 最高版本不够：Canary 可能对同一 `vX.Y.Z` 发布不同 commit，
回放旧 build 不会被拒绝。应改为：channel 带签名的单调 `release_sequence`（或等价的发行身份：commit + 序号），状态文件记录
最高已接受的序号；激活失败不推进；显式本机降级必须审计；更新锁与恢复 journal（崩溃后判定是否已部分安装）一起设计；
公钥变化只能由运维显式重置信任根，不能由下载数据触发。规模按 M 估计，不是简单的版本比较。

## 复核通过（对抗式检查后无发现）

- **Policy 路径匹配**：`glob`（组件边界、终端 `**`、零长度）、`path_prefix`（`/` 与自身包含）、
  `validate_policy_path`（拒绝 `.`/`..`/空组件/控制字符/中间 `**`）、折叠的变量绑定与循环、
  `deny_unknown_fields` 对 `{"var","id"}` 歧义的拒绝。
- **只读诊断双凭据通道**：运行时凭据不走缓存、服务受众精确匹配、令牌所有者重新核对；缺任一凭据、换 Agent ID、
  成员令牌、旋转/吊销/提供者变更均被现有测试覆盖，源码路径与之一致。
- **Policy 管理 token 文件**：逐级 `O_NOFOLLOW`、fd 上校验类型/属主/0600、独占创建 + `link` 发布、
  标记文件防止静默重建、Bearer 比较常量时间、`_identity` 总是剥离 `Authorization`。
- **launcher 来源头与 Host/Origin 守卫**：标记不一致、Origin、`Sec-Fetch-*`、DNS rebinding 的分支与
  文档一致。注意 `instance_id` 和 `agent_id` 可从未认证的 `/v1/manifest`、`/v1/status` 读取，因此
  本机非浏览器进程可伪造 `controller-api` 来源并获得“已证明发送者”权限（文档已声明“值是公开的，本机客户端可伪造”）。
- **消息监视**：只把经正则校验的 message ID 写入提示，正文不进提示；`ack`/`receive` 通过固定 argv 的
  `spawn`，无 shell；输出上限、超时存在。
- **Soul / 会话导入导出**：`safe_extract` 拒绝非常规文件、重复项、`..`、绝对路径、大小炸弹，使用
  独占创建；`session-files.ts` 严格白名单、逐级符号链接拒绝、`O_NOFOLLOW`、哈希与 base64 规范性检查、
  导入目录必须全新。导出的 Host/Origin 检查文档已降级描述为启发式。
- **Agent 凭据**：5 秒缓存 TTL；旋转/吊销在本 Proxy 立即生效，其他 Proxy 最多延迟 TTL；
  `authenticate_bound_runtime` 跳过缓存；常量时间比较；机器与 Agent 绑定检查。
- **Controller 本地 API**：operator 凭据 `ct_eq`；Agent 身份头必须与工作负载凭据成对出现。
- **上传路径**：目录 `canonicalize` 后必须在根下；文件名单一组件；暂存用 `create_new`（O_EXCL，不跟随符号链接）；
  覆盖时 `rename` 替换符号链接本身而不是写穿。
- **Bootstrap**：`--ref` 必须是 40 位小写 SHA；`git fetch` 后核对 `HEAD`；`--source` 被拒绝。
- **危险模式扫描**：全仓库无 `shell=True`、关闭证书校验、`eval`、`innerHTML` 赋值（除经 DOMPurify 的
  markdown 渲染）。

## 文档声明核对

| 声明 | 结论 |
| --- | --- |
| 活动 Policy Provider 不能公开（security.md） | 原先与实现不一致（P1-03）；已修并改写 |
| “不保证全局登录限额” | 仍然成立；已补充账号级限额的准确描述 |
| 路径规则 Tree 事实来自当前工作区 | 与 `TreeSnapshot::load` 一致；P1-04 补充包含边界的前提 |
| Controller 快照/事件校验 Agent 凭据绑定 | 与 `apply_agent_names`/`validate_agent_bindings` 一致；legacy 例外已披露（N8） |
| 令牌/工作负载凭据不是即时吊销 | 与 5 秒缓存和 12 小时 App JWT 一致 |
| AIS 来源头是公开值 | 与实现一致 |

## 尚未覆盖（诚实范围）

web 控制台（沿用 2026-10-12 web 审查）、Windows 安装器、mobile、语音/ASR 与 telegram App、
`traffic`/虚拟网络的数据面、终端流、Host 到 Controller 的 socket 权限、email-endpoint 的 OAuth 回调完整路径
（仅审了 `start/finish_oauth`、`notify`、`deliver`）、Postgres/NATS 的网络访问（只核对了 Compose 与文档）、
更新器的签名验证实现（仅读了 updater.py 的授权与镜像来源）。这些需要后续轮次；Codex 版同名 Agent 恢复后可
交叉验证上述 P1。

## 建议的合并顺序

1. P1-03、P1-01（小、风险低、各自独立）。
2. P1-02（含 schema 变更，建议先在 canary 观察 429 行为；回滚只需忽略新表）。
3. P2-01。
4. P1-04 的文档段落由 core-dev 决定措辞后合并。
