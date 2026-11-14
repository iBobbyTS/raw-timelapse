# 节点图产品工作流（当前架构）

本文记录当前交付的节点图工作流：模型、默认图、事件、停止/错误行为、编码合同、
运行日志与 scratch 生命周期。它是产品运行行为的记录，不替代
`docs/architecture-v2.6.md` 的历史架构身份（该文档描述的插件 ABI、持久事件库、
全生命周期恢复等不在本产品范围内）。需求权威仍是 `.agent-work/REQUIREMENTS.md`
的 U01–U10 / R01–R09。

## 1. 产品表面

一次运行产生：

```
<video-file>   单个已验证视频（默认 ProRes 4444 / MOV / 源尺寸）
<stem>.log     同名运行日志（唯一日志）
```

`--output` 指定视频文件；扩展名必须与解析后的容器一致（默认 `.mov`）。日志与视频
同目录同名：`/data/tl.mov` → `/data/tl.log`。不产生 `logs/` 目录、manifest、CSV、
审计 JSON 或第二份 encoder 日志。已存在的视频或日志一律拒绝覆盖（退出码 12）。

旧版 `--output <RUN_DIR>`（运行目录）形式已移除：传入目录会得到明确的迁移错误，
提示改为视频文件路径；不会猜测文件名。`--temporal-quality`、`--on-review`、
`--save-frames`、`--overwrite` 以及文件形式的 `--steps` / `--exclude-rois`
同样明确拒绝并给出迁移说明。

## 2. 图模型

一个有向无环数据图，外加只指向已声明节点的完成事件连线。配置为 JSON
（`--graph <file>`），内建默认图与之同一模型；不引入插件安装器、脚本执行或动态节点。

- **逐帧执行**：prepare 和 solve 的测量阶段共用有界有序执行器，可并发计算独立帧。
  当前图像链（develop / identity / exposure / resize / color）仍串行，FFmpeg 是单个流式 sink。
- **sequentialNode / sequence**：可随到达读取 DNG、积累小型测量；收到匹配的完整
  frame-ID 清单并能封口后才发布序列结果（EV 表）。数据端口不要求全图像进入内存。
- **事件**：节点完成事件带节点 ID、frame ID（逐帧）或序列范围，以及作用域。事件只
  触发已声明的等待；不是像素端口。普通运行内事件由框架发一次，不承诺跨进程
  exactly-once 或 replay。
- **调度条件**：数据就绪且声明的事件就绪。框架预置完整 frame-ID 清单并封口；重复/
  缺失 ID、重复 EOF、类型不合、数据与事件组合环在可确定时拒绝（运行/配置错误）。
- **删除顺序**：删除节点的逐帧事件先于图像消费者派发；删除不等待编码 ACK，后续只
  持有自有内存像素。

## 3. 默认图

```text
source -> convert-cache --prepared references--------------------+
                  |                                              |
                  +-> measure-solve (sequential) -> EV plan -----+--> apply-ev
                                                                       |
                                                                       +-> color -> encode
                                                                       |
                       delete-cache (waits apply-ev, scope=frame) <---+
```

- `source`：按数字序发出 frame token。
- `convert-cache`：每个源调用一次 `dng-linear-frame --prepare`，把 LinearRaw 产物
  写入 scratch，并只保留轻量引用表。该支路不被 EV 支路阻塞。
- `measure-solve`：可并发逐帧测量（0 EV），按序提交并在完整封口后仅求解一次有限 EV 表。
  `--strength 0` 时该节点替换为 `zero`，不执行测量。
- `apply-ev`：按帧读取 EV，二次 develop，EV 恰好应用一次，返回自有 FP32 像素。
- `color`：唯一的 10-bit 4:4:4 量化/编码交接点。
- `encode`：FFmpeg 流式 sink；第一帧时启动，序列结束时关闭并校验。
- `delete-cache`：等待对应帧的 `apply-ev` 完成事件，只删除本 run 生成的缓存 DNG；
  原始 RAW/DNG 不是删除目标（`generated=false` 时为空操作）。

用户图可以插入 identity / exposure / resize 节点、串联另一个 sequential 节点或
改变事件依赖；缺节点、端口类型不符、环、非法事件作用域、重复/未声明节点均在
RAW 处理前报配置错误。

### 节点并发配置（当前接入范围）

节点 JSON 可写 `"concurrency":4`；省略为 1，显式值必须是 1..64 的整数。
null、字符串、布尔、小数、越界和重复字段均在重工作前拒绝。prepare 和 solve 的测量
阶段支持大于 1；当前图像链和 source / references / barrier / zero / encode / delete
只允许 1，不会静默忽略大于 1 的声明。内置图全部显式为 1，这是尚未调优的保守默认值。
`--strength 0` 把合法 solve 规范化为 zero，同时把并发度变为 1，完全跳过测量。
CLI resize 覆盖保留原节点的并发度，新插入节点为 1。没有全局 --threads 或并发环境变量。

每个已接入逐帧阶段的容量等于节点并发度 N：

```
accepted_not_delivered = queued_or_running + completed_waiting <= N
```

计算可乱序完成；只有轮到源帧顺序时才校验身份、有序提交并发出完成事件。结果即使已
提交，仍占名额，直到下游接收；不等待整波任务结束。prepare 的下游是轻量引用目录，
目录接收即释放 prepare 名额，不等待 Solve / Develop / Delete 全部消费。
测量特征、EV 表、引用目录和磁盘缓存仍可随总帧数增长；N 不是总字节或磁盘上限。
当前图像链继续保持串行资源保障；本节不宣称已接通图像节点并发。

参考白平衡只在源序第 0 帧 prepare 的有序提交中冻结，后续测量使用该只读值。
Solve 必须等待所有测量和匹配 EOF 封口，完整验证 ID、源摘要、EV 有限性与顺序。
同一节点的日志按源帧顺序提交，不同节点可交错；计算耗时不包含排队等待，任务耗时
之和不是整体墙钟时间。所有 prepare / measure / develop 进程按任务身份登记到同一个
run owner，单任务结束只注销自己；失败、停止或编码失败会取消并回收全部活跃任务，
取消后的晚到结果不提交。每个阶段/帧有独立 transcript，错误时留在 scratch。

## 4. 运行日志合同

每行：

```
wall=<epoch 秒.毫秒> elapsed=<单调秒.毫秒> kind=<事件> <有界字段...>
```

事件类别：

| kind | 内容 |
| --- | --- |
| `run.start` | input / video / log / scratch 路径 |
| `settings` | fps、白平衡模式、去闪参数、ROI、step 数、reference gain |
| `inventory` | 帧数、首尾文件名 |
| `graph` | 图来源（内置/路径）、节点 ID 顺序与实际 concurrency 配置 |
| `encoder` | encoder/container/pix_fmt/profile/range/几何 |
| `colour` | profile、相机颜色方法、matrix/primaries/transfer |
| `prepare` | 帧号、源名、路由、generated、缓存路径、耗时 |
| `measure` | 帧号、源名、耗时 |
| `develop` | 帧号、源名、EV、应用/自身白平衡、源与像素摘要、像素字节数、耗时 |
| `node` | 每个节点的逐帧/序列完成事件（节点顺序） |
| `delete` | 帧号、源名、被删缓存路径 |
| `exposure` | 显式 exposure 节点的 EV 与累计 EV |
| `encoder.command` | 实际 FFmpeg argv（一次） |
| `timing` / `timing.publish` | preflight / graph / validate / publish / total |
| `video` | 最终视频路径、字节、codec/profile/几何/帧数/像素格式/range/resize |
| `publish` / `run.complete` | 发布与正常结束 |
| `retained-cache` | 自定义图未删除生成缓存时的保留位置与数量 |
| `node.error` / `run.error` | 错误家族、有界 Converter/FFmpeg stderr 片段、scratch 保留路径 |

日志不写逐帧 metadata 文档、矩阵或像素；不写去闪 PASS/FAIL/REVIEW/质量评分。
stdout 只保留输入/输出路径、fps、去闪开关、帧清单、白平衡模式与结束摘要；
stderr 只保留错误与 scratch 保留提示。

## 5. Scratch 生命周期

每次运行创建视频同目录下的隐藏目录：

```
.rawtl-<video-stem>-<pid>-<nonce>/
  cache/frame_NNNNNN/            prepare 产物与 prepared.context
  transcripts/prepare|measure|develop/frame_NNNNNN.log
  transcripts/encode.log
  <video-file>                   编码中的 partial
```

- 选在视频同目录是为了同文件系统的一次 rename 发布，并让大缓存落在输出卷上。
- 正常结束：校验 → rename 发布。删除由 graph 决定：若 delete 节点已消费全部本 run
  生成的缓存文件（默认图如此），随后删除整个 scratch，目录内最终只剩视频与日志；
  若自定义图未删除生成的 DNG，则保留 scratch/cache，并在日志写
  `kind=retained-cache count=… path=<scratch>/cache`，stdout 同行提示
  `retained graph cache: …`。原始 RAW/DNG 永远不是删除目标。
- 失败/停止：停止调度、回收子进程、保留剩余 cache/transcripts/partial，在单条
  `run.error` 中写入 scratch 路径，退出非零且不发布正式视频。
- 重试：新 nonce 的新 scratch；不复用旧缓存，不覆盖旧视频/日志。没有恢复或
  checkpoint 承诺。

## 6. 编码与颜色

- 内建默认：`prores_ks` profile 4 / MOV / `yuv444p10le` / limited range / 源几何。
- 图节点可配置 encoder/container/pix_fmt/profile/rate/options；显式 CLI 覆盖图配置，
  内建默认最低。冲突或不支持组合在 RAW 处理前报错，不静默替换。
- 通用输出选项不得引入 `-i`、第二个输出、覆盖几何/fps/滤镜或颜色解释；这些由管线
  持有。
- 颜色由节点保持：Canon Cinema Gamut/CLog 不伪装 BT.2020 transfer/primaries；YCbCr
  矩阵与 range 按字段写标签；ProRes 默认 limited。
- 发布前验证：非空文件、ffprobe codec/profile/pix_fmt/几何/帧数/fps、逐字段颜色
  标签、容器可表达的 range、整段解码。失败只保留 partial 与非零退出。

## 7. 停止与错误

- SIGINT/SIGTERM 只设置原子标志；调度在每个节点边界检查，终止并回收 owned 子进程
  （native/Converter/FFmpeg），3 秒后升级 SIGKILL；退出码 130。
- 任一节点错误即停止后续调度并回收子进程；错误带真实退出状态/信号与有界 stderr。
- 质量完整性、探针、反馈或独立审计**不是**产品门；本产品运行没有这些阶段，也不会
  以质量问题改变退出码。

## 8. 相关文档

- `pipeline/README.md`：CLI、编码、颜色 profile、测试。
- `native/dng-linear-frame/README.md`：native prepare/develop 与 stdout 协议。
- `docs/migration-log.md`：本工作流迁移的记录与证据。
- `docs/architecture-v2.6.md`：历史目标架构（插件/生命周期），仅作历史标识。
