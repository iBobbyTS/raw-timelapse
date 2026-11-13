# clog3 (rev-eng crm-dev)

实现日期：2026-11-12。在已有统一 signed FP32 架构中增加独立 Canon CR3 Log 选项。

| 显示名 | CLI ID | 相机颜色方法 |
| --- | --- | --- |
| `clog2(white paper)` | `canon-log2-cinema-gamut` | legacy inverse(PCStoCamera) |
| `clog2(rev-eng crm-dev)` | `canon-log2-cinema-gamut-rev-eng-crm-dev` | DNG SDK CameraToPCS + profile HueSatMap |
| `clog3 (rev-eng crm-dev)` | `canon-log3-cinema-gamut-rev-eng-crm-dev` | DNG SDK CameraToPCS + profile HueSatMap |

默认仍为 `srgb-preview`。既有两个 Log2 ID、矩阵和曲线不变。新 Log3 和逆向 Log2
共享 Canon CR3 / 同相机型号限制、ForwardMatrix 优先策略、signed FP32 交接和曝光合同。
产品不加载 Canon 私有库；逆向库仅用于独立验证。

## 构建和使用

工作目录 `/Users/ibobby/Projects/raw-timelapse`。先按 [native 文档](../native/dng-linear-frame/README.md)
准备 Adobe DNG Converter 和授权 DNG SDK；本机已有可用 native 构建。

```sh
cmake -S native/dng-linear-frame -B native/build \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=/opt/homebrew
cmake --build native/build -j4
cargo build --release --manifest-path pipeline/Cargo.toml

pipeline/target/release/rawtl-linear-video \
  --input /path/to/canon-cr3-sequence --output /path/to/empty-output \
  --preview-profile canon-log3-cinema-gamut-rev-eng-crm-dev \
  --white-balance first-frame --log-reference-gain 1
```

输入是一种扩展名的同型号 Canon CR3 序列，输出目录为空或不存在。`--white-balance per-frame`
选择逐张拍摄白平衡。`--strength 0` 可关闭去闪、隔离颜色流程。其他参数保持既有含义。
最终文件 `output.mp4`；manifest、曝光报告、去闪/视频审计日志按现有布局输出。

`--log-reference-gain` 对两个 rev-eng Canon 预设可用；正有限数，默认1，当前记录为
`unverified`，`calibration_id=null`。该标量应用一次，不代表已完成灰卡标定。
不要把 DNG 白电平归一化自动理解为实际18%灰卡输出0.18。

## 颜色和精度合同

```text
CR3 -> Adobe Converter -c -l -> camera-space LinearRaw DNG
-> DNG SDK FP32 Linearize / Stage3 camera RGB
-> CameraToPCS（ForwardMatrix 优先；无 FM 时用 SDK 回退）
-> linear ProPhoto/D50 -> 配套 profile HueSatMap -> XYZ/D50
-> Bradford D50/D60 -> AP1/D60
-> ×2^EV（native 用户/去闪曝光，只应用一次）
-> RAWTLST2 signed FP32 AP1/D60 -> Cinema Gamut/D65
-> ×reference_gain（一次）-> 连续 Canon Log3
-> 最终定义域裁切 / RGB8 -> FFmpeg RGB24 pipe
-> H.264 VideoToolbox 8bit 4:2:0，宽度上限4096，output.mp4
```

CameraToPCS 已处理相机 neutral/白点，不重复 WB 或 D50 适配。配套 HueSatMap 在 linear ProPhoto/D50 中应用，先于用户 EV；不应用 LookTable、ProfileToneCurve、BaselineExposure。HDR 与负值策略及实测见 [profile 校正说明](canon-profile-calibration.md)。FP32 保留 AP1 负值和高于旧约4上限的值。
native 元数据、descriptor、像素 hash、finite、几何、颜色策略和源 hash 均校验。
所有 analysis/probe/application/final pass 使用相同相机矩阵。

Cinema Gamut 使用 D65，原色 R(.74,.27)、G(.17,1.14)、B(.08,-.10)。沿用原色矩阵
和 Bradford 适配组成的列向量变换 `out=M*in`，AP1/D60 到 Cinema Gamut/D65：

```text
0.9026473478762027  0.0149210086784204  0.0824316434453768
0.0380101539120561  0.7922310883021482  0.1697587577857960
0.0092428630699484  0.1414464646267953  0.8493106723032565
```

AP1 `[-.005,.5,.05]` 的最终 Log3 RGB8 为 `[36,114,74]`；提前裁 AP1 红通道会变成
`[38,114,74]`，因此即使目标色域值全为正，也应保留相机矩阵输出的有符号值。

RAWTLST2/analysis schema2、2.4GB帧上限及格式细节见 [通用 Canon FP32 合同](canon-log2-rev-eng.md)。
协议格式和 DNG SDK 不变。补全 profile 校正后必须重建匹配的 native/Rust：Rust 要求相机算法 revision2，拒绝跳过现有 HueSatMap 的旧 native。

## 恢复的正反曲线

目标：CRD 2.15.10.1 / CrmSdk 2.11.0.27 arm64，函数
`__calculateCanonLog3Gamma(int,unsigned short*)` @`0x40ca8`。
原库 SHA256 `3fa4ae8e5d8e8a7ffc2aa93b6c952e5917cae99a80781e8bb90280ee21228a71`。
静态证据和独立实现位于 `/Users/ibobby/Projects/rev-crm-2`：

- `analysis/sdk/disasm-arm64.txt`：原指令与常数引用。
- `scripts/canon_log3.py`：不依赖 Canon 库的离散 LUT/连续正反函数。
- `scripts/verify_log3_sdk.py`：固定 SDK 身份，动态逐项比较。
- `analysis/app/log3-verification.json`：常数 bytes、9组原 SDK 表 hash、差分结果。
- `analysis/log3/findings.md`：完整地址、运行时表选择、首帧基准与复现。

令 r 为 reference gain 后的 Cinema Gamut 线性反射率，`x=r/0.9`：

```text
if x < -0.014:
    L = ln(1-14.98325*x)*(-0.42889912)/ln(10) + 0.07623209
else if x < 0.014:
    L = x*2.3069815 + 0.073059361
else:
    L = ln(1+14.98325*x)*0.42889912/ln(10) + 0.069886632
y = (L*56064+4096)/65472
```

内部 SDK LUT 输入为 `x=2*exp2(p/12)*(i-512)/15872`，输出是
`clamp(trunc(L*56064+4096+0.5),0,65535)`。照片使用上述连续形式，**不移植**
CRM 的 p=36、512 pedestal、15872，也不移植 CRM 逐帧 sensor 矩阵。
65472=1023×64 是码值归一化依据；不要除65535或额外做 limited/full 拉伸。

反函数用于确定裁切定义域和视频审计，由上述正函数解析推导；没有声称恢复 SDK Log3 反 LUT。

```text
l = (y*65472-4096)/56064
t0 = 0.073059361 - 2.3069815*0.014
t1 = 0.073059361 + 2.3069815*0.014
if l < t0:
    x = min(-0.014, -expm1((0.07623209-l)*ln(10)/0.42889912)/14.98325)
else if l < t1:
    x = (l-0.073059361)/2.3069815
else:
    x = max(0.014, expm1((l-0.069886632)*ln(10)/0.42889912)/14.98325)
r = 0.9*x
```

原 SDK 常数造成两处约1.68e-10的 L 向下跳变，未做平滑；重叠信号区优先中央段，
反射率往返歧义小于6.6e-11。x=±.014 对应 r=±.0126。signal0..1 对应
`r=[-0.07381334162475732,14.66830073241655]`；每通道在此定义域裁切，最终
`round(clamp(y,0,1)*255)`，无抖动。不能复用 Log2 的端点。

| r | 0 | .18 | .9 | 7.2 | 14.4 |
| --- | --- | --- | --- | --- | --- |
| 10bit参考码 | 128 | 351 | 577 | 908 | 1020 |
| RGB8 | 32 | 88 | 144 | 226 | 254 |

## 输出、去闪与审计

manifest schema2 / color pipeline_revision3；记录独立 profile ID、显示名、曲线、来源、
定义域、reference gain 和每帧 `final_signal_clipping`。编码采用 pc/full、BT.2020 NCL
YCbCr 系数；MP4 nclx 与 H.264 SPS VUI 为 primaries2 / transfer2 / matrix9 / full1。
这些标签保留 vendor gamut/transfer 未指定，不把 Cinema Gamut 声称为 BT.2020。

视频审计从 VIDEO_ARTIFACT 的 profile 身份选择 Log3 反函数，恢复 Cinema Gamut 线性值，
除 reference gain 一次后求亮度。去闪测量仍使用 FP32 AP1 的既有算法和分析阈值，
`analysis_highlight_fraction` 不等于最终 Log3 裁切。时序质量审核仍为 report/off。

最终交付保持 RGB8 / 有损 H.264 4:2:0，非高位深调色母版。普通播放器不会自动正确显示
Canon Log；在调色工具中指定 **Canon Cinema Gamut / Canon Log3**。

## 验证与复现

```sh
# 不需要安装/加载 Canon 私有库：Rust 从原 SDK 表 hash 复核全部589824个码值
cargo test --manifest-path pipeline/Cargo.toml --test canon_log3 -- --test-threads=1
# 原有测试及新增选项的合同、WB、去闪、真实编码/审计
cargo test --manifest-path pipeline/Cargo.toml -- --test-threads=1
ctest --test-dir native/build --output-on-failure
```

原 SDK oracle 文件 `pipeline/tests/fixtures/canon-log3-sdk-oracle.json` 保存静态版本信息及
九行 LUT hash，测试通过**生产 Transfer::forward**重建量化表，不调用测试专用曲线。
复核/更新此夹具时先在 rev-crm-2 执行 `python3 scripts/verify_log3_sdk.py`，确认
`all_exact=true`，再复制其 `sdk_sha256/arm64_sha256/function_address/cases/reference_codes/legal_reflection_domain`
字段，禁止用 Rust 当前输出更新期望值。

原 SDK 同一 CRM 首帧的最终 Log3 p=36 表另有65,536项零差异。SDK 在本路径中保持
`userDef+0x238` 为 Log2 中间表，`+0x248` 才是 Log3 输出表。已保存6960×3672
RGB16LE及TIFF，回读 SHA256 `8e078465ef4605565d4e8d840d5b0185b2e4a5dffa6994d62a1b9ca612102e16`。

最初 matrix-only revision2 的实拍与回归记录保留在 [历史验证记录](canon-log3-validation.json)；当前 revision3 的 profile 校正与实测见 [校正记录](canon-profile-calibration.md)。
单帧实拍验证关闭去闪；28帧受控序列另覆盖两种 WB、reference gain=2、所有趟次和
真实视频审计。色卡ΔE、官方 Canon CR3 显影匹配和灰卡标定尚未进行；精确恢复传输函数
不等于整个 CR3 官方显影管线逐像素一致。
