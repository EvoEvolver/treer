# Policy 安全默认迁移方案

状态：讨论方案，未实现；本次不改任何 workspace 的权限、默认 mode 或规则。
背景见 [控制面安全审查](2026-10-12-security-core.md)。目标是新工作区默认最小权限，
现有工作区明确选择迁移，同时不让管理员、Controller 恢复和 Policy Provider 互相锁死。

## 先分清 enforce 和 deny-by-default

当前没有 Provider/legacy policy 时是 Treer Default monitor/allow。
`CompiledWorkspacePolicy` 未匹配 action 的缺省是 Allow；Monitor 即使算出 Deny 也返回 Allow。
因此仅把 mode 从 monitor 改成 enforce 不会自动获得安全默认。需要同时确定：

1. mode=enforce；
2. 未匹配的 action/resource 默认 Deny；
3. 给必要 bootstrap、自己的身份操作和管理员恢复入口明确授权；
4. Provider 不可用、缓存过期、版本偏差时的确定行为；
5. 不绕过这些规则的路由覆盖和可观察的拒绝原因。

## 建议默认权限

下表是拟定原则，不是已实现 action 集；具体 action 名与 resource selector 要逐条对照
`crates/treer-protocol` 和 Proxy 调用点冻结。membership、machine/workload binding 等认证
前置条件始终独立于 Policy，Owner 的规则也不能授权跨 workspace/错误机器身份。

| 主体 | 默认允许 | 默认拒绝/需要显式 grant |
| --- | --- | --- |
| Workspace Owner/组织授权管理员 | Policy 配置、权限预览、只读运维、受审计恢复入口 | 跨 workspace；一般机器执行也使用显式管理员 role grant |
| 普通 human member | 自身身份、允许展示的目录/状态、显式属于自己的数据 | machine exec/file write、创建/停止/修改别人的 workload、完整 Agent UI、公开服务、Policy mutation |
| Agent/App | 自己的身份与必要运行声明、自己的消息收取/ack、自身获准的基本操作 | 别的 Agent prompt/queue、startup admin、machine exec/file write、公开 ingress、App 数据访问 |
| Controller/machine | 自己的 heartbeat/inventory、验证绑定身份、已批准 durable startup 的恢复协议 | 把 machine 身份当任意 human owner；改变别机/别 Agent 身份；超出自身边界的控制能力 |
| 未认证/public | 显式 public service 的匿名入口、必要登录/health | workspace/Agent/机器/Policy 管理；取得人类会话或 App 委托身份 |

Agent-to-Agent 协作通过 folder/具体 ID 的显式 grant 提供，例如允许团队 folder 下的
prompt/send/queue.read，限制 startup.manage/stop 和 exec。App 的 service token 不能被
解释成整个 workspace 的管理员。目录可见、读取内容、发送 prompt、执行机器命令是
不同能力；“同一个 folder”不自动创建信任关系或授予互相控制权限。

## 必须先补齐的边界

**完整 UI 权限。** 当前 registered AIS UI 是 opaque HTTP/WS tunnel，不是对每条 launcher
API 应用 `agent.prompt` 的语义代理。只拒绝 Core prompt 但仍允许完整 UI 可能仍能启动 turn。
需要明确一个“完整 interface access”授权，把它视为包括读 transcript/文件、操作 turn 的
整体权限；只读角色不能获得完整操作界面。或者让 launcher 按能力限制 UI，按已验证用户
进行授权。不能把 forwarding marker 当 Policy 或 human identity。App 自己的数据 ACL 也
不能由 Core membership 替代。此项没有在本次转发标识修复中实现。

**管理员救援。** 保留 owner-only、资源边界严格的 Policy 管理/恢复通道，带独立认证和审计。
现有默认 Provider installer 的 bootstrap 例外要列成固定白名单，不把 machine.exec/general
startup.manage 暴露成救援旁路。让失效 Provider 自己批准恢复它的请求会死锁。
没有 Owner session 时走 operator 明确恢复认证，不能因 loopback 或环境变量自报就放行。

**重启恢复。** 已批准 durable workload 的 restart/冷恢复和“任意新建 workload”不能简单等同。
当前 startup validation 核对 machine binding、active credential 和 stopped intent，不调用
一般 `agent.create` Policy；迁移必须保留/显式定义这个恢复契约，不能突然把所有服务都停掉。
Policy Provider 自己依赖的服务恢复要有固定的受限 bootstrap 路径。吊销/deleted/disabled
仍必须拒绝；启动命令被他人替换依旧单独授权，不靠恢复例外绕过 startup.manage。

**令牌窗口。** 已签发的 60s/12h JWT 和 App 离线验签不能立即应用新 Policy。
迁移报告要显示生效窗口；在线 App helper、session/auth epoch、缓存失效属于单独工程。
不能把 commit 或页面显示 enforce 的时刻说成所有已签发权限即时失效。

**审计与解释。** 先补只记录 actor/action/resource/revision/reason 的 decision 记录/指标，
不要写 prompts、message body、命令或秘密。要求显示是 enforce deny、monitor would-deny、
provider unavailable、认证/绑定失败还是旧版本不支持，避免全部变成 proxy_unavailable。
审计投递仍需显式声明完整性限制；预览失败不能当成“没有风险”。

## 分阶段实施与回滚

### 阶段 0：行为清单和救援演练

逐一盘点 browser/operator/machine/Agent/App 的 mutation/read 路由，登记哪些已经评估
Policy、哪些是身份硬约束、哪些是 bootstrap 例外、哪些 opaque UI/App 自主授权。
用封闭的假 Proxy/Controller/Provider/数据库验证冷恢复、Provider 启动失败、owner 救援。
没有 action coverage 和回归矩阵时不切全局默认。

### 阶段 1：版本化基线，只有新工作区选择

引入持久化、版本化 baseline：legacy-monitor-allow 与 secure-enforce-deny。
不要依靠二进制升级瞬间改变所有已有 workspace 的隐式行为；新建 workspace 的安全基线
由 Owner onboarding 明确列出必要 grant，初始无人类管理员的 machine enrollment 不自行
变成 allow-all。禁用 auth 的本地开发模式必须明确例外且不得宣传为安全部署。

新动作默认拒绝，不能因为未来未识别 action 自动允许。Provider 明确的 legacy Allow
仍有宽权限，需要预览指出，不能只查看 Provider mode 就显示“安全默认已开启”。

### 阶段 2：现有工作区预览与主动迁移

Owner 获得差异表：哪些角色/Agent/App/恢复操作会被拒绝；每条标注稳定资源 ID、实际树
path 只是展示，重命名/移动后不应意外扩大授权。观察历史请求可推荐 grant，但不能把
观察到的所有历史操作自动永久授权；没出现过的操作也不是安全证明。

切换前保存上一版 Policy/baseline/provider binding revision，以数据库事务和 revision CAS
提交；对多个 Proxy 缓存广播失效并可观测确认。界面展示 pending/applied/degraded 与撤销
窗口。旧 Proxy 不能解读新基线时 fail closed 并明确报错，不能静默回到旧 allow 默认。

回滚由同一 owner-only 管理通道完成，恢复上一个确定 revision；不允许通用 Agent/App
把 mode 改回 monitor。回滚若放宽权限必须提醒并审计。禁止遇到任意 403 就自动 fail-open。

### 阶段 3：扩大覆盖，再考虑强制迁移

只有新工作区通过完整恢复/协作验收、现有用户看过差异且救援流程可用，才讨论弃用旧
allow baseline。强制期限、无人维护 workspace 的处理方式和旧客户端支持由用户决定。
这一步不是当前讨论的默认选项，不部署自动批量改权限任务。

## 主要风险与验收

| 风险 | 影响 | 必须验证/缓解 |
| --- | --- | --- |
| 只改 mode，不改 implicit Allow | 看似 enforce，权限仍宽 | unmatched、新 action、explicit legacy allow 的单测与展示 |
| 权限覆盖漏掉 UI/App/restore | 实际绕过或控制能力突然失效 | 路由 coverage、完整 UI 权限模型、不能伪造 sender |
| Provider 与恢复互相依赖 | 重启后全部卡住/管理员锁死 | Provider down/cache expiry/Host cold recovery/owner rescue |
| 多 Proxy 缓存/老版本偏差 | 同一请求不同结果或退回 allow | revision CAS、失效传播、旧版本拒绝、可回滚 |
| 树移动/同名解析改变资源 | grant 错授给别的 Agent | 授权解析 stable ID、cross-workspace/别机否定测试 |
| JWT/离线 App 权限残留 | 用户误认为立即撤销 | TTL/auth epoch契约、窗口提示、App 接口集成测试 |
| 给 Owner/救援过宽旁路 | 管理入口成为普通执行绕过 | 管理角色显式 grant、救援白名单、审计且硬身份约束 |
| 对现有协作直接默认拒绝 | 消息、queue、app deployment断流 | 迁移预览、团队folder grants、用户确认、固定revision回滚 |

验收至少包括：普通成员/Agent 跨目标 exec/file/startup/prompt 拒绝；自身必要身份与收件
操作正常；owner 修复误配规则后恢复；Provider 拒绝/不可用/过期；一 Proxy 旧缓存/旧版本；
queue 通知与取消；App 人类委托身份范围；重命名/移动；冷恢复保留已批准 workload；
禁用/删除不复活；回滚不跨身份边界；UI 权限不能替代每项拒绝规则的安全承诺。

工作量：L，至少拆为路由/救援契约、版本化默认与规则矩阵、预览/审计/迁移工具、多实例
生效与回滚四项。它应作为独立功能推进，不能在此次 launcher release blocker 的小修中
顺手把 `.unwrap_or(Allow)` 改成 Deny。

用户需要讨论的选择：新工作区默认 secure baseline 是否立即强制；老工作区是否仅主动
迁移；普通成员是否只读；团队 Agent 默认能协作哪些 folder；完整 UI 属于什么权限；
Provider 失效允许多长 stale 窗口。推荐新工作区安全默认、老工作区明确迁移，无自动 fail-open。
