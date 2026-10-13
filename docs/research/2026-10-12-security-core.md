# 控制面安全审查：2026-10-12

范围：Proxy、Controller、Host 的认证、授权、身份路由、凭据与后台信任边界。
基线：`3372b341b5ae914851e08b9ae754da5cc4b9245b`；修复：`7a3a9a0`。
这是源码审查与有限的本地验证，不是完整渗透测试或安全认证。
没有访问 Canary 验收接口、攻击用户机器、读取真实凭据、重启现有服务或发送邮件。
数据库验证只使用 loopback 的隔离测试 PostgreSQL，每个 AuthStore 测试建立独立 schema；
help 回归使用 `env_clear` 的子进程和三个假秘密，不读取继承环境中的配置值。

## 结论和分级

当前能声明的是：可信机器/成员部署中的认证、身份绑定、显式 Policy 检查、
短期 audience-bound 服务令牌和私有本地 IPC。不能声明默认最小权限、恶意成员隔离、
即时 JWT 吊销、完整防篡改审计或抗资源耗尽。关键服务部署前必须配置 enforce Policy、
隔离后端和宿主账户、限制公开入口并保护日志/备份。

P0 指已认证主体跨越它的机器/工作负载绑定，不等于未认证互联网 RCE。
P1 指关键部署的高影响默认值、凭据输出或较长权限撤销窗口；有额外前提的风险明确列出。
P2 指运维/纵深防护缺口，特别是已经不在承诺范围内的同账户能力。
大小：S 为局部修复与回归，M 为多个接口/持久化变更，L 为权限迁移或跨平台机制重构。

| 顺序 | 等级 | 问题 | 本次处理 | 大小 |
| --- | --- | --- | --- | --- |
| 1 | P0 | 机器 inventory/AgentEvent 可冒报别机已绑定 Agent | 修复、隔离 DB 回归 | S |
| 2 | P0（需知道另一请求 ID） | CommandResult 没有核对 pending 目标机器 | 修复、状态层回归 | S |
| 3 | P1 | 默认允许、monitor 不执行拒绝 | 迁移方案；不直接翻转默认值 | L |
| 4 | P1 | JWT 与会话/Agent 吊销未形成统一在线校验 | 边界说明与方案 | M |
| 5 | P1（本地诊断） | Proxy help 打印管理员密码/NATS URL env 值 | 修复、隔离子进程回归 | S |
| 6 | P1 | 登录及受认证资源操作缺乏统一限额 | 源码证据与方案 | M/L |
| 7 | P1（需后端网络访问） | NATS 被当成可信命令总线，Compose 无 broker auth | 部署条件与方案 | M |
| 8 | P2 | 数据库/备份保存可用会话与签名秘密 | 保密要求与哈希迁移方案 | M |
| 9 | P2 | 审计尽力写入，缺少完整性/防篡改保证 | 修正文档与方案 | M/L |
| 10 | P2（需同账户文件操作） | 上传 canonicalize 与 rename 存在路径竞态 | 如实限制与方案 | L |
| 11 | P2 | 启动恢复文件含明文凭据，读取时不重新验证权限 | 保密要求与方案 | M |
| 12 | P2 | 缺少统一敏感日志字段约束 | 诊断边界与方案 | M |
| 13 | P2 | 锁文件包含 TLS/可选依赖 advisory | 定向 patch 升级，保留未修 advisory 说明 | S |

## 1. P0：Controller 可以冒报其他机器的 Agent 身份（已修）

**现象/根因。** `agent_socket.rs` 对 Snapshot 只验证外层 server，
`auth/machines.rs::apply_agent_names` 原先直接持久化嵌套 Agent 的 workspace/name。
`state/core.rs::apply_snapshot` 过滤内层 workspace/server 后仍按 Agent ID 替换路由；
攻击机器可把自己的 server 填进别机的 Agent ID。增量 AgentEvent 原先也只验证自报的
workspace/server，不检查 `agent_credentials` 中不可迁移的 durable binding。

**本地复现。** 隔离 AuthStore 种入 workspace-a/server-b 的 agent-a 凭据，提交
workspace-a/server-a inventory；另一案例直接在 A 的 inventory 中放 B 的 workspace。
新增两个 `auth::tests::controller_snapshot_*` 在旧代码均失败：应拒绝却成功。
修后验证跨机器/跨 workspace、无凭据旧名字的跨 workspace 冲突、合法及未知 legacy
inventory；也测试增量 event 的绑定拒绝与合法通过。测试不输出凭据。

**影响。** 已认证的恶意机器能够污染名字或同 workspace 的 Agent 路由，使发往受害
Agent 的 prompt/输入被交给攻击机器、伪造返回状态。没有证明未认证访问、窃取 durable
credential、或在真实受害机器上执行命令，不作这些声明。

**建议/处理/大小。** S，已修。批量校验嵌套作用域和 durable credential 的 workspace/server，
并检查旧 `agent_names` workspace；在名字写入、路由修改前拒绝，报
`agent_identity_mismatch`。增量 event 共用绑定检查，数据库不可用时闭合。
匹配的已吊销身份仍由原删除流程处理；未知 credential-less legacy 身份保留兼容。
下一阶段应明确 legacy inventory 的迁移终点：未知 ID 不是 durable 机器归属证明。
每次 event 增加数据库查询，需要观察容量，再考虑带版本/吊销机制的缓存，不能直接跳过检查。

## 2. P0：命令完成仅以 command ID 关联（已修）

**现象/根因。** 原 `agent_socket.rs` 对 CommandResult 只检查连接完成注册，
`state/commands.rs::complete_command` 按 command_id 删除 pending，未检查其机器目标。

**本地复现。** 新状态层测试创建 owner/intruder 两台假机器并发起一条 owner 命令，
把已知 command ID 的假结果从 intruder 提交；修后拒绝，pending 保留，非当前连接拒绝，
真正 owner 的结果成功返回。测试主动提供 ID，不证明实际攻击者能观察其他命令 ID。

**影响。** 若另机获得该请求 ID（调试信息/后端访问等），可伪造跨机器命令结果或提前
完成请求。UUID 难猜降低可利用性，但不是目标身份授权。

**建议/处理/大小。** S，已修。生产完成入口显式接收已认证 workspace/server/connection，
核对当前连接及 pending 目标，再取出结果；无作用域的快捷入口仅在 `cfg(test)` 下存在。
随机未知 ID 继续无副作用忽略，避免为已完成重放制造不必要协议错误。

## 3. P1：没有 Policy 等于允许，monitor 不 enforce

**现象/根因。** `policy.rs` 的未匹配规则默认为 Allow，Monitor 强制返回 Allow；
`policy_provider.rs` 的缺省是可见的 Treer Default monitor/allow。
Provider 的不可用 fail_closed 和没有配置 Provider 是不同状态。

**本地复现。** 本次跑过现有 Policy 单元测试，包含 monitor 的拒绝规则仍返回 Allow，
以及 API 的 enforce 拒绝测试。没有尝试实际机器执行；源码确认 machine.exec、file.write、
startup.manage 等 action 在缺省下没有最小权限限制。这个行为已文档化，不是认证绕过。

**影响。** 仅有“登录”和“加入 workspace”不足以安全托管部分可信成员的关键宿主。
受认证成员默认能调用广泛控制能力；允许 machine.exec 本身就是授予机器账户代码执行。

**建议/大小。** L，不直接翻转。新 workspace 使用显式 enforce/deny 默认：普通主体只保留
自己的必要只读/身份操作，跨 Agent 和机器 mutation 默认拒绝；Owner 保留可审计的 Policy
管理/恢复路径。现有 workspace 先展示实际权限差异、生成迁移预览，再由 Owner 接受。
Monitor 必须显著标成不限制访问；需要行为矩阵和 browser/operator/Agent/App 的回归。
不要用尚未签名的 App provider 成功启动作为唯一管理员救援前提。

## 4. P1：Token 的过期、身份吊销和登录会话吊销是三件事

**现象/根因。** `identity.rs` 有签名、issuer/audience、expiry/future-iat 检查；
Agent JWT 和 human forwarding 为 60s，human App OAuth 为 12h。
`api.rs::verify_workload_identity` 只做 JWT 验签，未接入 AuthStore。
`api/app_identity.rs` 当前重查 service 和 Human membership，但 Agent 在线吊销检查不统一，
也未把 Human App JWT 绑定签发时的 login session 或 auth epoch。

**本地复现。** 跑过 Identity 的 TTL/audience/错误签名测试，AuthStore 的 logout、密码重置、
Agent/machine/workspace 吊销，以及 API 的 membership revoke 测试。
额外验证方法：用假 session 签发 OAuth App JWT，再 logout，membership/service 不变，
调用 verify_app_identity；源码显示该路径仍只检查后两者。这个组合没有新加端到端复现，
结论依据明确的校验入口，不宣称验证了所有 App 实现的接受行为。

**影响。** 已被窃取的 App JWT 在登出/重置后可能仍有最多 12h 的服务 scoped 权限；
Agent JWT 在吊销后部分接口仍有最多 60s 窗口。Bearer 原本允许重放，不能称单次令牌。
Apps 自行离线验签的撤销延迟也不能由 Proxy 修复自动消除。

**建议/大小。** M。统一在线身份验证 helper，检查当前 service、Agent credential/machine
binding 和 workspace 状态；Human 引入 auth_epoch/session binding，登出/密码重置/管理员
吊销递增并验证。明确 offline token lease 与 online introspection 两种保证，缓存需有上限。
短期缩短 OAuth TTL 会改变 Apps 续期契约，不在审查里擅自切换；60s forwarding 保持。

## 5. P1：配置里的秘密出现在 Proxy help（已修）

**现象/根因。** Clap 对 ADMIN_PASSWORD 和 TREER_NATS_URL 启用了 env，却没有
hide_env_values；DATABASE_URL 等字段已有隐藏选项。

**本地复现。** `tests::proxy_help_redacts_configured_credentials` 启动 env_clear 子进程，
种入假密码、带假秘密的 NATS/DB URL；旧代码断言失败。修后不显示三个假秘密，仍显示
对应变量名。这不是远程 endpoint 泄露，也未检查真实环境的秘密。

**影响。** 安装/排障记录 `--help` 输出时，可把管理员密码/NATS credential 带进共享日志。

**建议/处理/大小。** S，两个 env 字段增加隐藏选项和子进程回归。hide_env_values 不隐藏
用户显式打印 argv 或配置文件，不作“所有日志绝无秘密”的承诺。

## 6. P1：资源耗尽防护不完整

**现象/根因。** `auth/accounts.rs::login_with_client` 直接做同步 password hash 验证，
没有统一的 IP/account 尝试限额或隔离的计算预算。密码重置/email 现有局部限额不等于
登录或控制面整体限额。`agent_socket.rs` 使用 unbounded outgoing channel；Controller
upload 限每 chunk，但 pending registry/累计 bytes 没有统一容量/超时清理契约。

**本地复现。** 本次只读取实现并运行正常/错误密码测试，没有做负载洪泛或磁盘填满。
安全复现方法是 hermetic login fixture 并发少量错误密码、假慢 WS consumer、临时目录内
有限上传，并记录 CPU/队列/临时文件上限；该容量测试未执行。浏览器 HTTP body 的上限、
machine exec timeout/output 截断是局部保护，不应混称全局配额。

**影响。** 公开登录入口可消耗计算；已认证机器/成员可造成内存、工作负载数量、磁盘和
消息积压。权限失窃后的影响不仅是保密性。没有确定可用的未认证大 payload 攻击链。

**建议/大小。** M/L。分 IP/account 速率与全局 Argon2 semaphore + blocking pool；有界 WS
channel + 断连/重新同步策略；每 workspace/machine 的并发命令、上传数量/总量/TTL、
消息/工作负载预算与指标。避免只靠反向代理限流，因为受认证 websocket 仍长连接。

## 7. P1：后端总线的访问就是控制面信任

**现象/根因。** `compose.yaml` NATS 启动没有认证/TLS，映射端口是 loopback，但 Compose
内部网络服务能接触它。`cluster_bus.rs` 使用该总线交换 ownership/命令/投影，未另做每条
消息的租户签名。Postgres schema 未配置 RLS，workspace 隔离由 API/auth 查询维护。

**本地复现。** 只审查 Compose 与总线源码，未连接已有 broker、未向运行中的总线发消息。
本地复现应启动全新临时 NATS，用两个假 workspace 和独立 subject prefix 尝试匿名 publish；
该场景未执行。不把 loopback 映射说成互联网公开的 unauth endpoint。

**影响。** 获得后端网络权限/同网络 App 被入侵后可能伪造控制面消息；Postgres 读写账号
可跨租户读取/修改数据。这里是 trusted-backend 部署条件，不是已复现公网 IDOR。

**建议/大小。** M。部署模板提供 NATS auth、按 Proxy role 的 subject ACL、TLS/external
地址检查；不要把普通 workspace App 放进 backend 网络。SQL 最小权限账户、独立管理迁移
角色，必要时评估 RLS；RLS 不替代持有多个租户权限的 Proxy 的认证和路由检查。

## 8. P2：数据库/备份的读权限等于部分在线身份权限

**现象/根因。** `schema.sql` 的 login/admin/ingress sessions 用 token 本身为键；
IdentityIssuer 的签名 material 在 Proxy metadata 中恢复。密码 Argon2、machine/workload
凭据哈希存储，不代表所有 token 都哈希存储。

**本地复现。** schema/IdentityIssuer load 源码核对；未导出任何数据库真实行或 signing key。
可在临时 AuthStore 创建假 session，只断言存储键是否等于生成的 token，不打印 token。
这次没有新加该测试；现有 session 创建/吊销测试已通过。

**影响。** DB/备份泄漏包含可用 opaque session；签名 key 泄漏权限更大。DB 管理员原本
可信，不能把这一点包装成未经认证的 DB 访问漏洞。

**建议/大小。** M。会话使用 lookup ID + secret hash，迁移短期兼容并清理旧 token；
签名 key 访问分角色或使用现有 operator 管理的 secret provider，明确轮换/备份恢复步骤。
不建议为 Treer 增加一套自研 Vault；先有受限数据库、加密备份和密钥轮换预案。

## 9. P2：审计并非保证完整或不可篡改

**现象/根因。** lifecycle、startup、exec/upload 等多条 route 先完成 mutation，再写 audit，
失败只 warn（`api/agents/*`、`api/workloads.rs`、`api/apps.rs`）。数据库管理员可修改 audit
表；monitor/deny 决策审计还未统一。组织管理的一些事务审计较强，不能覆盖推论所有 route。

**本地复现。** 源码核对各 warn-and-continue 分支，既有 body-free audit 测试通过；没有
为运行中的数据库注入故障。隔离复现应让假 Controller 完成 mutation，再令临时 audit
writer 返回错误，确认返回成功但没有事件；本次未增加该故障测试。

**影响。** 事故调查可能缺少“谁做了什么”；无法提供合规完整性或防后端管理员篡改保证。

**建议/大小。** M/L。mutation intent + durable outbox、结果状态补写、独立审计可用性指标，
再提供受限 append-only role/外部日志 sink。远程副作用不能伪称和 PostgreSQL 一次原子
提交；必须表示 pending/unknown/completed。本次删除 security.md 的过强 append-only 表述。

## 10. P2：文件上传的路径检查不是防同账户竞态的 sandbox

**现象/根因。** Controller `resolve_machine_directory` canonicalize 并做 root containment；
begin_upload 用 create_new 临时文件，commit 按保存的 path rename。没有持有目录句柄，
另一个同账户进程能在检查与 commit 之间替换父目录；overwrite=false 也存在检查/rename
间的并发目标创建窗口。静态的外部 symlink 被 canonicalize 拒绝，不能混为简单路径绕过。

**本地复现。** 实现核对；未对用户目录操作。安全复现应在新 tempfile root 中 begin_upload，
重命名 staging 父目录并换成指向另一个临时目录的 symlink，再 commit；所有路径属于测试。
此竞态没有新增 Controller 级复现，故不宣称可仅用公开上传接口制造 symlink 或越权。

**影响。** 协作/恶意同账户程序能改变文件落点；同账户已能直接写这些文件，所以不是新
的同账户权限提升。受限 file-only 权限未来若有防恶意代码保证，当前实现不够。

**建议/大小。** L，跨平台 descriptor-relative/no-follow 路径和不可替换的根目录句柄，
原子 no-replace 安装与 Windows 等价机制。在此之前如实保持 accidental containment
承诺，不扩成 hostile filesystem containment；通过外部独立账户/容器实现实际隔离。

## 11. P2：启动配置与凭据的落盘边界依赖账户和备份权限

**现象/根因。** `startup_store.rs` 的 StoredAgentStartup 包含 plaintext workload credential，
写入临时文件明确 Unix 0600 并 atomic rename；read_store 使用普通 fs::read，未验证已有
文件的 mode/owner/no-follow。命令 env/argv 是任意应用配置，Core 只能拒绝已知 Treer
凭据 literals，不能识别所有第三方秘密。之前环境清洗修复是防跨 Agent 继承污染，
不是隔离同一机器账户或加密文件。

**本地复现。** 本次核对存储实现和现有文档，不读取本机 startup store。可在临时目录
写 fake startup、断言 0600，再以 0644 旧文件 load 验证未拒绝；这次未执行新增存储测试。

**影响。** operator 的宽松备份/复制或旧文件权限能暴露多个 workload 身份；同账户读
credential 是当前信任模型，不是 remote credential theft。

**建议/大小。** M。load 时核对账户/权限与 symlink，提供只显示问题不打印秘密的修复
诊断；父目录权限与 backup guide；第三方秘密放受保护的文件/secret provider。
保留私有明文恢复凭据可以是明确的产品选择，不宣称启动规格绝不持久化任何秘密。

## 12. P2：缺少覆盖所有日志/错误路径的 secret 约束

**现象/根因。** 默认 TraceLayer 可在 debug span 记录 URL，数据库错误通过 tracing 输出；
任意 argv/env/第三方返回信息没有统一类型级 redaction。auth DB 错误的公开响应为通用
database_error，而不是原始 SQL 错误；这项已有正确边界。Structured Core Message body
和常规 lifecycle audit 不记录 prompt/命令内容，也不能推广成所有 debug 输出保证。

**本地复现。** 已执行第 5 项假值 help 泄漏回归；其他仅源码查找，没有采集生产日志。
下一步使用假 token/query/数据库错误 capture tracing，覆盖 info/debug 两级；本次不声称
已经证明所有日志零泄漏，也没有发现真实秘密出现在公开错误响应。

**影响。** 开启排障日志、转发日志或打印 Debug 结构时，应用自带秘密可能进入较宽访问域。

**建议/大小。** M。敏感值采用不实现原始 Debug 的类型、HTTP span 记录 path 模板而不是
query、带秘密的 URL 显示安全摘要、日志测试注入 sentinel、明确支持日志安全采集步骤。

## 已验证的边界与剩余验证

### 依赖扫描补充：P2（第 13 项）

**现象/根因。** release-eng 提供的本地 cargo-audit 0.22.0/RustSec 快照
`57dcc0f2192e025138907a495c1ac9a7e1e3e123` 命中 7 条 advisory。
`cargo tree -p treer-proxy -i rustls@0.23.43` 确认 TLS 路径实际使用 rustls；
RUSTSEC-2026-0285 是 TLS 1.3 encryption-level 验证缺口，advisory 明确 transcript 仍经过
认证，不能据此称中间人可伪造完整握手。quinn-proto 有五条资源耗尽 advisory，rsa 一条
未修的 timing advisory。`cargo tree --workspace --target all -i` 确认后两者在当前默认
workspace 构建图没有路径；锁文件包含可选 dependency 不等于已启用线上功能。

**本地复现/处理。** 无 exploit traffic。用真实 Cargo dependency graph 加只读离线
RustSec 数据库核对，定向更新 rustls 0.23.45、quinn-proto 0.11.18；只变两个 package 的
version/checksum，不引入功能。升级后完整 Proxy tests 仍 220 passed/1 原有 ignored。
离线 cargo-audit advisory 剩 1 条 rsa RUSTSEC-2023-0071，不能称 audit clean。
第一次 Mac 直接运行另一任务的 Linux audit binary 失败；改在 network-none 的临时 Linux
容器中运行。初次没有 crates.io index 缓存，yanked 查询不可用，不算完整 registry 审计。
随后只读挂载 release-eng 的公开 index 缓存重跑，没有缺失 index 错误；结果仍 1 条 rsa，
另有 unmaintained/yanked 警告。这是固定离线快照核对，不是对今日全部 registry 的实时证明。

**影响/建议/大小。** S，实际 TLS 路径修补；未启用的 QUIC 提前修补锁文件以免未来启用时
带入缺陷。rsa 未修，保持非选用路径，未来开启 MySQL/RSA 功能必须重审；保留 bincode
unmaintained/yanked 等维护警告。不能把依赖升级宣传成已经证明所有 TLS 用法无漏洞。

本次 Proxy 完整 binary tests：220 passed，1 个原有 public TCP 分配测试 ignored；
该测试需要显式独立公网 TCP range，与本任务认证修复无关，没有为本任务新增 skip。
覆盖现有 credential rotation/revocation、enrollment single-use、OAuth PKCE/single-use、
service audience、membership revoke、跨 Agent Policy 拒绝、service owner binding、queue
sender 证明与取消权限、public ingress identity header 清洗等。
没有发现新的已复现“未认证跨 workspace IDOR”；测试通过不等于证明所有路由无漏洞。

修复前明确失败的三类断言：foreign durable inventory、nested foreign identity、help sentinel。
CommandResult 增加假机器状态层回归；它不是真实网络攻击。未新启动真实 Host/Controller，
未做 Linux/Windows runtime 验收；本次仅修改平台无关的 Proxy Rust 认证/状态代码。
格式检查、Proxy all-targets Clippy `-D warnings`、文档检查通过；合并前由 release-eng
交叉评审。Controller all-targets Clippy 也通过。release-eng 对修复和定向依赖升级的源码
评审通过，并以清空环境/全新 HOME/隔离 PostgreSQL 独立执行 3 个针对性回归通过
（Core Message `msg_1e2b759d1a8e4198b8d968a7ba5ee4f5`）；没有独立重建完整 workspace。
Host Unix IPC 的 socket 0600 和 Windows current-user/SYSTEM DACL、拒绝远端 named pipe
客户端，以及 Controller 的 local workload credential 验证也检查了源码；本次没有重跑
这些平台的真实进程攻击测试。NATS 可选测试可能在未提供 TREER_TEST_NATS_URL 时提前返回，不把整个 suite
通过当成本次真实 NATS 集群或资源预算验收。

下一阶段优先顺序：修复发布与 legacy inventory 退出计划 → enforce 默认迁移 → JWT 在线
撤销契约 → 登录/连接/上传预算 → 后端部署隔离 → 审计可靠性。禁止用更强的宣传语替代
这些尚未完成的工程和外部运维条件。
