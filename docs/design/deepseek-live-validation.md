# DeepSeek 真实接口验证

验证日期为 2026-10-29。目标为用户提供的 `https://api.deepseek.com/responses`，请求模型与接口报告的模型均为 `deepseek-flash`。

这是方案的协议原型验证。生产 `src/` 与 `tests/` 没有修改，完整修复没有实现。密钥只保存在真实 HTTPS 转发进程的内存中，用于请求鉴权，不进入请求正文、归档或子进程。

## 真实调用结果

共执行 27 次真实 HTTPS 请求，其中 24 次返回 200，另外三次是以下受控反例。接口响应和实际发送正文保存在本地实验目录，未保存鉴权头。

| 探针 | 默认设置 | `reasoning.effort=high` | 可以得出的结论 |
| --- | --- | --- | --- |
| 模型实际发出工具调用，结果沿用原生 ID 回传 | 200，正确回答随机校验值 | 200，正确回答随机校验值 | 这一工具往返不需要将原生 ID 改成 UUID |
| 子进程从落盘请求、原生响应和工具结果重建下一请求 | 200，正确校验值 | 200，保留 reasoning 项并正确回答 | 本探针不依赖之前服务进程的 continuation 内存 |
| 已完成 turn 作为语义历史供新 turn 使用 | 200，正确校验值 | 200，正确校验值 | DeepSeek 接受这个语义历史请求形状 |
| 两个语义历史 turn 记录相同原生 ID | 200，正确校验值 | 200，正确校验值 | 普通历史数据里的重复 ID 没有原生关联冲突 |
| 同一个原生请求重复提交相同 `call_id` 的调用和结果 | 400，明确报告 duplicate call_id | 400，同样拒绝 | 同一请求中的原生 ID 必须避免重复 |
| reviewer 使用规定 JSON schema 返回审核结论 | 200，有效 JSON、允许读取 | 200，有效 JSON、允许读取 | 该 reviewer 请求格式实际可用 |
| 同一 active 工具链去掉 reasoning 项 | 默认原始响应没有该项，对照请求字节相同 | 200，正确校验值 | 这个简单 high 探针中，省略该项没有导致拒绝，不能推导质量等价 |
| 回传原生项时编辑 instructions 前缀 | 200 | 200 | DeepSeek 此探针接受改动，不能把 Anthropic 的签名前缀规则套用到所有 Responses 服务 |

还单独执行了 `reasoning=high`、`tool_choice=auto` 的连续两次工具调用。模型先请求 `read_fixture`，读取第一校验值后，再把该真实值作为 `check_fixture` 参数。第二工具返回另一校验值后，模型最终正确回答两个值。三次请求均为 200，每次由不同的新进程从已提交的 `chain.jsonl` 构造，instructions 与 tools 保持不变，原生响应项和工具结果逐步追加。这个探针覆盖了同一 turn 中自动选择第二个工具的路径。

第一次探索使用了强制指定函数的 `tool_choice`。接口返回 400，说明 thinking mode 不支持该选择方式。正式探针随后改用 COLA 当前采用的自动选工具方式。第三个 400 就是这次探索，另外两个来自表中的重复原生 ID 反例。

工具读取的是隔离实验文件中的随机校验值。第一次模型请求不包含该值，之后由本地读取结果提供。验证不止看 HTTP 状态，也检查最终回答是否包含实际提供的校验值。没有把仓库文件内容发送给模型。

## 单个 JSONL 文件恢复

额外将 high 组真实请求、原生响应和工具读取结果原子写入一个 JSONL 原型档案。随后启动新的 Python 进程，只从这个文件重建模型请求，通过真实 HTTPS 获得 200 和正确校验值。

恢复请求的 SHA-256 为 `f814b2f6ff0ab3aa68d3208bbf2c24a3bc49e499a76245f8d8c145ea330dc090`，与此前 high 组完整原生回传请求完全相同。恢复请求含一个真实 reasoning 项和原始工具 ID。子进程不持有 provider 密钥，只向本地转发器提交正文。

这个原型档案的格式为 `cola.durable-session.prototype`，不是已经落地的 COLA v2。它证明本次请求可以从持久化数据独立构造，不证明拟议完整文档、原子接受、崩溃恢复和所有字段已经实现。

## 使用当前 COLA 公共模型客户端复现故障

Rust 原型引用当前仓库的 `OpenAIClient`、领域 `Session` 和真实 `JsonlSessionStore`。HTTP 转发器只捕获正文并转发，返回的内容来自真实 DeepSeek，不使用模拟响应。

默认和 high 两组均观察到以下结果。

1. 首次生成工具调用成功，当前客户端将原生 ID 改成 UUID 领域 ID。
2. 携带内存 continuation 回传工具结果成功，得到正确校验值。
3. 将完整领域会话保存到真实 JSONL store，再加载并开始新 turn，传入空 continuation，得到 `MissingContinuation`，没有额外 provider 请求。
4. 原驻留会话携带 continuation 开始新 turn 成功，得到正确校验值。

每组当前公共客户端实际产生三次上游请求，分别是首次调用、工具结果回传、驻留会话新 turn。冷加载失败发生于本地。此处运行的是公共模型客户端与存储组合，没有声称完整 CLI/Web 的修复已完成。

## 原生思考数据的验证边界

默认 seed 响应没有独立 reasoning 项，报告 reasoning token 为零，所以它的“去掉 reasoning”不能作为有效对照。

high seed 确实返回 reasoning 项，包含 `content` 中的 `reasoning_text`。响应也包含非空 `encrypted_content` 字段，但本实验没有验证该字段的加密或绑定语义。DeepSeek 文档说明其兼容层对 reasoning 输入主要处理明文 content，不能仅凭字段存在就认定它具有 OpenAI 原生加密状态的含义。

本次完整回传、去掉 reasoning 和编辑前缀的请求都被接受。这只证明这几种请求形状可用。没有衡量复杂任务中的推理连续性、回答质量、成本或延迟收益，也没有验证 Anthropic 或 OpenAI 官方服务。

## 对方案的影响

保留硬盘作为权威来源、逐次请求响应归档、原生 ID 不随机替换，以及按作用域定位执行的方案。真实结果支持其核心协议可行性。

保留每个新 turn 建立语义历史视图作为候选策略。它已获 DeepSeek 接受，但回答质量仍需任务评估，不能声称它优于完整原生历史。

将“所有同协议服务都要求相同原生思考回传规则”的表述改成 provider 具体约束。原生响应完整归档，能否重放、哪些项必需，以及前缀能否编辑由适配器按目标服务契约处理，不由 `openai-responses` 字符串一概决定。

## 复跑与证据

原型位于 `.audit/durable-session/live/probe.py` 和 `live/public-client/`。脚本隐藏输入密钥，实验目录没有密钥。真实转发器在每次运行结束后关闭。

```sh
CARGO_TARGET_DIR="$PWD/target" cargo build --offline --manifest-path .audit/durable-session/live/public-client/Cargo.toml
python3 .audit/durable-session/live/probe.py
python3 .audit/durable-session/live/probe.py --effort-high
python3 .audit/durable-session/live/probe.py --replay-archive .audit/durable-session/live/run-9f8dfdfd
python3 .audit/durable-session/live/probe.py --two-tools
```

本次证据目录分别为以下路径。

- `.audit/durable-session/live/run-d5a3ca37/` 保存首次强制工具选择的 400。
- `.audit/durable-session/live/run-a8edbb6d/` 保存默认组。
- `.audit/durable-session/live/run-9f8dfdfd/` 保存 high 组。
- `.audit/durable-session/live/run-56c7085e/` 保存单 JSONL 恢复组。
- `.audit/durable-session/live/run-3b02f4f0/` 保存连续两工具、三次新进程请求组。

各目录中的 `summary.json`、`*.metadata.json`、`*.request.json`、`*.response.json` 和 `public-client.json` 提供对应事实。单文件恢复组还包含 `session.jsonl`。

查阅的官方来源为 [DeepSeek Responses 兼容说明](https://api-docs.deepseek.com/guides/responses_api/) 和 [Responses 请求定义](https://api-docs.deepseek.com/api/create-response/)。本报告将实测行为与文档声明分开，字段存在不等于其语义已得到验证。
