# Canon CR3 双 Canon Log 2 路线

当前为固定标定 revision4（native revision3）：原CR3 NormalWhite → 固定scene倍率1.8 → 反射率×0.9。详见[固定标定合同](canon-fixed-calibration.md)。旧验证记录保留历史含义。


实现及统一 FP32 更新日期：2026-11-12。两个独立预设沿用同一内存流式输出架构，最终产物是 `output.mp4`。

| 显示名 | `--preview-profile` ID | 相机矩阵 / native 帧交接 |
| --- | --- | --- |
| `clog2(white paper)` | `canon-log2-cinema-gamut` | 历史 inverse(PCStoCamera) / RAWTLST2 signed FP32 AP1/D60 |
| `clog2(rev-eng crm-dev)` | `canon-log2-cinema-gamut-rev-eng-crm-dev` | DNG SDK CameraToPCS + profile HueSatMap / RAWTLST2 signed FP32 AP1/D60 |

另有独立的 [clog3 (rev-eng crm-dev)](canon-log3-rev-eng.md)，复用相同 CameraToPCS/FP32 路线及 reference gain 合同，使用独立的三段式 Log3 正反曲线。

默认仍为 `srgb-preview`。旧 ID、白皮书 Log2 函数及旧矩阵保持；所有 RAW profile（包含 sRGB 和其他厂商 Log）统一 FP32，不再保留中间 RGB48 量化，因此不保证与旧版本逐像素一致。新预设只接受 Canon CR3 序列，各帧必须为同一相机型号。每帧在像素交给编码器前校验颜色合同；混合型号或失败时不发布最终文件。`first-frame` 和 `per-frame` 继续决定相机 neutral 的来源。

## 构建和运行

在项目根目录构建匹配的 native/Rust 二进制：

```sh
cmake -S native/dng-linear-frame -B native/build \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=/opt/homebrew
cmake --build native/build -j4
cargo build --release --manifest-path pipeline/Cargo.toml

pipeline/target/release/rawtl-linear-video --input /path/to/canon-cr3 \
  --output /path/to/white-paper --preview-profile canon-log2-cinema-gamut

pipeline/target/release/rawtl-linear-video --input /path/to/canon-cr3 \
  --output /path/to/rev-eng \
  --preview-profile canon-log2-cinema-gamut-rev-eng-crm-dev \
  --log-reference-gain 1 --white-balance first-frame
```

输出目录必须为空或尚不存在。Adobe DNG Converter 与授权 DNG SDK 的准备见 [native 文档](../native/dng-linear-frame/README.md)。`--strength 0` 可隔离去闪影响，仅执行最终单趟编码。

`--log-reference-gain` 只允许搭配 rev-eng Canon Log2 或 Log3 预设；必须是正有限标量，默认 1。当前没有灰卡校准数据，额外用户倍率记录为固定标定后的 reference gain，`calibration_id=canon-normal-white-fixed-v1`。不能把 SDK 白电平归一化理解为实拍灰卡天然得到 0.18。校准需要在基础 EV=0、固定照明/机型/ISO/读出模式/profile 下测中性灰 ROI；若测得线性 Y 为 m，可评估 `0.18/m`。不使用每通道增益替代该曝光标量。

## 共用空间和曝光合同

```text
CR3 -> Adobe Converter -c -l -> camera-space LinearRaw DNG
-> SDK FP32 Linearize / Stage3 camera RGB
-> 所选相机矩阵（white paper: inverse(PCStoCamera); rev-eng: CameraToPCS）
-> rev-eng: linear ProPhoto/D50 + profile HueSatMap -> XYZ D50
-> Bradford D50/D60 -> AP1 D60
-> ×2^EV（用户/去闪曝光，一次）
-> RAWTLST2 signed FP32 AP1/D60 -> AP1/D60 到 Cinema Gamut/D65
-> ×reference_gain（仅rev-eng，一次；其他选项为1）
-> 所选连续 Log2 -> 最终码域裁切/RGB8
-> H264 8bit 4:2:0，沿用宽度上限4096及比例缩放
```

CameraToPCS 优先使用 ForwardMatrix；无 FM 时由 SDK 提供逆矩阵回退。它已经处理相机 neutral / 白点，不再额外乘 WB 或重复适配到 D50。逆向路线将 CameraToPCS 与配套 profile HueSatMap 一起应用（在 linear ProPhoto/D50 中，先于用户 EV）；LookTable、ProfileToneCurve、BaselineExposure 不应用。单层线性 HSM 采用保持曝光比例的 HDR 扩展，负 ProPhoto 像素保留有符号矩阵结果。详见 [饱和度修复与实测](canon-profile-calibration.md)。各帧 metadata/manifest 记录方法、实际矩阵、selected white、profile 指纹、FM/表存在与应用状态。

内部 AP1/D60 浮点空间本身不会天然丢色。全部路线跳过旧 `round(AP1*16384)` 的提前负值裁切和约4的上限，直到最终 Log 码域才裁切。ADC 和 SDK 上游的既有范围限制仍然存在。

矩阵采用列向量 `out=M*in`。Cinema Gamut 白点 D65 `(0.3127,0.3290)`，原色 R `(0.74,0.27)`、G `(0.17,1.14)`、B `(0.08,-0.10)`。AP1 到 Cinema Gamut 参考矩阵为：

```text
0.9026473478762027  0.0149210086784204  0.0824316434453768
0.0380101539120561  0.7922310883021482  0.1697587577857960
0.0092428630699484  0.1414464646267953  0.8493106723032565
```

产品沿用由原色/Bradford 中间矩阵组成的现有数值，尾数误差约7e-11。负 AP1 `[-.005,.5,.05]` 变换后 Cinema Gamut 全为正，10bit Log2 参考码约 `[151,491,361]`；先裁 AP1 红通道会变成 `[176,491,361]`。

## 连续曲线和逆向来源

静态目标：Cinema RAW Development 2.15.10.1 所含 CrmSdk 2.11.0.27 arm64。恢复入口 `__calculateCanonLog2Gamma` 地址 `0x40a98`、inverse `0x40b9c`。原内部 LUT 为：

```text
x_sdk = 2 * 2^(p/12) * (i-512)/15872
L = sign(x_sdk)*0.122411586*ln(1+87.09937546*abs(x_sdk)) + 0.035388128
q_sdk = clamp(trunc(L*56064+4096+0.5),0,65535)
```

新照片预设用浮点参考反射率 r，明确采用 `x=r/0.9`。把舍入前 SDK 信号除 `65472=1023*64` 后得到：

```text
L = sign(x)*0.122411586*ln(1+87.09937546*abs(x)) + 0.035388128
y = (L*56064+4096)/65472

z = (y*65472-4096)/56064 - 0.035388128
r = 0.9*sign(z)*expm1(abs(z)/0.122411586)/87.09937546
```

实现使用 `ln_1p` / `exp_m1`，并有独立正反式测试。最终 RGB8 为 `round(clamp(y,0,1)*255)`。65472 是 SDK 归一化来源，不能拿它解码 0..65535 的普通 RGB16，也不再做一次 limited/full 扩展。CRM 的 p=36、512 pedestal、15872 和逐帧 sensor 矩阵都不移植到 CR3。

灰阶 r=0/.18/.9/7.2/14.4 的10bit参考码为95/407/575/797/871。归一化曲线与白皮书近乎等价，测试网格最大连续差约7.69e-9。两个 Log2 选项现在共用浮点边界，主要区别是矩阵策略与曲线来源，不宣称微小曲线常数变化产生可见提升。

逆向工具参考 [rea](https://github.com/morluto/rea)。本机完整证据：`/Users/ibobby/Projects/rev-crm-2/analysis/sdk/findings.md`、`scripts/canon_log2.py`、`scripts/verify_log2_sdk.py`。原 LUT 的9行599040个样本曾与原 SDK 逐码相等；验证工具不是产品运行依赖。公开参考为 [Canon Log 白皮书](https://www.usa.canon.com/content/dam/canon-assets/white-papers/pro/white-paper-canon-log-gamma-curves.pdf)、[Cinema Gamut 原色](https://gdlp01.c-wss.com/gds/8/0200003438/01/colorgamutinfo-dim1102.pdf)。DNG CameraToPCS 来自 `native/vendor/dng-sdk-1.7.1/dng_sdk/source/dng_color_spec.cpp`，并非从 CRM 推导出的 CR3 相机校准。

## 传输、去闪和审核

新 native 接口（模式必须是第一个参数）：

```sh
native/build/dng-linear-frame --stdout-frame-f32 /path/to/input.CR3 \
  --camera-color-method dng-camera-to-pcs --exposure-ev 0 > /tmp/frame.rawtlst2
```

该文件是有头部的协议，不是裸 RGB 浮点文件。magic 为 `RAWTLST2`，12字节记录头仍是 `type:u8 | reserved:3*0 | length:u64LE`，顺序为 metadata 0x01、descriptor 0x03、pixels 0x04、EOF和成功退出。descriptor 声明 AP1/D60、linear、rgbf32le、合同版本1、几何/字节数/SHA256。像素为紧密 HWC RGB、LE binary32，每像素12字节，无 padding。R7 6984×4660 为390545280字节。

所有 profile 的完整帧统一 RAWTLST2；analysis 仍用 RAWTLST1 封装小 JSON。Rust 对全部路线校验实际颜色方法、3×3有限矩阵、profile 指纹、source hash、几何/长度/分配上限、像素 hash/finite、严格记录顺序、EOF及子进程退出；Canon 型号/同型号限制仅属于rev-eng。格式不再由 `is_rev_eng()` 决定，不回退RGB48。共用 `rawtl.ap1-f32-frame@1` F32 HWC/LE；最后转换为RGB24/U8交给FFmpeg。原native RGB48和位置参数接口作为显式兼容工具保留，不参与默认RAW流水线。每帧处理后释放像素，不积累完整序列。

所有 analysis/probe/application/final pass 使用同一策略。analysis 仍是小 JSON/v1，无完整像素。VIDEO_ARTIFACT 保存独立 profile ID、显示名和 reference gain。真实 output.mp4 审核先用对应反曲线恢复 Cinema Gamut 线性值，再除 reference gain 和 scene→reflection 的0.9各一次、用其 XYZ 的 Y 行求亮度。标签不能代替独立 profile 身份；身份/标签冲突报告不可观测或错误。时序审核继续 report/off、失败不阻止发布；字节损坏、native/encoder/交付完整性失败不发布。

manifest schema仍为2；逆向路线的颜色 pipeline_revision 为3，其他路线为2。`final_signal_clipping` 对全部profile记录最终transfer定义域上下限裁切，native负值/高值仅是观测而非裁切。

analysis JSON schema升至2：移除 `quantum_linear` 和 `output_clip_code_threshold`，改用 `highlight_threshold_linear=0.980010986328125`；这是保留历史行为的分析阈值，不是FP32量化步长。`floor=0.00048828125` 保持为测光算法参数。逐tile字段改为 `analysis_highlight_fraction`，`highlight_quality=analysis_ap1_threshold`；报告和CSV对应改名为 `analysis_highlight_increase_fraction`。报告schema也为2。相机域 `saturation_fraction` 不变。分析阈值占比不等于最终视频裁切，视频损失与传感器饱和也不能由最终Log裁切计数推导。native和Rust必须配套构建；旧schema分析会被拒绝。

## 可重复检查和限制

```sh
ctest --test-dir native/build --output-on-failure
cargo test --manifest-path pipeline/Cargo.toml -- --test-threads=1
```

native 合成 DNG 测试覆盖 FM 有/无、实际矩阵乘法、EV一次、FP32负值/高值、LE/hash、analysis矩阵一致与v1兼容。Rust 测试覆盖两预设身份、独立连续正反式、灰阶/负值反例、类型合同、错误协议/方法/型号/hash/NaN/Inf拒绝、固定与逐帧WB所有趟次、真实H264交付与审核。

macOS 沙箱可能阻止 Adobe Converter 或 VideoToolbox 创建会话；在普通用户终端运行相同命令，不改像素算法或降级编码器。实际验证结果另见 [实施验证记录](canon-log2-validation.md)。

尚无标准色卡、Canon 官方照片线性输出和参考灰标定的对照数据。新选项是可独立评估的矩阵/传输预设，不能称完整 Canon CR3 官方显影匹配或已证明较小 ΔE。最终仍是8bit有损交付，非高位深调色母版。
