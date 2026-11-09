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

## 2026-11-09 — S01 / T08：native DNG → camera-linear FP32 → ACEScg → RGB16 PNG

本节 primary task marker **T08**，竖切同时覆盖 T06/T07/T09 的严格路由、SDK 语义归属、颜色和显式量化语义。实现位于 `native/dng-linear-frame/`；仅发布单帧 CLI/PNG/JSON producer，consumer 为 S02。未执行序列编排、视频编码或 F01 整体旅程验收。提交由 Main 按 worker 边界负责，本执行者没有 commit/push/分支操作。

### 环境与 vendor

- macOS 27.0.1 / 26A434，arm64；Apple clang 21.0.0（clang-2100.3.34.2），C++20；CMake 4.3.3。
- Adobe DNG Converter bundle version `18.7 (2743)` / `18.7f2743`；路径 `/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter`。
- SDK ZIP `/Users/ibobby/Downloads/dng_sdk_1_7_1_2768_20261013.zip`，SHA-256 `c1d3a545ed45b150e59af2e6aa0695c1551f0e2bed34ad87436898a05194d2b1`；SDK `1.7.1 (2768)`。解包到忽略目录 `native/vendor/dng-sdk-1.7.1/`，SDK 源码不修改、不入库。
- **清单纠正**：ZIP 有 4215 个 entries，实际为 **3769 个文件 + 446 个目录**，并非 4215 个普通文件。普通文件分布：xmp 2566、libjxl 805、dng_sdk 193、libjpeg 184、sample_files 14，另 7 个根说明/许可证/规范文件。完整清单：`.agent-work/tmp/dng-linear-frame/logs/vendor-manifest.json`。全部 3769 个解包文件逐字节与 ZIP 对照相同。
- SDK 和 bundled XMP core 从源编译；libpng 1.6.59、libjpeg ABI80（jpeg-turbo 8.3.2 dylib version）、libjxl 0.11.1 / threads 与系统 zlib 链接。本机 CMake 默认会优先搜索 Mono framework 的旧 PNG/JPEG 头文件；现设置 `CMAKE_FIND_FRAMEWORK=LAST`，最终新构建使用 `/opt/homebrew/include` 与对应 Homebrew 库。没有修改 SDK 以规避 XMP/JXL 依赖；XMP core 开启、XMPFiles/DocOps 和 VC5 关闭。
- 环境、动态依赖证据：`logs/environment.*`；最终 configure 的精确 include/library 路径见 `build-matched/CMakeCache.txt` 和 `logs/configure-matched.*`。以下相对证据路径统一以 `.agent-work/tmp/dng-linear-frame/` 为根。

### Converter `-l` 实测（AC1）

1. 单文件实际 argv：`"/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter" -c -l -d /Users/ibobby/Projects/raw-timelapse/.agent-work/tmp/dng-linear-frame/converter-first /Users/ibobby/Temp/tl/TIMELAPSE_0709.DNG`。退出 **0**，产物命名 `TIMELAPSE_0709.dng`（扩展名小写），有 GPU-disabled 警告。ExifTool 对照：IFD0 为 RGB 缩略图；**SubIFD 的 SubfileType=0、PhotometricInterpretation=34892**，4000×2256、RGB16、JPEG compression。判型不能用 IFD0 缩略图。
2. 把 `/Users/ibobby/Temp/tl` 目录直接作为 source 参数：Converter 退出 **1**，`Invalid source file`。保留失败证据 `logs/converter-all39.*`，不声称目录输入可用。
3. 批量成功形式：同一 `-c -l -d <绝对运行临时目录>` 后传 **39 个明确文件参数**，按文件名排序，从 `TIMELAPSE_0709.DNG` 至 `TIMELAPSE_0747.DNG`，Converter 退出 **0**。完整实际 argv、所有 stdout/stderr 与每张输出的 ExifTool 主 IFD 检查见 `logs/converter-all39-files.*`；复现脚本 `probe_converter.py`。
4. **39/39** 输出均逐张检查 `SubfileType=0` 对应的 `PhotometricInterpretation==34892`，不以抽样或 Converter exit0 代替验证；输出文件数也为 39。汇总 `logs/converter-all39-summary.json`。临时批量目录 `converter-all39-uwxpeacf` 用后删除，`CLEANED ... True` 记录在完整日志；失败试验目录同样清理。单帧运行使用系统普通 `mkdtemp`，成功侧车记录目录、清理状态及 Converter transcript；失败/超时在 stderr 记录清理。最初单帧探针 LinearDNG 在完成直接路由检查后删除，见最终 direct probe 日志。
5. Converter 改变存储/颜色元数据：第一帧转换后 BlackLevel 为 `[1028,1028,1028]`、WhiteLevel `[65535,65535,65535]`，不是源 CFA 的黑白电平；默认嵌入 profile 也与原 CFA ColorMatrix 不同。解码使用 **当前 LinearDNG** 的 levels/profile，绝不重复减源黑电平。Converter 还把输出 AsShotNeutral 舍入至约六位；CFA 路径默认 WB 保留原源文件 rational 精度，direct 路径默认使用其实际输入文件的 neutral。跨输入比较需显式传播同一个原始 neutral，不能假设两份文件默认 WB 相同。

### SDK 与颜色语义（AC2/AC3）

- SDK 路径是 `ReadStage1Image → BuildStage2Image → BuildStage3Image`。adapter 仅把 SDK `Linearize` 的 destination 设为 FP32；线性化表、非零黑电平、白电平、适用 opcodes 均由 SDK 执行。无 `dng_render`、无 LibRaw/fallback；CFA 在 SDK 入口再被 guard 拒绝。
- 16×16 受控合成 LinearRaw 探针：零黑控制的 `[250,500,750] / 1000 → [0.25,0.5,0.75]`；2×2 各位置/通道不同的非零 BlackLevel 用逐像素独立预期校验；WhiteLevel=2000 把控制值减半；2×LUT 与非零 black 联合用例确认 **先 LUT，后减 black**。实际 SDK 使用每通道 `WhiteLevel-MaxBlack` 作分母并扣局部 black。
- OpcodeList1/2/3 分别注入已知 MapPolynomial(0.5×) 并逐像素验证，再验证三列表组合；JSON 记录每 stage 的 opcode ID/flags、数量和 SDK dispatch。空列表记录 absent；非空记录 dispatch 已完成，**不声称所有任意 optional opcode 都一定执行**。
- 同像素/元数据仅把 BaselineExposure 从 0 改为 1.9 的探针，camera FP32 **完全相同**，PNG 字节也相同；JSON `applied=false`。符合计划的“不并入 decode”预期，未做静默 EV 抵消或重标尺。
- **源事实修正**：真实 spot frames 0709/0728/0747 的 BaselineExposure 实际是 **1.90 / 1.93 / 1.88 EV**，ExifTool 原源文件确认；并非全序列固定 1.9。首次 real test 的固定 1.9 断言因此失败，修正测试假设后逐帧原样记录。证据 `logs/baseline-spot.*`、初始失败 `logs/real-material.*`，保留未覆盖成成功。
- **可观察上游裁切限制**：受控 stored `[0,1000,2000]`、black100、white1000 被 SDK integer Linearize 归一到 `[0,1,1]`，FP32 destination 也不能恢复低于黑/超白信息。JSON 明确记录，未冒称 decode 全范围保真；Converter 的去马赛克 clipping/其他 baking 为 **UNKNOWN**。Stage3 残余黑电平非零会显式拒绝。
- 颜色走 DNG 第6章 no-ForwardMatrix 路径：取 SDK 依 CalibrationIlluminant/neutral/signature 选择的 `AB*CC*CM*MapWhiteMatrix(D50, selectedWhite)` 并做 SDK 白饱和归一化，**取逆已包含 selected-white→D50，绝不再叠 CA**；之后独立显式 Bradford D50→D60、XYZ→AP1。即使文件提供 ForwardMatrix，本路径也不用它。JSON 记录实际 profile/illuminants、CM1–3、AB/CC/signatures、所选白与合成矩阵。
- 独立 `tests/reference_color.py` 使用 Decimal 45位、余子式逆矩阵、规范 `CA*Inverse(AB*CC*CM)` 分步构造，并从 AP1 primaries 推导 XYZ 矩阵；不调用生产 helper/SDK 生成参考值。真实非对称 DJI ColorMatrix1、neutral/非中性/含负值向量、AB/CC 及 override 参考误差阈值为 `3e-6`，可拒绝方向/转置、漏适应或双适应错误。SDK profile normalization/四位舍入单独建模。最初该测试发现 SDK neutral setter 六位舍入，adapter 现读取原 rational neutral；未扩大容差掩盖差异。
- FP32 scene 域保留负值和 >1；受控 blue-frame 验证 min<0/max>1，实际 PNG 的负通道为0。量化已知值独立覆盖 0、负值、1→16384、0.125→2048、精确上界、超上界；不以编码往返代替。RGB16 PNG payload 由测试独立解压验证位深/通道/字节序、线性码值及无 gamma/sRGB chunk。

### 确定性与最终检查

所有长/完整命令用工作目录 `run.py` 捕获完整 stdout、stderr、真实退出码与 argv；`logs/*.json` 是各命令记录（不是从管道末端推断退出码）。失败 build/test 记录保留。最终检查命令及精确结果见下表及 `logs/run-index.json`。

| 命令/输入选择 | 真实退出码与结果 | 证据 |
| --- | --- | --- |
| SDK 解包 + ZIP hash/清单；受控 Converter 探针与 all39 显式文件批量 | 解包0；单文件0；目录试验1；39文件批量0，39张验证均0 | `logs/sdk-extract.*`、`logs/converter-first.*`、`logs/converter-first-tags.*`、`logs/converter-all39.*`、`logs/converter-all39-files.*` |
| `cmake -S native/dng-linear-frame -B .agent-work/tmp/dng-linear-frame/build-matched -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=/opt/homebrew`；`cmake --build .../build-matched -j 8` | 0（全新构建）；最初禁用 XMP 的 build exit2，随后 expat 路径、namespace define、公有 JXL include 问题各自保留失败记录 | `logs/configure-matched.*`、`logs/build-matched.*`、`build-investigation.md` |
| `ctest --test-dir .agent-work/tmp/dng-linear-frame/build-matched --output-on-failure` | 0，2/2 tests pass；此前两组 CTest pass，初始 independent-reference 失败保留 | `logs/ctest-matched.*`、`logs/ctest-matched-detail.log` |
| `python3 native/dng-linear-frame/tests/real_material.py <build-matched/dng-linear-frame> /Users/ibobby/Temp/tl <work>/real-matched` | 0，三组 PNG 字节一致；0709/0728/0747 各两跑，加末帧 frozen-WB，一共7次 CLI | `logs/real-material-matched.*`、`real-matched/summary.json`、`real-matched/logs/` |
| direct LinearRaw 两跑；原始 neutral override 与 CFA 路径比较；删除初始探针 LinearDNG | 0，两跑 direct PNG 相同；原源 neutral override 与 CFA PNG 相同；初始 LinearDNG 目录已删除。早期附加跨输入默认 WB 相等断言错误，失败 retained（两次相同 direct 输入本身已一致） | `logs/direct-corrected.*`；失败 `logs/direct-final.*` |
| ZIP 解包树逐文件对比；39源 DNG SHA-256 与 pre-S01 冻结 golden manifest 对照；冻结架构 hash | 0，3769 vendor 文件相同、39材料 hash 相同、架构 SHA-256 未变 | `source_integrity.py`、`logs/source-integrity-final.*` |

最终 matched-header 构建获三组 PNG 字节一致（与较早构建相同）：0709 `7ed59b1c…`，0728 `dcf6913b…`，0747 `7e5c66b4…`；场景范围分别约 `[-0.1503743,2.7998750]`、`[-0.2580652,3.9398568]`、`[-0.0156245,3.2573314]`，上界裁切计数皆0，负值裁切计数逐帧记录。末帧 frozen-WB 的 applied values 与0709一致，同时保留0747自己的 neutral。完整数值、hash 与检查状态见 `real-matched/summary.json`。

### NOT_TESTED / UNKNOWN / 边界

- **NOT_TESTED**：其余36帧完整 native PNG 解码、39帧 native→视频组合、FFV1往返与预览（S02 owner）；本节的 **all39** 证据只覆盖 Converter→LinearRaw 主 IFD 路由。
- **NOT_TESTED**：Windows/Linux（当前适配器显式 macOS only）、其他相机、多通道 LinearRaw、BigTIFF、dual/triple-illuminant 的独立数值 oracle、float-source range、BlackLevelDeltaH/V 独立扰动、所有 opcode 类型与未知 optional opcode 独立像素 oracle。
- **UNKNOWN**：Converter demosaic clipping/其他 metadata baking 的详细归属；源 CFA 与 Converter 输出不是声明数值等同的像素域。SDK integer `[0,1]` 裁切是已测限制；无恢复/补偿 fallback。
- **不声称**：与冻结 legacy/rawpy 的数值等同、绝对反射率、物理曝光参考或跨工具链 PNG 字节一致。输出是 SDK white-saturation normalization 的相机线性→ACEScg，不施加 BaselineExposure。
- JSON 含运行临时路径，确定性合同针对 PNG。PNG/JSON 逐文件发布，I/O 在两次 rename 间失败可留下 PNG，退出非零；S02 必须先检查进程状态。
- independent code review、Section acceptance、后继释放与最终 T08 commit 由 Main 负责，本报告不替代它们。

## 2026-11-09 — S02 / T14-adjacent：帧序列管线与编码视频（最小整体系统）

本节 primary task marker **T14**（FFmpeg encoder / 高位深 / profile sidecar / 编码后独立审计）的最小系统竖切：只实现用户 COR-003/REQ-007 范围内的“DNG 帧序列 → 16-bit FFV1 线性 MKV + sRGB 预览”，不包含 v2.6 的 encoder 插件 ABI、官方 profile 系统、事件/图执行器或完整审计框架。实现位于 `pipeline/`（Rust 单 crate，`rawtl-linear-video`），消费 S01 已接受的 `native/dng-linear-frame` CLI/JSON/PNG 契约；未修改 native 工具、legacy/python 或 `docs/architecture-v2.6.md`。提交由 Main 负责，本执行者没有 commit/push/分支操作。

### 环境与命令

- macOS 27.0.1 / arm64；rustc/cargo 1.99.1（Homebrew）；ffmpeg/ffprobe 8.0.1（`/opt/homebrew/opt/ffmpeg-full/bin`，fixed default 并记录于 manifest）；native 工具使用 S01 的 matched build `.agent-work/tmp/dng-linear-frame/build-matched/dng-linear-frame`（SDK 1.7.1/2768，Converter 18.7 per S01）。
- 构建：`cargo clean` + `cargo build --release`（`pipeline/target/release/rawtl-linear-video`），exit 0；`logs/cargo-build-release3.log`。
- 真实样例命令（39 帧，默认 fps，默认 ffmpeg/ffprobe）：

  ```sh
  pipeline/target/release/rawtl-linear-video \
    --input /Users/ibobby/Temp/tl \
    --output outputs/linear-video-pipeline-e2e \
    --dng-linear-frame .agent-work/tmp/dng-linear-frame/build-matched/dng-linear-frame
  ```

  真实退出码 **0**，`real 230.93s`；完整 stdout/stderr 在 `.agent-work/tmp/linear-video-pipeline/logs/e2e-run.log`，退出码文件 `e2e-run.exit`。输入素材只读；交付物在 `outputs/linear-video-pipeline-e2e/`（`/outputs/` 已忽略，不入库）。

### 管线语义与实测

- **排序**：枚举大小写不敏感 `.dng`，按文件名 text/digit run 自然序（数字按数值比较）排序；无数字段、仅大小写不同、数字拼写等价（`IMG_1` vs `IMG_01`）均显式拒绝，无 lexicographic fallback。真实序列 `TIMELAPSE_0709–0747` 按数值顺序 39/39。
- **序列冻结 WB（REQ-008）**：首帧默认 as-shot 解码，取其 `applied_white_balance` = `0.605080831408776, 1, 0.48250460405156537`（即 0709 AsShotNeutral），作为 `--white-balance` 传给其余 38 帧。39/39 JSON 的 applied 值逐位相同，首帧 `white_balance_source=as-shot`、其余 38 帧 `override`；末帧 0747 保留自身 `as_shot_neutral = 0.6425505824647455, 1, 0.4488222698072805`。逐帧 own neutral 与 applied 均记录在 manifest。独立校验脚本 `verify_e2e.py` 结果 PASS（`logs/e2e-verification.txt`，exit 0）。
- **fps 来源**：默认 `30/1`，provenance=`legacy/python/src/rawtl/cli.py:91 (argparse default Fraction(30))`；本次未用 `--fps` override。manifest 记录 value/decimal/provenance。
- **线性编码参数（显式，无隐藏重采样/色彩转换）**：`-framerate 30/1 -start_number 1 -i frames/frame_%06d.png -frames:v 39 -c:v ffv1 -level 3 -g 1 -slicecrc 1 -pix_fmt gbrp16le -map_metadata -1 -f matroska`。ffprobe：`codec_name=ffv1`、`pix_fmt=gbrp16le`、4000×2256、`nb_read_frames=39`、`avg_frame_rate=30/1`；FFV1 文件 583,584,930 bytes，SHA-256 `9aae33e5dfc9306fae753b49711e10fe2dd8989f95baea4d6e455a4acb16f25b`（`logs/ffprobe-linear.txt`、`logs/deliverable-sha256.txt`）。
- **量化政策消费**：管线本身不做额外量化；每帧 PNG 的 `code/16384` 标尺与 S01 量化政策（nearest, ties upward；负值→0；>65535/16384→65535）由 native JSON 逐帧记录（scale/rounding/两端裁切计数、observed scene min/max）。FFV1 只做无损 RGB16 传输；preview 的 8-bit 量化是独立、显式声明的显示量化（见下）。
- **无损往返验证**：编码后先写 `.rawtl-partial/linear.mkv`，ffprobe 通过后再把送入的 PNG 序列与编码 MKV 都解码为 `rgb48le`，逐帧逐字节比较（39×54,144,000 bytes/frame）；结果 byte-identical（6.67s），`logs/roundtrip.log` 记 `result: PASS`。独立第二 oracle：`ffmpeg -f framemd5 -pix_fmt gbrp16le` 对 PNG 序列与 MKV 各生成 39 行哈希，`diff` 为空、exit 0（`logs/framemd5-png.txt`、`framemd5-video.txt`、`framemd5-diff.txt`）。
- **发布语义**：两个交付物均先写运行期临时目录，验证通过后同目录 `rename` 原子发布；成功时临时目录删除（实测 `.rawtl-partial` 不存在），失败时以可区分名保留并打印路径。首选测到一次真实缺陷：首次 39 帧运行的 preview 虽可解码，但外部 ffprobe 显示 `color_primaries/transfer=unknown`；原因是 ffmpeg 8.0.1 的 `scale` filter 重置了帧色彩属性。修复为在 filter chain 追加 `setparams=color_primaries=bt709:color_trc=iec61966-2-1:colorspace=bt709:range=tv`，并把四项 tag 加入产品验证。首次运行日志保留为 `logs/*-pretagfix.*`（未删改）；第二次运行（上述 exit 0）是最终证据，ffprobe 显示 `color_space=bt709`、`color_primaries=bt709`、`color_transfer=iec61966-2-1`、`color_range=tv`。
- **sRGB preview 颜色契约**：`code/16384` 解释为 AP1/D60 场景线性；显式矩阵 AP1/D60→XYZ(D60)→Bradford D60→D65→linear sRGB(D65)（矩阵与 white points 记录于 manifest 与 `pipeline/README.md`）；逐通道裁切到 `[0,1]`（负值/域外裁切，无 tone mapping）；IEC 61966-2-1 sRGB OETF；`round(value*255)` 量化到 8-bit。转换在 Rust 中完成（ffmpeg 仅把 PNG 解码为 rgb48le），随后 libx264 `-preset medium -crf 18 -pix_fmt yuv420p`，显式 `scale=out_color_matrix=bt709:out_range=tv` + `setparams`，tag bt709/bt709/iec61966-2-1/tv。preview = 8,344,613 bytes，SHA-256 `9192449d05e46f62e62ff09b986ff41bd5758b2048b76a81dbd5eee3aa6b3017`；ffprobe h264/yuv420p/4000×2256/39/30fps，并通过完整 `-f null -` 解码。数值单测：neutral 0.18→`118,118,118`；非中性 `[0.5,0.25,0.125]`→linear `[0.686670107181761,0.218754435511259,0.099877619173666]`→8-bit `[216,129,89]`；并把“把 AP1 当 sRGB”错误路径（`[188,137,99]`）作为反例拒绝。抽取 preview 第 1/21 帧目检：夜间行车场景，红色尾灯/白色车灯/黄色车道线/仪表盘颜色合理。

### 检查与证据

| 命令/选择 | 真实退出码与结果 | 证据（根=`.agent-work/tmp/linear-video-pipeline/`） |
| --- | --- | --- |
| `cargo clean && cargo test -- --test-threads=4` | 0，34 unit + 8 integration，0 failed | `logs/cargo-test-dev3.log` |
| 39 帧真实端到端（命令见上） | 0；frames 162.71s；total 230.76s | `logs/e2e-run.log`、`e2e-run.exit` |
| `python3 -I verify_e2e.py outputs/linear-video-pipeline-e2e` | 0，RESULT: PASS（39 帧 WB/来源/尺寸/文件/roundtrip 全核） | `logs/e2e-verification.txt`、`.exit` |
| `ffprobe -count_frames`（linear/preview） | 0/0；见上文参数 | `logs/ffprobe-linear.txt`、`logs/ffprobe-preview.txt`、`.exit` |
| PNG 序列 vs MKV `framemd5`（独立实现） | 0/0；39/39 行相同，`diff` exit 0 | `logs/framemd5-*.txt`、`logs/framemd5-diff.exit` |
| 交付物/39 帧 PNG SHA-256 | 0 | `logs/deliverable-sha256.txt`、`logs/frame-png-sha256.txt` |
| 集成测试注入场景（fake native + real ffmpeg） | 0；覆盖乱序/自然序、WB 漂移检测、坏帧 fail-fast（exit 30、无视频）、编码器写字节后失败（exit 40、仅有 `.rawtl-partial/linear.mkv`）、ffprobe 拒绝（exit 50）、roundtrip 不一致（exit 50）、目录重叠（exit 11）、非空输出（exit 12） | `pipeline/tests/pipeline_cli.rs`、`cargo-test-dev3.log` |
| 首次 39 帧运行（preview tag 缺陷，已修复） | 0，但外部 ffprobe 显示 preview primaries/transfer unknown；作为缺陷证据保留 | `logs/e2e-run-pretagfix.log`、`ffprobe-preview-pretagfix.txt` |

### NOT_TESTED / UNKNOWN / 边界

- **NOT_TESTED**：Windows/Linux（产品路径当前以 macOS + Homebrew ffmpeg 为默认；仅 macOS arm64 实测）；`--fps` 非 30 的真实素材端到端（`30000/1001` 等仅单测/参数解析覆盖）；`--converter` / `--converter-timeout` 的真实素材 pass-through（本次用 native 默认）；其他相机、非三通道、direct LinearRaw 输入经完整管线的组合（S01 已覆盖单帧 direct 解码，S02 样例全为 CFA）；中断/信号处理与磁盘满/超大序列的清理保证；播放器兼容性（4K H.264/yuv420p、tag 是否被各播放器尊重）；preview 与外部 sRGB 参考/校准显示器的颜色一致性（仅数值单测 + 两帧目检）；视频编码字节确定性（FFV1/libx264 受线程/版本影响，只对内容与 lossless 往返作声明）；音频/字幕/多视频流（本来不产生）；硬件编码器。
- **UNKNOWN**：Converter demosaic clipping 与 metadata baking 的详细归属（S01 继承）；播放器端 sRGB/bt709 tag 的实际显示解释；ffmpeg 8.0.1 `scale` filter 重置 primaries/trc 的规范依据（以实测和 `setparams` 修复为准，最终 tag 已由 ffprobe 验证）。
- **不声称**：与 legacy/rawpy 的数值等同；视频字节可复现；preview 是可测量的交付域（它只是 display-referred 眼检预览）；`.DS_Store` 等 Finder 元数据不影响交付物。
- JSON/manifest 含运行路径与耗时，非确定性合同的一部分；确定性证据针对原生 PNG（S01）与本次无损往返（内容级）。
- 本节完成的是 T14 邻近编码竖切的最小系统语义；T14 的插件/官方 profile/审计框架、T13 的 Log 输出、T15/T16 等不在本计划范围。independent code review、Section acceptance 与 commit 由 Main 负责。

## 2026-11-09 — S03 / F01：去闪节点管线集成（两遍编排/验证反馈/独立审计/报告/端到端）

本节对应用户任务 COR-004（把 legacy V4 去闪子系统重建为当前系统的处理节点）在 F01 的 S03 竖切：CLI 参数、两遍编排、增益探针、渲染验证与 ≤2 次反馈、独立编码视频审计、曝光报告/曲线、manifest 去闪块、原子发布与 `--strength 0` 回归路径。实现位于 `pipeline/src/{main,cli,error,frame,manifest,audit,deflicker_run}.rs`、`pipeline/tests/**`、`pipeline/README.md`；除 Main 授权的 `native/dng-linear-frame/README.md` 交叉引用（repair-1）外未修改 `native/**`（S01 冻结）；未修改 `pipeline/src/deflicker/**` 引擎内部、`git-worktree/**`、颜色/量化/WB 决策。提交由 Main 负责，本执行者没有 commit/push/分支操作。

### 环境与构建

- macOS 27.0（darwin 27.0.0）arm64；rustc/cargo 1.99.1（Homebrew）；ffmpeg/ffprobe 8.0.1（`/opt/homebrew/opt/ffmpeg-full/bin`）。
- native 工具从当前源码全新构建（构建目录先清空）：`cmake -S native/dng-linear-frame -B .agent-work/tmp/deflicker-node/s03/native-build -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=/opt/homebrew` → `cmake --build ... -j 8`，exit 0；`ctest --test-dir ... --output-on-failure` → 3/3 pass（`logs/native-configure.log`、`native-build.log`、`native-ctest.log`）。未复用 S01 旧二进制。
- pipeline：`cargo clean` + `cargo build --release`，exit 0（`logs/pipeline-build-release*.log`）。修复轮次后的最终 `cargo test`：**90 lib + 42 bin 单测 + 17 集成测试 = 149 passed，0 failed**，exit 0（repair-2 证据 `repair-2/repair-2-cargo-test.*`；初版 145 为修复前计数，已被取代）。

### CLI 与默认值

- 新增 `--strength`（1.0）、`--period`（60，>=6）、`--brightness smooth|uniform`、`--estimator pairwise|legacy`、`--ev-limits none|LOW,HIGH`、`--roi X,Y,W,H`、`--exclude-roi X0,Y0,X1,Y1`（可重复）、`--step START:EV`（可重复、0 基）、`--on-review error|passthrough`、`--temporal-quality strict|report|off`。默认值即 legacy V4 默认（`legacy/python/src/rawtl/cli.py:95-114`），去闪默认开启。
- 新增退出码：80 `REVIEW_REQUIRED`、81 增益探针失败、82 `TARGET_NOT_REACHED`、83 编码审计 strict `FAIL`；现有 2/10/11/12/20/30/40/50/60/70 不变。USAGE 与 `pipeline/README.md` 已同步。
- `--strength 0`：单遍、无分析/探针/验证/审计执行，报告逐帧 `DISABLED`，manifest `deflicker.enabled=false`；native 参数形状与改动前完全一致（不附加 `--exposure-ev/--emit-analysis/--no-png`）。

### 编排语义（同步 CLI）

分析遍（`--no-png --emit-analysis`，0 EV，序列冻结 WB 不变）→ capture-time 连续性（中位标称间隔、gap_factor 1.5、≥3 样本；文件名数字跳变仅报告元数据）→ S02 估计 → REVIEW 策略（默认 error→80；passthrough→0 EV 渲染并标记 `PASSTHROUGH_UNCORRECTED_FRAME`，报告 `PARTIAL`）→ 增益探针（strength 0 / n<8 / 无可靠估计→`SKIPPED`；否则代表帧 ≤5 × ±0.25 EV，门 median≤0.02、P95≤0.05）→ 应用遍（逐帧 `--exposure-ev=applied --emit-analysis` + PNG）→ `evaluate_render`（不可观测→`NOT_VERIFIED_NO_CONSENSUS`/`NOT_INDEPENDENT_LEGACY`，绝不伪造 PASS）→ ≤2 次反馈（只改 applied EV；发散保最优；仍不达标→82 且不发布）→ 现有编码/ffprobe/无损往返/预览 → 独立审计（ffmpeg `rgb48le` 解码线性母版、`linear16`（code/16384）逆传递、f64 BOX 面积降采样、24×18 paired photometry；边界=scene cuts ∪ capture-time gaps；交付码饱和代理 `code>=int(0.965*65535)`，与 sidecar 的 16057 线性刻度序列明确区分）→ `exposure_report.json` + `exposure-curve.csv`（守卫后的失败也写；报告写失败不掩盖原始错误）→ manifest `deflicker` 块 → 原子发布。`new_output_clipping_fraction` = applied 与 0 EV baseline 的逐 tile `output_clip_fraction` 正增量之和 / tile 数。守卫后的失败一律保留报告——包括首帧之前的必需可执行文件检查（exit 20）与源枚举（exit 10）失败（报告允许空 frames）；守卫/用法拒绝仍不产生报告。分析 sidecar 的 `exposure_ev` 按 producer 的 10 位有效数字精度校验（精确 10 位格式或半 ulp 界），native metadata JSON 的 `exposure_ev` 仍按全精度精确校验；因此分数 EV 不会误报 exit 30，而实质错误的增益仍被拒绝。

### 真实样例（39 帧，默认参数）— 如实结果

命令：`rawtl-linear-video --input /Users/ibobby/Temp/tl --output outputs/deflicker-node-e2e --dng-linear-frame <fresh native>`。真实退出码 **80**，`run_status=REVIEW_REQUIRED`：分析遍 39 帧 40.74s，估计后 **39/39 帧 REVIEW_REQUIRED**；pairwise 候选边 164、可靠边 0，拒绝原因 `structure_mad=164 / colour_change=164 / insufficient_support=164`；scene cuts 29，capture-time gaps 0（标称间隔 5s）。增益探针未执行（REVIEW 在探针前终止）。这不是实现缺陷：冻结 legacy golden `legacy/python/tests/fixtures/real-material/exposure-plan.json` 在同一素材、同一默认参数下达到 **状态级等价**——39/39 REVIEW_REQUIRED、0 条可靠边（164 条候选全部拒绝，per-lag 拒绝数 38/37/35/31/23 完全一致）；拒绝原因计数有 2 条差异：golden 为 structure_mad=164、colour_change=162、insufficient_support=164，本次为 164/164/164；scene cuts 30（本次 29，阈值边缘）。真实素材是夜间行车延时（每帧 5s，画面内容持续变化），matched-background 估计器没有可用背景。按 legacy 语义，默认 `on_review=error` 应在该状态失败，故未发布视频；`exposure_report.json`/`exposure-curve.csv` 已写。域映射导致的数值边界差异：scene cuts 29 vs legacy 30（阈值边缘），不改变状态结论。

为完整覆盖后半程，补充一次 `--on-review passthrough`（其余默认）：exit **0**，`PARTIAL`/`COMPLETED_WITH_REVIEW`，39 帧以 0 EV 输出并逐帧标记，探针 `SKIPPED`（无可靠曝光），审计 `SHORT_CONTEXT`（29 个边界→30 段，0 events），FFV1 无损往返 `PASS`，线性视频/预览/报告/曲线/`temporal_audit.json`/`pairs.csv`/`frames.csv` 齐备；ffprobe：`ffv1/gbrp16le/4000x2256/39 帧/30 fps`，preview `h264/yuv420p/4000x2256/39`。该补充运行不属于 AC2 的“默认参数”判定，仅证明失败状态之外的链路。

### REQ-011 回归（`--strength 0`，真实样例）— 需要 Main/owner 决策

- 39 帧 PNG：与存档 `.agent-work/tmp/linear-video-pipeline/logs/frame-png-sha256.txt` **39/39 逐字节一致**（mismatched=0，`e2e/regression/frame-compare.txt`）。
- preview MP4：SHA-256 与存档一致 `9192449d…`（派生物，不作 REQ-011 判定）。
- 线性母版 MKV：文件大小相同（583,584,930 bytes），但 SHA-256 不同（本次 `27a86c23…`，存档 `9aae33e5…`）。差异仅 **43 字节**，全部位于 Matroska 头部（`DateUTC` 220–223、`SegmentUID` 264–279，以及头部 CRC/时间相关字段）；解码后的 FFV1 内容逐帧 **39/39 framemd5 相同**（`regression-framemd5-compare.txt`：mismatched_frames=0），进程内无损往返亦 `PASS`。
- 根因与实现无关：同一份 PNG 序列用同一 ffmpeg 8.0.1 命令连续 mux 两次即产生不同 MKV（实验记录见 `e2e/regression/container-determinism-experiment.txt`；`-fflags +bitexact -flags +bitexact` 也不能消除随机 SegmentUID/DateUTC）。因此“线性母版 MKV 逐字节等于 @f461baa 存档”在**当前编码器字节确定性边界**内不可实现；PNG 部分（REQ-011 的确定性主体）已逐字节满足。
- 按 TASK 要求，本节**不将 framemd5 内容一致视为 REQ-011 通过**：线性母版字节不一致须上报 Main，由 owner 决策“修订 REQ-011 为内容/无损往返判定，或引入可复现的 muxer 方案并重制基线”。本执行者未修改编码参数或基线。

### 报告确定性

默认参数运行（失败路径）重跑一次（独立输出目录）：两次 `exposure_report.json` 在归一化 `config.output` 后**逐字节相同**（浮点固定格式、无耗时/时间戳字段），两次退出码均 80（`e2e/default/report-determinism.txt`）。

### 检查与证据

| 命令/选择 | 真实退出码与结果 | 证据（根=`.agent-work/tmp/deflicker-node/s03/`） |
| --- | --- | --- |
| native 全新 configure/build/ctest | 0/0/0；3/3 tests pass | `logs/native-*.log` |
| `cargo clean` + `cargo test` | 0；145 passed（90+40+15），0 failed | `logs/cargo-clean-final.*`、`cargo-test-final.*` |
| 39 帧默认参数 e2e | **80**（REVIEW_REQUIRED，39/39 review，0 条边） | `e2e/default/e2e-default.*`、`exposure_report.json`、`exposure-curve.csv` |
| 默认参数重跑（报告确定性） | 80；报告归一化后相同 | `e2e/default/e2e-default-rerun.*`、`report-determinism.txt` |
| 39 帧 passthrough e2e（补充） | 0；PARTIAL；审计 SHORT_CONTEXT；roundtrip PASS | `e2e/passthrough/*` |
| 39 帧 `--strength 0` | 0；PNG 39/39 一致；framemd5 39/39 一致；MKV 容器 43 字节差异 | `e2e/regression/*` |
| 集成测试（合成 DNG + 真实 ffmpeg） | 15/15 pass | `logs/cargo-test-final.stdout` |

集成测试覆盖：默认 pairwise 纠正交替增益且审计 PASS；确认台阶（估计侧保留、共享 offset 渲染检查判 `TARGET_NOT_REACHED`）；`--ev-limits`→LIMITED 且 requested≠applied；探针路径正确而渲染欠执行→82；`on_review` error→80 / passthrough→0 EV 标记；错误修正过探针但审计 strict FAIL→83 且不发布；`--strength 0` 单遍回归（PNG fixture 逐字节、FFV1 内容与编码参数一致）。

### NOT_TESTED / UNKNOWN / 边界

- **NOT_TESTED**：Windows/Linux；真实素材上的成功发布路径（该素材按 legacy 语义就是 REVIEW_REQUIRED，passthrough 仅补充后半程）；真实素材 `--step`/`--roi`/`--exclude-roi`/`--ev-limits` 组合；审计 JSON 的 `video_sha256`（纯 std 无哈希实现，记录为 `NOT_COMPUTED_STD_ONLY`，内容身份由无损往返证明）；多 ROI/chroma variation（legacy 明示 NOT_IMPLEMENTED）。
- **UNKNOWN**：跨 provider（legacy rawpy/linear-sRGB ↔ ACEScg）数值等同；审计面积降采样与 legacy float32 的逐位一致（声明为语义等价、非逐位）；ffmpeg/Matroska 容器字节确定性的稳定方案。
- **不声称**：与 legacy 产生域数值等同；MKV 容器字节可复现；审计 PASS 等价于“物理光照未变化”；`--strength 0` 的载荷字节由本次证据覆盖，不代表其他 ffmpeg 版本。
- `native/dng-linear-frame/README.md`：repair 1 经 Main 授权仅新增一段交叉引用（分析遍/探针使用 `--exposure-ev/--emit-analysis/--no-png`、应用遍使用 `--exposure-ev/--emit-analysis`，编排见 `pipeline/README.md`）；未改动任何 native 代码/测试/CMake。
