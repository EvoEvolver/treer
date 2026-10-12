# Treer 架构审查：2026-10-12

审查基线：本地 main `a221659`，以及本分支的 virtual-host revision 回归修复。
范围：通读维护中的 architecture/security/quality 文档，沿 Proxy、Controller、
Host、runtime、platform、protocol、CLI 的调用边界核查；launcher/App 只核对
与 Core 相接的契约，不替这些 owner 重写实现。未部署、未重启任何现有服务。

总体判断：Proxy / Controller / Host 的责任拆分值得保留。主要缺口是控制链路的
调度、分布式所有权的故障收敛、投递与执行的区别，以及启动/升级的可重复性，
不是缺少透明网络或沙箱。应继续提供认证、互联、稳定身份和恢复意图；不要为了
解决这些问题再做 OS 隔离、平台网络配置或应用构建平台。

严重度：P1 为可能造成机器/任务不可用、重复副作用或权限污染的问题；P2 为
运维成本、恢复或产品语义问题；P3 为一致性与文档问题。没有在这次审查中证实
新的 P0 漏洞。大小：S 为单个边界内的改动，M 为数个模块及故障测试，L 为
跨组件协议/生命周期迁移。大小不是交付日期承诺。

证据标记：**确认**表示源码或回归测试直接证明；**风险**表示源码存在该交错，
尚未用故障注入复现；**现场线索**来自 leader 的报告，不冒充独立现场测量。
已修的事故仍记录其设计教训，避免把已解决的问题当成当前故障。

| 顺序 | 严重度 | 问题 | 大小 | 本次处理 |
| --- | --- | --- | --- | --- |
| 1 | P1 | 控制连接等待业务命令，心跳和恢复可能相互阻塞 | M | 方案 |
| 2 | P1 | NATS 租约、快照与本地连接不是同一原子所有权事务 | M–L | 方案；过期租约恢复已在 main |
| 3 | P1 | 重试与 generation 不能替代持久化操作去重 | M–L | 方案 |
| 4 | P1 | 启动环境隐式传递身份和配置，易受调用者污染 | M | 已有清洗保留；后续方案 |
| 5 | P2 | Message、prompt、queue、bridge 的交付/完成语义不一致 | M | 方案；修正文档 |
| 6 | P2 | 无变化的 virtual-host 刷新推动 revision 和日志 | S | 修复并回归测试 |
| 7 | P2 | 文件日志无容量上限，日志所有权跨进程 | M | 方案，避免错误的在线轮转 |
| 8 | P2 | Host 冷重启是整机工作负载中断 | L | 方案和明确边界 |
| 9 | P2 | 版本兼容是局部 capability，而非机器级升级事务 | M–L | 方案 |
| 10 | P2 | App 稳定身份不等于稳定运行版本 | M | 方案，交给 App/发布 owner |
| 11 | P2 | 在线、可路由、已恢复、harness 可用混为一个健康判断 | M | 方案 |
| 12 | P3 | path/name/folder 已统一，但仍有命名域与文档残留 | S–M | 修正文档；不改寻址契约 |

## 1. P1：控制连接的业务处理存在队头阻塞（确认）

- **现象**：一个慢命令可能拖慢整机心跳、WebSocket ping、输入/输出和连接上的
  后续命令；恢复校验也可能在同一循环中占用时间。
- **根因**：[Controller Proxy 连接循环](../../crates/treer-agent-server/src/proxy.rs)
  在 `tokio::select!` 的 Command 分支中直接 `self.execute(envelope).await`。
  心跳和恢复重试也在同一循环内。部分 exec/AIS/HTTP 操作允许等待数十秒，
  与活租约 TTL 处于同一量级。异步 await 不阻塞 Tokio 的其他 task，但会阻塞
  这个连接 task 的其他分支。尚不能据此断言它就是当天 Mac 断连的触发原因。
- **影响**：业务慢会表现成控制面离线；离线又触发恢复和重试，放大抖动。
- **建议**：连接 task 只负责读写帧、心跳和调度。业务请求进入有容量限制的工作
  队列，结果通过单独输出通道返回；控制帧优先于大量输出。保留 recovery_lock、
  generation fencing 和每目标有序性，不能简单地对所有命令无界 spawn。
- **大小/验收**：M。用慢 AIS/exec 和慢校验假实现验证等待期间心跳、流量和其他
  目标命令继续运行；测队列满、断连、取消与重复命令，不能只测 happy path。

## 2. P1：分布式所有权仍有多步提交窗口（部分确认、部分风险）

- **现象**：过期 lease 曾使正常重连报 duplicate；这个恢复缺陷已修。仍应检查
  新 owner 已接管时，旧连接的迟到写入/失败清理能否影响新 owner。
- **根因**：[cluster.rs](../../crates/treer-proxy/src/cluster.rs) 将 lease、snapshot
  和 control projection 写到不同 KV bucket；`publish_snapshot` 检查当前 lease
  后再异步写 snapshot，不是跨 bucket 原子事务。旧发布者检查通过后被接管，
  再迟到写入旧 snapshot 的交错仍可能存在。watcher 的 owner 校验会拒绝旧
  snapshot，但不能保证最后保存的是新 owner 的有效 snapshot。
  [register_server_instance](../../crates/treer-proxy/src/state/core.rs) 在本地替换
  connection 后 await cluster claim；失败路径按机器 key 删除 connection，
  没有再次比较 connection_id。并发注册时失败者可能清理后来的成功者。
  这些剩余交错是源码风险，尚未证明发生过；不要误报成现场根因。
- **影响**：短暂丢失远端 inventory、错误离线或错误清理获胜连接。watch stream
  结束后的重新建立和全量校准，也需要故障测试覆盖。
- **建议**：为每机器所有权变更设独立串行边界；所有清理必须 compare-owner。
  snapshot 以 owner/epoch 分区或在消费时读取当前 owner 对应记录，避免旧 owner
  覆盖新 owner 的唯一 key。NATS 是在线路由投影，PostgreSQL 才是持久意图；
  watch 断流后要重建和校准。不要取消认证或把所有 duplicate 都无条件忽略。
- **大小/验收**：M–L，信任边界需 release-eng 评审。用真实隔离 NATS 的屏障测试
  注入 claim 失败、接管中途 publish、迟到 heartbeat/release、watch 断流与恢复。
  现有 main 的 TTL 恢复、续约重建、旧 owner fencing 测试是基础，不是全部证明。

## 3. P1：操作结果去重缺少统一持久边界（确认边界，风险需复现）

- **现象**：连接恢复期间的重发可以关联原 command ID；调用者重新提交或进程
  更换后，不能普遍保证同一个逻辑操作只执行一次。
- **根因**：[Proxy commands](../../crates/treer-proxy/src/state/commands.rs) 和
  [Controller command_cache](../../crates/treer-agent-server/src/proxy.rs) 的 pending/
  结果记录主要在内存中；Host 的有界 operation cache 属于 Host 生命周期。
  新的 HTTP/CLI 调用会生成新 command ID。generation 防止旧进程事件覆盖新进程，
  不代表同一个 restart 或任务不会被再执行。AIS 持久 submission journal 和
  Core Message idempotency 已改善各自边界，不能推广为整个系统 exactly-once。
- **影响**：丢失响应后的人工重试可能再启动、再重启或再次输入有副作用的任务。
- **建议**：为有副作用的 API 提供 caller idempotency key，绑定主体、目标与
  请求摘要；明确 queued/running/done/failed/unknown。只持久化必要的操作记录，
  不持久化凭据。跨 Host 冷启动无法确定已执行的 shell 副作用时返回 unknown，
  不盲目补执行。复用现有 Message/AIS journal 思路，避免创建另一套投递队列。
- **大小/验收**：M–L。测试副作用之后、响应之前断连/崩溃，重试内容冲突，记录
  过期，以及 Host 更换；按操作说明 at-most-once 或可安全重试的边界。

## 4. P1：环境是隐式身份总线（旧泄漏已修，剩余设计边界确认）

- **现象**：前两天受管 Agent 启动服务，曾把身份、旧 loopback 代理和 UI 会话
  变量传入 Host/Controller；HTTP 恢复校验失败、工作负载恢复被推迟。
- **根因**：原先直接继承整个调用环境。目前
  [共享 environment filter](../../crates/treer-platform/src/environment.rs) 已在
  服务入口、子进程和工作负载继承处清洗；Controller HTTP 已忽略隐式代理，并
  会记录恢复失败、随后重试。当前是明确 denylist 加 UI 前缀，仍允许普通环境
  和外部代理，因此未来新增注入变量时必须维护清洗规则。
- **影响**：历史故障不是新 bug；剩余问题是部署配置依赖“是谁调用了 start”。
  同 OS 用户的环境凭据和 loopback 身份入口也不是可信进程隔离，这符合目前
  产品不承担沙箱的边界，不能称其为安全容器。
- **建议**：服务启动配置显式列出所需变量，和 workload launch env 分开。
  建立 service、workload、installer 三种继承配置及共同回归 fixture；今后注入
  变量必须同提交更新清洗测试。CLI 身份继续独立于机器/operator 凭据；有隔离
  需求时提供显式凭据入口和外部隔离方式，不尝试 PID 认证或自动创建沙箱。
  不能立刻改为极小全局 allowlist，破坏用户 provider/工具配置。
- **大小/验收**：M，信任边界需 release-eng 评审。沿用真实污染环境、冷启动、
  热 Controller 替换及 workload 新身份测试，补新增配置来源的迁移测试。

## 5. P2：四种投递机制没有一个一致的用户完成语义（确认）

- **现象**：Message 持久化后还要 receive/ack；prompt 让 harness 执行；AIS queue
  排在下一回合并可通知；旧 bridge 本地队列是另一种消费模式。调用者容易把
  “送到了”“处理了”“回合结束”“任务成功”当成一件事。
- **根因**：[message_store](../../crates/treer-proxy/src/message_store.rs) 正确地区分
  Message 与 delivery acknowledgement；新的 queued prompt/submission/notify
  又属于执行接口。历史 bridge 兼容命令和部分旧 `AgentCommand::Prompt` 调用
  仍存在，例如 voice 入口；不是所有入口都自动具备 CLI 的默认 queue 语义。
  完成通知写入 Core Message，不等于接收方 harness 自动读到了通知。
- **影响**：协调者仍可能轮询 inbox；显式插话与任务队列行为随入口不同；错误
  ack 时机会丢掉无人处理的任务，重新投递又可能重复执行。
- **建议**：保留两个概念：Core Message 是可审计持久内容，AIS submission 是
  执行请求；用关联 ID 连接它们。提供 launcher 可选 inbox relay，先持久化
  submission 再 ack，并明确 notify 的 inbox 目的地。launcher-dev 已有独立
  message-watch 待评审实现；应复核并复用，不能把它说成 main 已上线。旧 bridge 标为兼容迁移，
  新集成只走规范 AIS。逐步让语音/网页/CLI 显式选择 queue 或 steer；PTY 只能
  显式 steer，不声称有语义完成通知。不要把所有消息自动执行或删除 ack。
- **大小/验收**：M，Core/launcher/web 配合。覆盖忙/闲、重启、通知重试、ack 前后
  崩溃、取消/中止、同内容不同 submission。此次修正文档的 PTY fallback 描述。

## 6. P2：virtual-host polling 冒充 routing change（确认，本次已修）

- **现象**：每 5 秒重复推送相同映射并记录更新；mariana revision 11000+ 是现场
  线索，本次没有读取该机器日志或以此判断容量。
- **根因**：[AuthStore refresh](../../crates/treer-proxy/src/auth/network.rs) 原来每次
  完整重载数据库都 `fetch_add` revision，无条件替换 cache。Controller 看到
  新 revision 就重新应用并记录日志。
- **影响**：无效传播和日志增长，revision 不再代表路由内容变化。
- **建议/实施**：仍保留 5 秒 polling 以发现其他 Proxy 的数据库更改；在既有
  更新锁内比较完整投影，内容相同不递增。先规范化删除后的空 workspace map，
  避免最后一项删除造成下一次 poll 的假变更。数据库失败仍不替换有效缓存。
- **大小/验收**：S，已实现。新 PostgreSQL 测试先在旧实现稳定失败，再验证空表、
  有内容的三次刷新、模拟远端 service port 更新、删除及后续刷新。3 项相关
  测试通过；Proxy all-targets Clippy `-D warnings` 通过。
  revision 仍是 Proxy 本地计数，不是跨 Proxy 的全局时钟；此修复没有改变协议、
  授权、广播机制或轮询频率。周期快照仍发送，Controller 可以忽略相同 revision；
  本次减少的是重复应用与 info 日志，不能声称已经消除广播流量。
  更细的 per-workspace revision 是后续优化。

## 7. P2：日志容量和文件描述符所有权未设计（确认）

- **现象**：91 MB 日志是现场线索；nohup 和 macOS launchd 文件日志持续追加，
  源码没有统一的容量/保留轮转；Linux systemd journal 有其独立管理方式。
- **根因**：[service.rs](../../crates/treer-agent-server/src/service.rs) 和
  [service_platform.rs](../../crates/treer-agent-server/src/service_platform.rs) 重定向
  daemon stdout/stderr；Host 拉起 Controller 又继承这些 FD。日志不一定由
  当前写文件路径的那一个进程拥有，重命名文件不会切换活进程的 FD。
- **影响**：磁盘压力、旧日志长期保留、故障排查噪声；粗暴 copytruncate 会损失
  并发写入，靠 service restart 切换文件又会杀工作负载。
- **建议**：Host/Controller 各自持有可滚动 writer，明确字节上限、文件数、私有
  权限和 log path；systemd 可继续 journal。共享配置与 `service logs` 展示由
  core/release-eng 配合。轮转只影响 daemon tracing，不截断 Agent PTY transcript。
- **大小/验收**：M。活 writer 超限、热 Controller 替换、磁盘满/权限/符号链接、
  follow、macOS/Linux 实测及 Windows 编译。此次不做简单 rename 或在线截断：
  这不是单个文件名修改能安全完成的小修。第 6 项先减少无效日志来源。

## 8. P2：Host 冷重启的中断代价是所有权设计的结果（确认）

- **现象**：更新/重启 Host 会终止全部受管进程；Controller 热替换能保留进程。
- **根因**：[Host](../../crates/treer-agent-host/src/main.rs) 持有 PTY 和进程树；
  [runtime](../../crates/treer-agent-runtime/src/lib.rs) 的生命周期退出清理子进程，
  Windows Job Object 还有 kill-on-close。运行进程和回放 ring 不存在于持久启动
  规格中；启动规格是恢复意图，不是 RAM/PTY 快照。
- **影响**：Host 升级成为整机维护窗口；只能重启并接回 durable harness session，
  已退出进程的旧 Host 输出无法恢复。这与已修 Controller inventory 擦掉现存
  输出的 bug 不同，不能承诺不存在的数据还能恢复。
- **建议**：Host 尽量稳定、少承载产品逻辑；默认只热更新 Controller，只有 Host
  协议变化才要求明确冷升级。机器级 drain/checkpoint/恢复验收先做，跨机器滚动
  升级优先。未来确有必要才研究 PTY FD/进程所有权移交；Unix 与 Windows 必须
  分别设计，不能简单 detach 进程逃离 Host 或取消 Job 清理。
- **大小/验收**：M 的升级预检与维护 UX；L 的不中断 Host 更换。验证 ID/凭据、
  session 恢复、App 就绪和旧输出边界，不能只看进程“已启动”。

## 9. P2：能力协商与机器级版本切换仍是两件事（确认）

- **现象**：新 CLI 对旧 Controller 明确拒绝路径/queue 功能；操作员必须理解
  升级顺序。这比旧解释器误解析新 target 安全，但 bootstrap 命令也可能被较宽
  的前置检查挡住。
- **根因**：[协议与 CLI](../../crates/treer-protocol/src/lib.rs) 的 capability 针对
  功能，新 Controller 对未知命令也已有明确响应；Host framing/protocol 是另一
  边界。二进制签名/哈希验证证明产物可信，不证明与当前机器状态兼容，也不
  等于升级、健康确认、状态回滚的一项事务。
- **影响**：半升级机器有短暂不可操作窗口；错误回滚可能遇到新状态格式。签名
  的旧 channel manifest 仍需额外版本策略才能防止误降级。
- **建议**：按请求的功能检查 capability，保留 health/status/update 等恢复入口；
  机器 release-set 固定 Host/Controller/CLI 版本和兼容范围，预检后分阶段切换，
  健康确认后才提交。优先 Controller 再 CLI，Host 仅在必要时冷更换。明确状态
  schema 是否可回滚；显式 operator downgrade 可允许，但不可隐式接受回放。
  先评估现有 release-eng 的更新/回滚工具，不另外创建竞争升级器。
- **大小/验收**：M–L，release-eng 主导。测 N/N-1/N+1、旧 launcher、旧 Host，
  每个切换点失败后的恢复；交互式失败应说缺哪个 capability 和怎样恢复。

## 10. P2：App 的稳定身份掩盖了可变执行内容（确认）

- **现象**：App 可以直接运行主 checkout 的脚本；checkout 合并后，重启会执行
  另一版本，解释型程序还可能在运行中读到改动文件。
- **根因**：[Managed App API](../../crates/treer-proxy/src/api/apps.rs) 保存 command/
  args/cwd 等启动配置，而不是不可变 artifact。稳定 service ID、virtual host
  和公网地址只保证寻址；它们不证明运行版本。未完成的 app-update 任务也需要
  保持这个区分，不能把更新命令当成完整发布事务。
- **影响**：难以重现、混合版本、未经健康检查生效，回滚和数据迁移不可追踪。
- **建议**：用户 launcher/App 发布流程用独立不可变 release 目录或固定 checkout，
  userdata/秘密在目录外；记录 revision/来源摘要，健康检查后切换启动规格，旧
  版本保留可回滚。Core 提供原子更新规格和稳定身份，不替 App 构建、不强制
  容器，也不把用户 cwd 挂载权限说成沙箱。
- **大小/验收**：M，apps-dev/release-eng 配合；Core 只补通用生命周期接口。
  测重部署中断、版本切换失败、service/public URL 不变及迁移回滚边界。

## 11. P2：健康与恢复进度缺少清晰的多层模型（确认）

- **现象**：WebSocket 已在线但 HTTP validation 失败，用户看到机器连着，部分
  工作负载却仍 deferred；launcher 端口能访问也未必表示 harness 能执行任务。
- **根因**：Host process、Controller connection、Proxy routing、恢复校验与
  harness ready 属于不同状态源。现有 proxy_connected、日志与重试已改善观测，
  但一个“online”不能概括这些条件。
- **影响**：问题被误归因于登录/网络，升级验收只验证 online 就可能漏掉部分恢复。
- **建议**：健康响应增加恢复 pending/failed 数量、最近原因与下次重试；区分
  process_alive/control_connected/routing_ready/restore_complete/interface_ready。
  UI 说明正在等待什么，不把 terminal idle 当成任务成功。恢复工作异步推进，
  避免第 1 项循环阻塞；错误日志限频但保留状态改变事件。
- **大小/验收**：M，core/web/launcher 配合；真实假 Proxy 注入暂时失败和部分
  无效凭据，确认有效项自动恢复，无效项被隔离并有可操作原因。

## 12. P3：树寻址的基本语义正确，命名域和文档需收口（确认）

- **现象**：用户容易把 Agent path 当 cwd 或权限目录；Agent 同文件夹重名规则、
  workspace-wide service 名字和旧全局名称解析看似冲突。
- **根因**：[共享 target resolver](../../crates/treer-protocol/src/targets.rs) 与
  [workload_tree](../../crates/treer-proxy/src/auth/workload_tree.rs) 已明确根/相对路径、
  sibling 名称唯一、创建前预留 placement，且路径变化不改变 ID。service/vhost
  名称还在另一寻址域。architecture 旧段落仍描述创建后 placement；security
  旧段落仍描述已退休独立 ACP/Remote Codex launcher。
- **影响**：误选 target、文档示例误导；没有证据表明目前 move 导致身份或 Policy
  随路径继承，不能凭目录结构声称它提供安全隔离。
- **建议/实施**：路径用于发现，稳定 ID 用于持久引用、消息收件人和权限；Message
  path/name 是发送时快照。明确 folder 不是文件系统，也不是 ACL inheritance。
  此次修正创建前预留和单一 launcher 文档，保留现有 resolver；下一步统一所有
  target 调用入口并提供 legacy ambiguous target 的明确错误。
- **大小/验收**：S 文档，M 清理旧入口。复用同文件夹重名、跨文件夹同名、移动、
  relative/self/App/service 区分和旧 Controller 拒绝测试，避免重造 resolver。

## 建议执行顺序与本次验证

1. 本次交付 virtual-host 内容比较与文档纠正；不要顺带更改协议/鉴权。
2. 优先给控制连接做慢操作隔离和 NATS 故障注入；这是“所有 Agent 同时不可用”
   的放大点。剩余所有权风险先稳定复现，再由 release-eng 交叉评审后修。
3. core/launcher 统一 submission、Message、notify 的状态契约，明确旧 bridge 退出
   路径；运行代码发布目录与日志 writer 分别交给对应 owner 协同。
4. 最后做升级事务、健康进度与可选 Host checkpoint；不要先做大型无中断移交。

已跑：3 项 virtual-host PostgreSQL 测试（测试独立 schema，隔离本地测试数据库），
Proxy all-targets Clippy `-D warnings`。新回归先在修复前稳定失败，修复后通过。
fmt、文档与 diff 检查通过。未跑完整发布门禁、跨平台实机、生产
故障注入；这些不是本次小修的已验证结论。91 MB 日志及 Mac 断连只作为现场
线索；本次未触碰运行中的机器、凭据或日志。
