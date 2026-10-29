# 百炼 qwen3.8-max 真实接口验证

验证日期为 2026-10-29，使用用户指定的 `qwen3.8-max`。请求与响应报告的模型名称一致，不据此推断未公开的底层模型版本。

目标地址是 `https://dashscope.aliyuncs.com/compatible-mode/v1/responses`。用户给出的文本生成页面链接至 Responses 说明，官方参考确认旧 dashscope 域名仍可使用，新路径为 `/compatible-mode/v1/responses`。因此不编造 WorkspaceId，也不使用已停止维护的旧路径。

## 结论

本次核心协议原型成功。原生工具 ID 原样往返、单 JSONL 新进程重建、多步工具循环、语义历史新 turn 均被真实百炼接受。当前 COLA 公共客户端冷加载后仍复现 `MissingContinuation`。

主矩阵共 19 次真实 HTTPS 请求，18 次 HTTP 200、1 次 HTTP 400。18 个 200 中有 17 个 `completed` 和 1 个 `incomplete`，不能把全部 200 算作生成成功。用户追加的同会话切换模型验证另外执行八次请求，见下文，不混入主矩阵计数。

这是隔离原型验证，生产 `src/`、`tests/`、现有会话没有修改。密钥仅在 HTTPS 转发进程内存中用于鉴权，没有进入模型正文、归档或子进程。工具链、恢复和当前 Rust 客户端共 16 个请求显式设置 `store:false`，三个直接审核请求没有设置该字段。所有请求均不传 `previous_response_id` 或 `conversation`，后续请求不依赖这些服务端历史引用。不因没有引用就声称服务端没有保存响应。

三个未设置 store 的审核响应均报告 `store:true`。本次没有调用服务端响应查询验证其实际保存行为，审核内容仅为隔离读取任务。

## 真实结果

| 探针 | 结果 | 可以得出的结论 |
| --- | --- | --- |
| 自动选择读取工具，沿用原生 call_id 回传 | 200、completed，正确读出随机校验值 | 这一往返不需要随机 UUID 替换 |
| 回传真实 reasoning.summary 与原生调用 | 200、completed，正确校验值 | 本次完整原生请求形状被接受 |
| 去掉 reasoning 项 | 200、completed，正确校验值 | 本次省略未导致拒绝，不能推导复杂任务质量等价 |
| 修改 instructions 后继续原生回传 | 200、completed，正确校验值 | 本次未表现出不可编辑的签名前缀限制 |
| 已完成历史变成带角色来源的语义记录，再开始新 turn | 200、completed，正确校验值 | 百炼接受这种历史输入形状 |
| 两个语义历史 turn 记录相同调用 ID | 200、completed，正确校验值 | 普通历史数据中的相同 ID 没有引入原生请求冲突 |
| 重复原生 function_call，连同原生输出项 id 一起重复 | 400，Duplicate message id | 证明原生输出项 id 冲突被拒绝，不能据此单独证明 call_id 重复被拒绝 |
| 对照中移除第二项的 id，只保留重复 call_id | 200、completed | 百炼接受这个重复 call_id 请求形状，与 DeepSeek 的实际拒绝行为不同 |
| reviewer 原探针，默认思考，max_output_tokens=2048 | 200、incomplete，reason=max_output_tokens | 2048 个输出 token 均计为 reasoning，未产生有效审核结论 |
| 相同 reviewer 请求改为 reasoning.effort=none | 200、completed，有效审核 JSON | 当前平铺 format 形状在此请求被接受并返回所需字段 |
| 使用文档嵌套 json_schema 形状，关闭思考 | 200、completed，有效审核 JSON | 该文档请求形状也被接受，不据此宣称已验证所有 schema 强制规则 |
| 当前 Rust ModelClient<Decision>，关闭思考 | 200，Generation::Complete，should_allow=true | 真实响应可通过当前 COLA 的类型化审核解码 |

随机校验值第一次请求不包含在模型输入里。只有本地工具实际读取隔离文件后才提供它。成功标准除状态外还检查回复是否包含该实际值。没有把仓库文件或其他用户会话发送给模型。

## 单个 JSONL 与新进程

将真实 seed 请求、原生响应和实际工具结果原子写入 `session.jsonl`，同步文件与目录后启动新的子进程。子进程仅由这个档案构造请求，真实百炼返回 200、completed 和正确校验值。

该请求与此前完整原生回传请求逐字节相同。SHA-256 为 `469fcf5ef32d6f20863269d7e71b9e32d046f987409c2a21d27bd7cbc38c9dbb`。两次请求均含同一真实 reasoning 项与原始 call_id。

JSONL 格式为实验档案，尚不是 COLA v2 实现。这个观察证明请求可由持久化数据确定，不证明崩溃恢复或完整应用事务已经实现。

## 同一 turn 两次工具调用

显式请求 `reasoning.effort=high`、`tool_choice=auto`。官方文档将 qwen3.8-max 的 high 映射至 xhigh，报告不将文档映射冒充独立测量。

三个不同子进程分别从同一个逐步提交的 `chain.jsonl` 构造请求。

1. 模型请求 `read_fixture`，本地返回第一校验值。
2. 模型请求 `check_fixture`，参数中的第一校验值与实际读取值完全相同，本地返回第二校验值。
3. 模型不再调用工具，最终回答包含两个正确值。

三个请求均为 200、completed。system instructions、tools、模型和其他设置保持相同，input 只追加已观察原生响应与工具结果。没有使用后台进程中的模型 continuation。

## 当前 COLA 的真实复现

Rust 原型引用未修改的 `OpenAIClient`、领域 `Session` 与 `JsonlSessionStore`。

- 首次工具调用和携带 continuation 的结果回传成功。
- 当前客户端仍将原生 ID 转为 UUID 领域 ID。
- 保存并加载会话后开始新 turn，传空 continuation，返回 `MissingContinuation`，不产生额外上游请求。
- 原驻留会话携带 continuation 开始新 turn 成功。

这部分共产生三个真实上游请求。另一个独立 Rust 审核探针使用 `Effort::Off`，得到有效 `Decision`。没有运行或修复完整 CLI/Web 应用。

## 对方案的修订

完整持久化、逐次 invocation 审计、作用域调用地址、删除随机 ID 映射及模型输入派生视图的方向保留。

修订原方案中“turn 内所有 call_id 一律唯一”的通用领域约束。领域只要求一个调用批次内唯一，跨 step 由 `session + turn + step + native ID` 区分。额外的请求级限制由目标模型适配器处理。原生输出项 `id` 和工具关联 `call_id` 是不同字段，均应完整归档。通用适配器可以采用较严格的兼容策略，但需明确这是策略，而非所有 provider 的必然要求。

重复 call_id 对照只证明请求被接受，没有验证多个不同结果在所有重复编号场景中都能被正确解释。不能因此自动关闭适配器的关联校验。这个修订保持事实表达与 provider 机制分离，尚待用户 review 后实现。

审核探针说明必须保存生成状态、停止原因与 usage，不能因 HTTP 200 自动放行。原探针采用 2048-token 上限，不能直接推断 COLA 默认 4096 上限或其他任务必然失败。关闭思考的配置本次得到有效结果，可作为 reviewer 配置候选。

这里没有证明加大上限就能解决原审核探针的问题，也没有用禁止任务评估审核质量。有效 JSON 与 allowed 结果仅证明这几个允许读取的请求获得了可解析结论。

官方文档还声明，接近输入窗口上限时可能自动截断。这意味着“等 provider 报容量错误”不是通用保证。适配器应结合目标模型预算预检，无法容纳强制依赖时停止。没有为了本次验证发送超大上下文，自动截断行为只作为文档约束记录。

## 同一会话切换模型

用户追加要求后，使用同一会话 ID 和同一个 `session.jsonl` 连续完成五个 turn。每个 turn 结束后重新读取已提交文档，每个模型请求都由新的子进程独立构造。切换仅发生在已完成 turn 的边界。

| turn | 模型 | 操作与结果 |
| --- | --- | --- |
| 1 | DeepSeek deepseek-flash | 读取隔离文件得到 A，正确回答 A |
| 2 | 百炼 qwen3.8-max | 从旧历史理解 A，执行本轮新工具获得 B，正确回答 A、B |
| 3 | 百炼 qwen3.8-flash | 在同 provider 内切换模型，执行新工具获得 C，正确回答 A、B、C |
| 4 | DeepSeek deepseek-flash | 切回另一 provider，仅凭持久化历史正确回忆 A、B、C |
| 5 | 百炼 qwen3.8-max | 再切回 max，仍正确回忆 A、B、C |

八次请求来自八个不同进程，均为 HTTP 200、completed。前面三轮分别发生工具调用和最终回答，后两轮各一次回忆请求。每轮检查了实际工具读取值与回答中的全部值及顺序，不只看模型自称记得历史。

文档在每个 turn 保存 requested_model，在每个 step 的原始响应中保留报告的 model，可逐次追溯。语义历史只包含用户输入、观察到的工具结果和 assistant 回答，没有自动把历史模型审计字段塞入 prompt。新 turn 不携带旧 provider 的原生 reasoning、function_call 或 function_call_output 项；同 turn 内仍保留当前目标返回的原生项。所有八次请求显式 store:false，不传服务端历史引用。

每轮读取任务会覆盖同一个隔离的 `current-fixture.json`，旧值由会话档案保存。请求生成使用固定的原型 renderer 和显式当前模型配置。输出顺序通过独立的离线脚本 `.audit/durable-session/bailian/verify_switch_evidence.py` 检查，生成循环自身只检查值是否存在。该脚本可以重新检查本次捕获，不产生模型请求。

这个实验覆盖跨 provider 切换、百炼内部 max 与 flash 切换，以及切回原模型。它验证此处基于语义历史的新视图方式可行，不证明跨模型原生思考块可直接互换，也不证明完整 Rust AgentService 已支持切换。没有测试工具链中途热切模型。

切换证据位于 `.audit/durable-session/bailian/switch-0769aafe/`，包含同一个 `session.jsonl`、八次正文与响应、`summary.json` 和 `evidence-check.json`。模型与 provider 的选择是显式配置，子进程不持有 API 密钥。

实验目录编号为 `switch-0769aafe`，逻辑会话 ID 为 `switch-0298724260e135be`，两者承担不同用途。五个 turn 均属于后一个会话 ID，并非五次创建独立会话。

## 证据与边界

证据位于以下目录。

- `.audit/durable-session/live/run-bailian-0e76e1fe/` 保存主矩阵、首次审核 incomplete、当前 Rust 会话客户端结果。
- `.audit/durable-session/live/run-bailian-f85f39b5/` 保存两工具循环与三个不同进程的结果。
- `.audit/durable-session/live/run-bailian-17c93688/` 保存单 JSONL 重建。
- `.audit/durable-session/live/run-bailian-f197c041/` 保存隔离 call_id 对照、两种审核请求格式和 Rust 类型化审核结果。

每个目录包含无鉴权头的实际正文、原始响应、metadata、summary。原型脚本是 `.audit/durable-session/live/probe.py`，通过 `--provider bailian` 明确选择 endpoint 和 model。Rust 原型位于 `live/public-client/`，模型由实验环境显式传入。

没有证明模型质量、原生思考连续性的收益、性能、掉电恢复或其他 provider 的行为。每个用例是小样本请求，不能作为服务可靠性评估。实验转发器只捕获正文后转发真实 HTTPS，未替换模型返回。

来源为用户提供的 [文本生成文档](https://help.aliyun.com/zh/model-studio/text-generation) 和其链接的 [Responses API 参考](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-responses)。文档事实与上述实测结果分开陈述。
