# 迁移日志

本文件按 v2.6 §9.2 记录每个迁移任务的执行环境、命令、证据与未覆盖项。
每个 Task 独立提交，commit message 带任务号。

---

## T00 — 冻结 Python V4 基线并生成真实素材 golden

- 日期：2026-11-09
- 权威：`docs/architecture-v2.6.md` §9（T00 行）、§8（legacy/python/ 目标结构）、§9.2（强制交付）；`legacy/python/docs/exposure-contract.md` 的 V4 条款。
- 源基线：`backup/main-legacy` @ `f6d3ae7`（63 个 tracked 文件）。

### 1. 环境（实测）

| 项 | 值 |
| --- | --- |
| OS / 架构 | macOS 27.0.1 arm64 |
| uv | 0.12.0 (b88d7c5c4 2026-07-28 aarch64-apple-darwin) |
| Python（`uv sync` 解析） | 3.14.6（`requires-python >=3.12`） |
| numpy | 2.5.4 |
| scipy | 1.18.1 |
| rawpy / LibRaw | 0.28.0 / (0, 22, 2) |
| Pillow | 12.3.0 |
| pytest | 9.2.0 |
| ffmpeg / ffprobe | 8.0.1（Homebrew `ffmpeg-full`） |
| exiftool | 13.50 |

说明：`legacy/python/uv.lock` 自 `f6d3ae7` 原样保留，`uv sync --extra test` 按
该锁文件解析；`requirements.lock` 中的注释性版本指向与本次解析结果不同，以
`uv.lock` 与实际安装为准。执行命令前将
`/opt/homebrew/opt/ffmpeg-full/bin` 与 `/Users/ibobby/miniconda3/bin` 加入
`PATH`（当前非交互 shell 默认不加载这些目录）。

### 2. 源拷贝、排除与保真

- 从 `git-worktree/main-legacy` 执行 `git archive f6d3ae7` 得到 63 个 tracked
  文件，拷贝到 `legacy/python/`，并排除：
  - `*.DS_Store` ×4：`.DS_Store`、`src/.DS_Store`、`src/rawtl/.DS_Store`、`tests/.DS_Store`（macOS Finder 元数据）；
  - `.build/` 构建缓存元数据 ×2：`.build/.buildSystem_debug`、`.build/CACHEDIR.TAG`。
- 排除文件 SHA-256 与逐文件对比结果见 `.agent-work/tmp/legacy-baseline-golden/logs/copy-fidelity-final.txt`：57 个拷贝文件中 56 个逐字节一致；`.gitignore` 前缀完全一致、仅追加下述例外；`missing=0 differs=0`，`fidelity_status=0`。
- 唯一允许的新增/修改：`legacy/python/tools/generate_golden_fixtures.py`、`legacy/python/tests/fixtures/**`、`legacy/python/tests/test_golden_fixtures.py`、`legacy/python/.gitignore`（fixture 例外）。
- `legacy/python/.gitignore` 原样保留，仅追加显式例外：
  `!tests/fixtures/real-material/reference-video.mp4`（原规则 `*.mp4` 会挡住参考视频入库）。`src/rawtl/**` 零改动。
- `docs/architecture-v2.6.md` 由 `.agent-work/tmp/architecture-v2.6-source.md` 逐字节复制；二者与 `/Users/ibobby/Downloads/raw_timelapse_architecture_v2_6.md` 的 SHA-256 均为 `2ef2545a853a244289d5cde97b80c5922f7c29b31485724208701064d081ed11`（见 `logs/architecture-doc-sha256.txt`）。

### 3. 入库产物

- `legacy/python/`：冻结的 V4 实现（`src/rawtl/**`、`tests/**`、`docs/**`、`benchmarks/**`、`pyproject.toml`、`uv.lock`、`requirements.lock`、`README.md`、`LICENSE`、`.github/workflows/tests.yml`）。
- `docs/architecture-v2.6.md`：v2.6 规范入库。
- `docs/migration-log.md`：本文件。
- `legacy/python/tools/generate_golden_fixtures.py`：golden 再生成入口。
- `legacy/python/tests/test_golden_fixtures.py`：fixture 完整性/一致性回归测试（不依赖素材）。
- `legacy/python/tests/fixtures/real-material/`：46 个 manifest 列出的载荷文件 + `manifest.json`（共 47 个生成产物）+ `README.md`，共约 37 MB：
  - `measurement-input-basis.json`：renderer 配置、固定 WB、quantum、掩码规格、素材逐文件 SHA-256；
  - `frame-measurements.json` + `pixel-samples/*.f32le.bin`：39 帧 `FrameMeasurement` 全字段（含 `log_pixel_samples` 原始 float32）；
  - `pair-constraints.json`、`exposure-plan.json`：legacy 估计结果；
  - `end-to-end-baseline.json`、`temporal-audit.json`、`reference-video.mp4`：端到端行为基线与独立审计（审计 JSON 仅在 legacy 输出上附加 `production_domain` 注记，其余字段原样）；
  - `manifest.json`：文件体积/SHA-256、参数、代码与环境身份。

### 4. 执行的命令与证据

| 命令 | 真实退出码 | 证据 |
| --- | --- | --- |
| `uv sync --extra test` | 0 | `logs/uv-sync.log` |
| `.agent-work/tmp/legacy-baseline-golden/check_copy_fidelity.sh` | 0 | `logs/copy-fidelity.txt`（修改 .gitignore 前）、`logs/copy-fidelity-final.txt`（最终） |
| `uv run pytest -ra`（初始冻结树） | 0（136 passed，0 skipped，23 个测试文件） | `logs/pytest-full.log` |
| `uv run pytest -ra`（新增 fixture 回归后） | 0（141 passed，0 skipped，24 个测试文件） | `logs/pytest-full-final.log` |
| `uv run rawtl probe /Users/ibobby/Temp/tl` | 0 | `logs/probe-real-material.log` |
| `uv run rawtl run /Users/ibobby/Temp/tl -o <tmp> --encoder libx264 --temporal-quality strict --overwrite` | 5（默认 `--on-review error`，39 帧全部无可靠 EV） | `logs/trial-strict.log`、`trial-strict/exposure_report.json` |
| `uv run rawtl run /Users/ibobby/Temp/tl -o <tmp> --encoder libx264 --temporal-quality strict --on-review passthrough --overwrite` | 0 | `logs/trial-passthrough.log`、`trial-passthrough/` |
| `uv run python tools/generate_golden_fixtures.py --input-dir /Users/ibobby/Temp/tl --output-dir tests/fixtures/real-material`（第 1 次） | 0 | `logs/golden-run1.log` |
| 同上，`--output-dir .agent-work/tmp/legacy-baseline-golden/golden-run2`（第 2 次） | 0 | `logs/golden-run2.log` |
| `diff -r <run1> <run2>` | 0 | `logs/golden-determinism.txt` |
| 46 个生成文件 SHA-256 逐文件对比 | 0 | `logs/golden-run1.sha256`、`logs/golden-run2.sha256`、`logs/golden-run1-vs-run2-sha256.diff` |

三次完整命令的 stdout/stderr 与真实退出码均保存在
`.agent-work/tmp/legacy-baseline-golden/logs/`，未做过滤。

### 5. 测试结果

- 冻结树原始 23 个测试文件、136 个用例：`136 passed in 29.42s`，无 skip。
- 新增 `tests/test_golden_fixtures.py`（5 个用例）后：`141 passed in 24.04s`，无 skip。
- 该环境 ffmpeg/ffprobe 可用，因此原测试中的 FFmpeg 条件 skip 均未触发。
- 未发现需要外部素材才能运行的测试；素材只用于 golden 生成，不参与 pytest。

### 6. 真实素材上的冻结行为（事实记录）

- 素材：`/Users/ibobby/Temp/tl`，39 张 `TIMELAPSE_0709–0747.DNG`（DJI AC004，4000×2256，5 秒间隔，约 690 MB，只读）。
- 固定 WB：`[1.6526716947555542, 1.0, 2.072519063949585, 1.0]`；渲染几何 4000×2256；分析几何 1024×578；tile 网格 12×8 = 96。
- PairConstraint：164 个候选边全部被拒绝（`structure_mad` 164、`colour_change` 162、`insufficient_support` 164），**0 条可靠边**。
- ExposurePlan：39 帧全部 `REVIEW_REQUIRED`，`applied_ev` 全为 `null`；`scene_cuts` 30 条，`time_gaps` 无。
- 端到端：默认 `--on-review error` 直接失败（exit 5）。使用 `--on-review passthrough` 后 `run_status=PARTIAL` / `COMPLETED_WITH_REVIEW`；`encoder=libx264`；参考视频 30,797,138 字节，39 帧，yuv420p，BT.709 primaries/matrix，sRGB OETF，limited range。
- 编码后独立审计：`SHORT_CONTEXT`，`candidate_count=0`。素材逐帧内容变化，缺少同背景 pair，审计无法给出质量门结论。
- 解释：这是移动镜头素材在 legacy 默认算法下的真实行为，不是 fixture 生成失败；golden 如实冻结该行为，供后续 Rust 端口逐字段对照。

### 7. 确定性与 libx264 线程结论

- 两次生成的全部 47 个产物（46 个 manifest 列出的载荷文件 + `manifest.json`，不含 `README.md`）逐字节一致（`diff -r` 退出码 0，SHA-256 清单无差异）。
- 排查发现：同一份 rgb24 输入下，libx264 默认自动 frame threading 会产生不同 MP4 字节（`logs/encoder-determinism*` 与 `real_encode_probe.py` 证据：两次自动线程编码 SHA-256 不同；`-threads 1` 两次完全一致）。测量与估计字段不受影响。
- 处理：golden 生成器在运行时把 legacy FFmpeg 命令固定为 `-threads 1`（`pin_encoder_threads`，仅改 `rawtl.video.ffmpeg_command` 的运行时引用，`src/rawtl/**` 零改动），并在 `manifest.json` 的 `determinism` 段与 fixture `README.md` 中记录。参考视频是在该确定性编码配置下的产物。
- rawpy 渲染、measurement、estimate 与 exiftool 元数据在两次独立进程间均逐字节/逐字段一致（`rgb8_hash_probe.py`、`render_determinism_probe.py` 证据）。

### 8. 未覆盖 / 未验证项

- `NOT_TESTED`：Windows / Linux 上的复现（本次仅 macOS arm64）；旧测试在 Python 3.12 上的重放（本次由 `uv.lock` 解析到 Python 3.14.6）。
- `NOT_TESTED`：与 Adobe DNG Converter / DNG SDK Reference Development 的像素级对照（属阶段 B，需 Adobe 工具链）。
- `NOT_TESTED`：真实去闪质量门。该素材 0 条可靠 pair、审计 `SHORT_CONTEXT`，无法形成去闪质量结论。
- `NOT_APPLICABLE`：本次无 Rust/ABI/Schema 变更，§9.2.2 的 schema diff 与 ABI 兼容性结论不适用。
- `UNVERIFIED`：golden 视频的逐字节可复现性依赖固定的 FFmpeg/libx264 构建；更换 FFmpeg 版本后需重新生成，不应与旧 golden 做字节比较（数值/审计对照仍有效）。
- §9.2.1 的“commit message 带 T00”由主控在检查 diff 后提交时落实；本执行者不 commit。
