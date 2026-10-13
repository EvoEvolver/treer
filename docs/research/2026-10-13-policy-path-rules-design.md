# Policy 路径规则与 App 动作设计

状态：讨论方案，未实现；2026-10-13 用户决定保持 Policy 解耦，并允许自建 Policy App。
此次只写文档，不改默认 mode、既有规则或运行服务。与
[Tree 链接设计](2026-10-13-tree-links-design.md) 配套：链接只提供发现/寻址，授权来自规则。

## 当前事实与最小 Core 边界

[Provider 协议](../policy-provider.md) 目前由 App 提供 immutable bundle，Proxy 编译并本地
执行，不逐请求调用 Provider，不运行它提供的任意代码。
[PolicyRequest/编译器](../../crates/treer-proxy/src/policy.rs) 有主体、action、resource；
resource 已有字符串 attributes，但 matcher 不按这些 attributes 匹配：resource selector
实际检查 kind/id/principal_group，self 在 subject selector 检查。subject 没有 Tree 上下文。
不是已有通用 ABAC 引擎，也不能直接把路径放进 attributes 就认为规则会生效。

[document validator](../../crates/treer-proxy/src/policy_store.rs) 当前接受 schema v1，
严格字段校验，document 256 KiB、最多 1000 rules；Provider response 更大但有 300 KiB
上限。当前优先级降序、同优先级 Deny 在前、第一匹配规则生效；未匹配 action 默认 Allow，
monitor 将算出的 Deny 放行。以上默认本方案不修改。

Core 只负责可信身份/资源映射、路径事实、受限规则语言、确定性执行与诊断接口。规则内容、
团队权限模板、编辑界面、审批与建议由用户选用的 Policy App 负责；bundled Policy App
没有特殊授权地位。manifest 自声明 action 也不赋予 App 管理角色。

建议按优先级先实现：①可信 Tree context，②schema v2 路径/关系条件，③explain/monitor；
之后接入 App 动作检查和 Tree mutation。新增能力不能用返回 Allow 的 stub 冒充支持。

## 1. 评估时动态 Tree 上下文

新增 typed `subject.tree` 与 `resource.tree`，保留 v1 kind/ID/group/self；不要将动态树
展开进 bundle，不依赖 JWT 中发行时的 folder/path，也不采用调用者的 header/JSON 自报。
每次评估 Proxy 按已认证 ID、workspace 和当前 Tree 查询生成：

| 字段 | 语义 |
| --- | --- |
| `node_id`, `kind`, `name`, `path` | 稳定规范 node ID、Agent/App/folder 类别、当前标签与绝对路径 |
| `folder_id`, `folder_path` | workload 所在真实目录；folder 节点是其 parent，另用 node_id 表示自己 |
| `ancestor_ids`, `ancestor_paths` | 根到所在 folder 的有序目录链，包含所在 folder；不含 workload 自己 |
| `placed` | 区分已放在 root 与没有有效 placement，不能两者都当 NULL/root |
| `tree_revision` | 本次一致快照 revision，subject/resource 相同 |

root 对外用显式 root 语义（例如 `folder_id:null, folder_path:"/", placed:true`），
关系匹配不能以两份缺失 context 都为 null 就当同目录。human/machine 默认没有 Tree
placement，tree 为 absent；不能借操作某 Agent 的浏览器身份冒充此 Agent。App runtime
虽然底层 credential 用 Agent ID，其 `tree.kind` 必须由 Core 标记为 app，防止“仅 Agent”
规则错误涵盖 App。App service resource 映射稳定 deployment App ID 和规范位置，不能
按请求的 service 显示名猜对象；无对应 Tree node 的独立 service context 是 absent。

对象属性、规范路径来自同一数据库快照。资源路径始终是最终真实目标，绝不采用 alias
path；链接创建等节点操作另携带 link 自身位置。跨 workspace/错误 machine binding、
credential revocation 等硬认证先检查，任何规则 Allow 不能越过。

## 2. 路径选择器、关系和绑定

schema v2 为 subject/resource selector 增加 `tree` 的 typed 字段，规则增加 `conditions`。
v2 仍是 subjects 内 OR、resources 内 OR、单个 selector 字段 AND；conditions 全 AND。
不暴露 arbitrary attributes 表达式、脚本、SQL、用户正则或递归调用 Policy。

### 路径语法

路径大小写敏感，按现有 Tree label 原始 Unicode 比较，不新增隐藏的大小写折叠/Unicode
归一化。matcher 输入是 Core 已规范化绝对路径；pattern 必须绝对、无空组件/`.`/`..`、
无 URL decode，不是宿主文件路径。相似 Unicode label UI 提醒，稳定 ID 可用于关键规则。

- `path_eq` 精确匹配 `/infra/email-endpoint`。
- `path_glob` 的 `*` 匹配恰好一个完整组件；`/team/*` 只匹配直属节点，不匹配 `/team`
  或 `/team/sub/reviewer`。不支持组件内 `rev*`、`?`、字符类或转义脚本。
- `**` 匹配零个或多个完整组件；`/infra/**` 包含 `/infra` 与后代，不能匹配 `/infrastructure`。
  要仅工作负载时同时限定 `tree.kind`；v2初版只允许一个且位于结尾的 `**`，避免回溯。
- `path_prefix` 按组件边界匹配，`/infra` 等价包含自身及所有后代的 subtree，不是字符串
  startsWith。必须显式提供是否包含自身；validator 不允许同时设冲突的 eq/glob/prefix。
- 保持标签允许普通字符的现有边界；恰好叫 `*`/`**` 的 legacy label 不能靠 glob 字面
  表达，使用 path_eq 或 stable ID，不能自动改名。

### 关系与变量

`same_folder(subject, resource)` 比较 workspace 内真实 folder ID，仅双方 placed 时成立。
`resource_below_subject_folder` 判断目标所在 folder 的 ancestors 含 subject.folder_id，
默认含同目录；`strict:true` 排除同目录。`resource_below_folder` 接受 folder stable ID
或当前 folder path，并显式 `include_self`；路径参数引用的目录若不存在则不匹配，
不能退回名字搜索。root 用专用 root sentinel 处理，缺失不是 root。

建议最小变量绑定是 typed 引用，而非字符串模板拼接：
`bindings:{"team":{"ref":"subject.tree.folder"}}` 创建 rule-local folder 引用；
`conditions:[{"op":"resource_below_folder","folder":{"var":"team"},"strict":false}]`。
变量只绑定可信 request 字段或声明的常量 folder ID/path，名字有限且禁止重新赋值；
绑定先独立完成，缺失字段为 unmatched，未声明变量/类型不合/循环引用由 validator 拒绝。
不用 glob capture 写 `${team}` 任意路径，不做来自 OR 分支的隐式绑定。将来 capture
需要新能力版本，不静默改变优先级或同名变量语义。

下面是拟议 v2 rule 节选，非已可发布格式：

```json
{
  "id": "team-b-email", "priority": 100, "effect": "allow",
  "subjects": [{"kind":"agent", "tree":{"kind":"agent", "path_glob":"/teamB/*"}}],
  "actions": ["app.use", "email.send"],
  "resources": [{"kind":"app", "tree":{"path_eq":"/infra/email-endpoint"}}],
  "conditions": []
}
```

同目录策略只需 conditions 中放 `same_folder`；不需要列所有 Agent ID。路径允许会在
移动/改名后按新位置重算，重命名 `/infra` 会使旧字面规则不再匹配；在原路径新建不同
对象也会受路径规则约束/获权。这是用户选择的位置语义，不能伪称绑原对象身份。关键
service 建议 stable ID selector；Policy App 显示移动影响预览、字面规则引用提醒，不自动
重写全部规则，也不暗中将 path 解析为永久 ID。固定 folder ID 引用则随该目录改名移动
而保留身份，必须在 UI 区分这两类。

### 优先级、代价与校验限额

保持 v1 first-match/priority/Deny-tie 规则，v2 增加同 effect 同 priority 按 rule ID 的
稳定 tie-break。不改成全局 Deny-wins；高优先级 Allow 覆盖低优先级 Deny 需 explain
如实显示。缺失 tree context 不使正向条件匹配，也不在 v2初版提供容易误放行的 NOT。

编译按 action 分桶，再用 path component trie 索引精确/prefix/glob候选，仍按全局优先级
合并 ID/group/关系候选后执行 first-match；不能因索引先命中就跳过更高优先级规则。
ancestors 预建 ID set，same_folder O(1)，subtree 判断 O(depth) 或集合查询；root/ID
在快照内解析，避免每条规则向数据库取一次。限制条件数、pattern 长度与深度、变量数、
compiled trie 大小；建议初值每规则32条件/8变量、路径4KiB/128组件，实际值需基准确定。
1000 rules、document UTF-8序列化256KiB上限保留，包括新增 fields/conditions/groups；
不把运行时 tree算进bundle、不枚举树来展开glob，也不允许巨大编译状态绕过大小限制。

## 3. Explain 与 monitor 日志

拟议 owner-only `POST /api/workspaces/{id}/policy/explain` 接受 action 与稳定 target ID、
可选模拟 subject ID；由 Proxy 生成 Tree context、复用真实编译器，不信自报 attributes。
不执行该 action，不签发 token，不向目标发 prompt。move preview 允许 proposed destination
但明确 simulation；真实执行必重查。认证失败/跨 workspace 没有 explain 旁路。

响应包含 computed_effect、effective_decision、mode、命中 rule ID/priority 或 default、
未匹配条件的有界原因、policy/provider/tree revision、source(fresh/stale/fail_open/
break_glass)、评估时间与 correlation ID。区分 monitor would-deny 和实际放行；Provider
不可用不能伪造成“无匹配规则”。Owner 可以查看自身 workspace 的规则细节，普通请求者
只得通用 deny reason/correlation ID，不提供其它规则全文或隐藏目标路径。

monitor decision 日志记录真实 actor ID/action/resource ID、would-deny、命中rule ID、
版本与source，不记 prompt/message/argv/env/文件内容/token。默认不写完整path/name，
owner按当前可见对象查看context；如需历史path快照作为可选敏感审计字段，需owner权限、
保留期与加密/存储约束。would-deny记录优先保留，重复allow聚合、采样/丢失计数和每
workspace限流，禁止无限磁盘。诊断记录非tamperproof/完整审计保证，真实审计拒绝亦
不阻塞break-glass。规则schema校验错误不得回显任何请求秘密。

## 4. App 自声明动作和检查接口

Managed App manifest 增加版本化 `policy.actions.v1` 能力与 bounded action声明，例如
`email.send`、`app.use` 的说明/风险类别。Core为App实例登记action catalogue；manifest
只是发现，不是grant，也不能注册/覆盖machine.exec、Policy管理等Core action语义。
App check始终作用于调用App的真实app resource，catalogue以App ID隔离；别的App即使
使用同名email.send也不能冒充此实例。manifest来源是已绑定App的受控loopback获取通道，
不是用户任意URL；限制数量/长度/重复声明，先冻结哪些名称保留给Core。

扩展 `/.treer/apps/identity/verify` 返回当前 `principal.tree` 与 service对应
`resource.tree`、tree revision（owner/App只取当前请求的最小上下文），不返回grant或
`entitled_via_link`。human无folder，App runtime真实kind不能假装Agent。身份active仅
表示当前身份/绑定有效，不等同某App action已获授权；Core在线重查credential与membership。
树变化不改JWT身份，不把current path快照写进60秒token当永久事实。

拟议 `POST /.treer/apps/policy/check`：App后端以自己的已认证service/runtime身份提交
caller service-audience token + 声明的action + 可选有界operation ID。Core校验服务所有权、
caller JWT与同workspace/audience、当前revocation，resource从App身份确定，subject从
caller JWT确定；不接受自报actor/目标App/path替代。以用户token直接自查最多允许自身
audience/action，不返回完整规则explain。响应返回实际decision、computed_effect/mode、
policy/tree revision和短有效期/no-store，无grant bearer capability。

email-endpoint由apps-dev改为调用此check执行`app.use`与`email.send`（两者都需实际
Allow），不再硬编码同folder，不自己运行ls判权限。同folder或跨team授权由Policy App
规则写入。保留endpoint activation、human管理身份、真实收件同意、业务防重复/速率限制；
Core规则不能代替用户同意。没有配置规则且仍monitor/defaultAllow时会实际允许，必须
清晰展示，不能保留一个隐藏Core同folder硬限制来替用户制定Policy。

敏感发信每次副作用前online check，队列执行重查；签名60秒JWT不是60秒授权缓存承诺。
若App允许缓存肯定决策，缓存到期取token.exp、固定最多60秒、Provider剩余stale预算
及其它要求中的最早值，cache hit不滑动；多Proxy变化和Provider刷新还可能带来更长
旧规则窗口，需展示总边界，不能把TTL说成立即撤销。独立离线App不接入check时不能
声称Core Policy覆盖其操作；App不合作/共享UID绕过也不是Core沙箱能防止的行为。

## 5. Tree 写操作与 owner break-glass

新增 `tree.move`/`tree.rename`，以及未来`tree.link.create/delete/update`动作，不用硬编码
“目标owner/admin才可ln”或“mv必然特权”。Proxy形成transition context：真实node ID、
`before.path/folder/ancestors`、`after.path/folder/ancestors`、目的folder、受影响subtree和
树revision；after由受校验的目的节点/名称计算，不信客户端的完整path。主体也用before
快照，不能先把自己移入管理员目录再按after身份为此次move授权。

规则可检查源目录、目的目录、操作主体与关系；一个operation一次明确的transition评估，
不是随意用source或destination其中一个代替。批量folder提升需逐受影响node授权或显式
有界subtree规则，任何失败整笔拒绝；preview不提交。链接create是对目的folder/link节点
的写入，额外target引用仅用于现有discover/metadata校验及规则条件；它不产生target使用
权，不硬要求目标管理资格。用户可写规则自行把create/move限制为特权。

按用户要求，**真实workspace Owner对Policy Provider管理与Tree管理的break-glass永远
放行，且不依赖Provider或用户bundle**。固定范围为本workspace Provider读取/绑定/移除/
官方恢复入口、Policy校验/救援，以及folder/placement/link的创建、move、rename、删除。
普通Agent生命周期、machine.exec、App email.send与读取秘密不因owner tree救援而全豁免。
Provider自身配置文件的App操作仍需独立App管理身份，不能据此通过匿名HTTP编辑Provider。

先验证会话、当前membership.owner、workspace边界、目标归属和命名/树一致性等硬约束，
再走静态Core rescue allow；不是接受JWT自报role或环境变量里的owner，Agent/service
不因名字leader/拥有Provider身份成为Owner。Owner树写不意味着跨workspace、伪造
credential、覆盖他人ID或忽略generation。审计注明break_glass、真实actor与动作；拒绝
Provider不得有机会截住此分支。已有Provider管理多数owner-only恢复入口不依赖delegated
Policy，Tree owner bypass是需新增的设计，不能称当前已全部实现。

## 多 Proxy 一致性、故障和竞态

Postgres workspace tree_revision与节点变更同事务提交；subject/resource和transition读
一致快照。NATS/outbox通知失效，不是Tree授权权威；通知缺口重新读。评估的cached tree
必须以primary DB当前revision确认；检测未知/过期不能fail open。强一致接受点在写事务
或动作接受时复核revision/activebinding，树/Policy变化重算或`tree_changed`，不自动选择
另一目标。外部Provider fetch不持有Tree行锁；fetch完成后再构建/复核context。

已接受的prompt/message固定真实ID，不随mv重新路由；发信等延迟业务副作用执行前再check。
数据库接受后到机器执行间没有跨进程原子撤销，需定义“已接受动作”边界，不能承诺改名
瞬间停止已运行命令。owner移动也遵守CAS和合法树校验。

Provider不可用先使用最后有效bundle，其loaded_at stale预算不能因失败重试而延长；
旧bundle也用**当前Tree**求值，不回用旧path。stale到期：fail_closed拒绝governed请求，
fail_open明确Allow并记录降级，但不能免认证、扩大workspace或猜测缺失Tree；break-glass
独立可用。max_stale当前默认300秒、允许到86400秒，是用户选项不是安全保证。
正常Provider/规则revision多Proxy目前约5秒元数据收敛；应加持久revisionHint+NATS失效与
可观察确认，严格撤销可增加revision barrier检查。仅配置NATS不等于全局即时生效。

Explain/check暴露policy_revision/tree_revision、freshness/stale_remaining；App缓存不
延长Provider剩余stale预算。最坏撤销窗口取规则传播/Provider剩余stale再加App剩余缓存，
fail_open甚至允许持续到Provider恢复，无法声称固定60秒撤销。Tree路径变化若在线当前
revision重查可立即改变新请求匹配，但离线缓存仍有窗口。用户Policy App应展示选择。

## schema v2、validator 与协商

保留Provider envelope的additive能力约定，但新增明确`policy.bundle.v2`、
`policy.tree-context.v1`、`policy.path-selectors.v1`、`policy.explain.v1`，App检查另有
`app.policy.check.v1`。manifest协商支持schemas；bundle endpoint可显式请求schema。
不将v2字段塞入deny_unknown_fields的v1文档，不能旧Proxy忽略路径条件而放行。
选择Provider时校验双方能力与实际bundle，unsupported_schema清楚拒绝且不改已有binding。

新Proxy继续读取执行v1 ID规则；v2只影响显式选用的workspace。旧Proxy不能在已切v2的
workspace接流量：先全Proxy实现兼容，再Provider提供v2，再Owner选择；版本不能懂时
明确拒绝不退回v1/allow。支持dual-schema的Provider只有规则可无损表示时才返回v1，不能
通过丢弃path/condition自动降级。旧bundle/回滚需保留确定revision和schema，Owner rescue
可恢复v1/移除Provider；不改变其他workspace默认mode。

validator检查schema/未知字段、kind/action保留命名、rule ID/priority与重复、引用变量类型、
不存在group、非法glob/路径/关系字段、绑定环、大小/复杂度/规则数；输入有界，错误定位
字段/rule ID，不复制秘密。编译器与validator共用语义测试，Provider侧可用Core validate
接口离线预检；Provider“验证过”不能代替Proxy验证。不能把自声明未知App action解释成
任意Core动作，注册action catalog与Provider rollout顺序需明确。

## 风险、测试与阶段

| 风险 | 要求 |
| --- | --- |
| 隐藏目标被path rule/explain枚举 | action先做硬workspace约束；普通caller不给rule详情或候选hint；owner诊断限workspace/no-store/限流 |
| Confused deputy/自报路径 | Core由身份和稳定ID映射；App只能检查自己audience/声明action；alias不是subject/target规范path |
| move进入高权限路径/rename扩权 | move/rename自身受Policy、before主体；用户明确位置规则影响，新同路径对象也适用；owner救援是明确例外 |
| 缓存、NATS丢事件与TOCTOU | DB revision接受点、currentTree+staleBundle、短App缓存及业务执行重查，降级来源可见 |
| bundle造成CPU/内存膨胀 | 限1000rules/256KiB及编译节点、无regex/脚本，按action/trie/ancestors索引并基准 |
| Provider/所有规则Deny锁死 | owner静态救援与当前role校验，禁止递归Provider授权，降级和审计失败也能救援 |

验收至少覆盖：动态mv/rename后subject与resource同时更新、alias同规范ID同决策、直属*
与**边界、root/无placement/human/Appkind、missing绑定、优先级/同效tie、path-vs-ID
语义、跨workspace伪造context、多个Proxy断NATS/旧revision、Providerdown/stale到期两模式、
token有效但已吊销/旧缓存、App越audience/action检查、email同folder不再硬编码、owner在
enforce deny-all/Providerdown/过期/错误schema时仍管理Provider与Tree、非owner/Agent
不能用rescue、Owner撤销角色即失rescue、break-glass不能跨workspace或执行机器命令。
这些是待实现测试，本次文档检查不能当执行验收。

| 阶段 | 内容 | 规模 |
| --- | --- | --- |
| 1（优先） | Core可信subject/resource Tree snapshot与revision、owner rescue、v2 envelope/validator基础；不先开放未经check的新写动作 | M–L |
| 2（优先） | paths/关系/typed绑定、编译索引、v1兼容与Provider协商；用户Policy App自己编写模板 | M–L |
| 3（优先） | explain/monitor would-deny、脱敏/限流/diagnostic测试、multiProxy失效与stale观测 | M |
| 4 | App action manifest/catalogue、verify tree字段、App policy check；apps-dev同步email-endpoint与在线副作用验收 | M–L |
| 5 | tree move/rename/link动作与transition条件、冲突/并发/CAS、web预览与规则影响提示；纯寻址links可独立交付但不得授予权限 | M |
| 6 | 全Proxy兼容后workspace主动选v2、Provider/App版本协调、回滚演练与用户验收；无全局默认翻转 | M |

总量L，先做1/2/3有独立价值；App共享授权等4完成后才宣传为可用。规则编辑、链接
创建时建议规则草稿是Policy App层能力，不将自动授权模板写死Core。路径治理、旧Allow
迁移可由用户讨论决定，沿用[迁移方案](2026-10-12-policy-enforce-migration.md)但以此新
owner救援/路径规则契约为后续实现依据，不在本次设计修改任何默认。
