# 真实素材 V4 golden 基线（T00）

本目录冻结 Python V4（源基线 `f6d3ae7`）对只读素材
`/Users/ibobby/Temp/tl` 中 39 张 `TIMELAPSE_0709–0747.DNG` 的输出，
供后续估计器移植逐字段重放及端到端行为对照。素材本体不入库。
参数、代码身份、生成环境与文件校验值以 `manifest.json` 为准；
完整执行记录见仓库根目录的 `docs/migration-log.md` T00 条目。

## 两层 fixture

- **(a) 算法重放记录**：`measurement-input-basis.json` 记录素材逐文件
  SHA-256、renderer 配置、固定白平衡、quantum 与掩码规格；
  `frame-measurements.json` 记录 legacy `FrameMeasurement` 的实际全字段；
  39 个 `pixel-samples/*.f32le.bin` 保存 `log_pixel_samples` 的小端
  float32 数据。渲染帧本体不存储，按输入基准由冻结的 legacy renderer
  与原始素材确定性重导出。
- **(b) 端到端基线记录**：`pair-constraints.json` 保存候选边统计与可靠边；
  `exposure-plan.json` 保存估计结果；`end-to-end-baseline.json` 保存运行结果
  与报告；`temporal-audit.json` 保存编码后独立审计；
  `reference-video.mp4` 为参考视频。

## 产生域与对照边界

各 JSON 的 `production_domain` 注记表示旧链路的产生域：
rawpy / LibRaw → linear-sRGB → BT.709 系数亮度
（`0.2126, 0.7152, 0.0722`）。参考视频使用 sRGB OETF、BT.709
primaries/matrix 与 limited range。

这些记录用于 legacy 行为基线与质量对照，**不是未来 Adobe DNG Converter /
DNG SDK / ACEScg 链的数值 oracle**；两条链路不承诺像素等同。
算法逐字段对照须使用本目录记录的同一测量输入基准。

## 已观察到的基线状态

164 条候选边全部被拒绝，接受数为 **0/164**；39 帧全部
`REVIEW_REQUIRED`，`applied_ev` 全为 `null`。使用
`on_review=passthrough` 后，`run_status=PARTIAL`，
`execution_status=COMPLETED_WITH_REVIEW`。独立审计为 `SHORT_CONTEXT`，
`candidate_count=0`，不能据此形成真实去闪质量门结论。
这是被冻结的实际行为，不能把这些产物解释为成功去闪的证明。

## 确定性与再生成

`manifest.json` 的 `determinism` 策略限定为：同一机器、固定环境与相同素材下
逐字节一致。libx264 自动 frame threading 在本素材上不能保证位级复现，
因此生成器仅在生成时将编码命令固定为 `libx264 -threads 1`；
该设置只作用于生成器运行时，冻结的 `src/rawtl/**` 未作修改。
更换 FFmpeg/libx264 构建后不能直接承诺与旧视频逐字节一致。

在 `legacy/python/` 中，使用锁定环境与只读素材再生成：

```sh
PATH=/opt/homebrew/opt/ffmpeg-full/bin:$PATH \
  uv run python tools/generate_golden_fixtures.py \
  --input-dir /Users/ibobby/Temp/tl \
  --output-dir tests/fixtures/real-material
```

生成器只读取素材目录，不写入素材。`manifest.json` 记录生成文件的体积与
SHA-256；本说明 `README.md` **按设计豁免于 manifest 哈希清单**，
生成器与 `tests/test_golden_fixtures.py` 均显式将其列为例外，
因此补充本说明无需重新生成 fixture 或修改 manifest。
