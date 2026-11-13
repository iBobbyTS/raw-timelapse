# Canon CR3 profile 校正与欠饱和修复

日期：2026-11-12。基线 `6df57ff85c89e4f5275e01ebf6ad3f81800a8591`。
适用 `clog2(rev-eng crm-dev)`、`clog3 (rev-eng crm-dev)`；白皮书 Log2 和其他预设保留原相机颜色方法。

## 问题与因果证据

用户报告，同场景 Canon EOS R7 CR3 的项目输出比机内 CLog3 和 Resolve 直接解码到
Canon Cinema Gamut / Canon Log3 明显欠饱和。Resolve 使用 As shot、Exposure=0、无节点转换。
曝光基准差异与本次修复分开；原导出已经使用 `--strength 0`，不是去闪造成。

R7 的 Adobe LinearRaw DNG 包含 Adobe Standard profile、ForwardMatrix1/2，以及
90×30×1、linear 编码的 HueSatMap。基线只用 CameraToPCS/ForwardMatrix，显式跳过
HueSatMap，把不完整的 profile 表征当成最终场景颜色。绕过编码器直接比较 FP32 数据，
已经能复现欠饱和。保持输入、WB、矩阵、曝光和 Log 函数不变，只加入插值后的 profile
HueSatMap，主要差异消失。这是本次修复的因果对照。

SDK 自身的顺序可在以下本地授权源码中核查：

- `native/vendor/dng-sdk-1.7.1/dng_sdk/source/dng_render.cpp`：`fCameraToRGB` 使用
  linear ProPhoto `MatrixFromPCS()*CameraToPCS()`，随后在曝光/LookTable/tone 之前应用
  `profile.HueSatMapForWhite(spec->WhiteXY())`。
- `dng_camera_profile.cpp`：所选白点下的 profile 表插值。
- `dng_reference.cpp::RefBaselineHueSatMap`：HSV 插值与范围行为。
- `dng_color_space.cpp::dng_space_ProPhoto`：D50 矩阵及白点归一化。

不调用整个 `dng_render`，不加入 LookTable、ProfileToneCurve 或 BaselineExposure。
本次补全的是 Adobe profile 校正，不是恢复了 Canon 官方 CR3 显影器，也不是人为增加固定饱和度。

## 当前算法与边界

```text
CR3 -> Adobe DNG Converter -c -l
-> SDK FP32 Linearize / Stage3 camera RGB
-> CameraToPCS（FM 优先，SDK no-FM 回退；包含所选 neutral）
-> XYZ D50 -> linear ProPhoto D50
-> 按同一所选白点插值的 profile HueSatMap（存在时）
-> XYZ D50 -> Bradford D60 -> AP1 FP32
-> 用户/去闪 ×2^EV -> FP32 pipe
-> Cinema Gamut D65 -> reference gain -> 原 Log2/3 函数 -> RGB8 / H.264
```

`Decoded.camera` 保留校正前的 camera RGB，供传感器 WhiteLevel 饱和掩码使用；
`calibrated_scene` 保存未加 EV 的 AP1。analysis 和最终像素均消费这份校正后的场景数据。
白平衡只由已有 first-frame/per-frame 合同决定，校正中不重复乘 WB。

HDR 与负值处理必须显式：

- 单层、linear 编码的 HSM 仅依赖 H/S。SDK kernel 本身会裁切 HSV 的 V 到1；
  因此先按每像素最大值及表的最大 ValueScale 安全归一化，再恢复比例。这一扩展保持
  曝光齐次性及 >1 高光；不是把高光裁到1后再扩回。
- 其他表形状/编码使用 SDK 编码表及 `supportOverrange` 行为，不能声称它们也是曝光齐次的。
- HSV 对负 ProPhoto 分量没有本项目定义的校正意义；这些像素绕过 HSV，保留完整
  signed CameraToPCS→AP1 矩阵结果。逐帧记录 bypass 数量；这两张实拍为0。
- 保留校正后 AP1 的负值和高值。相机 Stage1/Linearize 的既有 [0,1] 范围约束不变。

`color.algorithm_revision=2`，记录 `hue_sat_map_present/applied` 和
`profile_correction`（空间、维度、编码、HDR/负值策略、bypass 数量）。
`camera_to_xyz_d50/camera_to_acescg` 只描述 HSM 前的矩阵，不能代表完整非线性校正。
Rust 对逆向预设要求算法 revision2 且 present/applied 一致，拒绝旧 native 静默跳过校正。
协议 `RAWTLST2`、descriptor color_contract_version1、manifest schema2 不变；
逆向预设 `video.color_contract.pipeline_revision=3`，其他预设仍为2。

## 白电平排查

直接用 LibRaw 0.21.4 unpack 原 CR3，再用 DNG SDK 读取 Adobe LinearRaw Stage1，
比较相同 active area 的绿色通道区块均值。原片是14bit，Adobe 确实将像素扩大到16bit：

| 原片 | CR3 黑电平 | DNG 黑电平 | 原有效上限 / DNG WhiteLevel | 去黑后像素缩放中位数 |
| --- | --- | --- | --- | --- |
| R7_L2219 | 2047 | 8188 | 16383 / 65535 | 3.99781 |
| R7_L2220 | 约511–512 | 2048 | 16383 / 65535 | 3.99947 |

ADC 去马赛克、整数取整及各通道黑位使像素比例不必逐样本严格等于4。
SDK FP32 与手算 `(DNG_sample-BlackLevel)/(WhiteLevel-BlackLevel)` 的全通道最大误差
分别为 2.98e-8、2.98e-8。没有把未缩放的14bit值错误除以65535。
此检查不等于完成相机曝光/反射率标定；不因此自动增加 ISO 比例或3 EV。

## 真实参考比较

输入：`/Users/ibobby/Temp/tl/pair/test/R7_L2219.CR3`（ISO800、1/8、f/4），
`R7_L2220.CR3`（ISO100、1/60、f/4）。逐张 As shot，EV0，reference gain1。
Resolve 参考：`/Users/ibobby/Temp/tl/pair/davinci-reference/clog{2,3}_{2219,2220}.tif`。
这些 TIFF 的 ICC 标为 Rec.709，但本比较按用户确认的 Cinema Gamut / Log 数值直接读取，
不应用 ICC。Log2/Log3 TIFF 各自反解后相互一致，支持这一数值解释。

将 native 6984×4660 的有效图像裁到 `[x=12,y=12,w=6960,h=4640]`，与 TIFF 对齐，
面积降采样为870×580。参考反解到 Cinema Gamut 线性值。基线生成的固定掩码排除
近黑、强高光及边缘纹理；两张分别保留353657、402754个采样像素。
色度定义 `c=RGB/mean(RGB)-1`；幅度为欧氏范数。幅度比只统计参考范数>0.12的像素。
这是排除统一曝光标量影响的诊断指标，不是感知饱和度、色卡 ΔE 或产品饱和度参数。

| 原片 | 修复前幅度 / Resolve | 修复后幅度 / Resolve | 色度 RMSE 前 → 后 |
| --- | --- | --- | --- |
| 2219 | 0.62760 | 1.09078 | 0.11865 → 0.03569 |
| 2220 | 0.56251 | 1.04261 | 0.13963 → 0.02777 |

机内视频交叉检查：`/Users/ibobby/Temp/tl/pair/ref video/` 的两段原视频第一帧，按其
full-range/BT.709 YCbCr 标签解码，SIFT+RANSAC 对齐照片；分别1318/1668个匹配内点。
隔离 HSM 实验的色度 RMSE 从0.14330/0.13731降为0.04549/0.05357。视频与照片的曝光、
视角、WB及采集时间并不完全相同，这一比较只支持校正方向，不支持逐像素匹配结论。

修复后主要欠饱和消除，仍有约4%–9%的上述幅度比偏差及局部色相/亮度差异。
不使用这两张照片拟合或硬编码新色彩矩阵，不声称与 Resolve/Canon 官方显影完全一致。
曝光标量、色卡精度、其他机型及其他 illuminant/profile 尚未完成实测标定。

## 构建、复现和检查

在项目根目录：

```sh
cmake --build native/build -j4
cargo build --release --manifest-path pipeline/Cargo.toml
ctest --test-dir native/build --output-on-failure
cargo test --manifest-path pipeline/Cargo.toml -- --test-threads=1

pipeline/target/release/rawtl-linear-video \
  --input /Users/ibobby/Temp/tl/pair/test --output /path/to/empty-output \
  --preview-profile canon-log3-cinema-gamut-rev-eng-crm-dev \
  --white-balance per-frame --strength 0 --log-reference-gain 1
```

新增 native 合成 DNG 测试包含非恒等 HSM 的独立 HSV 期望值、校正后曝光只应用一次、
HDR 齐次性、负 ProPhoto 保留、legacy 控制组。Rust 故障注入验证算法 revision1 和
遗漏现有 HSM 均被拒绝；两个 Canon 逆向选项共享该门禁。

完整数值/文件身份见 [验证 JSON](canon-profile-calibration-validation.json)。
研究脚本和原始观测位于 `/Users/ibobby/Projects/rev-crm-2/analysis/brightness-check/`、
`analysis/saturation-check/`：`white_probe.cpp`、`capture.py`、`compare.py`、`hsm_probe.cpp`、
`video_control.py`、`post-comparison.json`。Python 比较使用本机 miniconda 的 NumPy/OpenCV；
原始 SDK-only 对照与生产修复分别保存，避免用修复输出生成基线。

修复导出：`/Users/ibobby/Temp/tl/pair/test-clog3-profile-corrected/output.mp4`，
两帧、30fps、4096×2734、H.264 8bit 4:2:0；完整解码通过。两帧各自 WB
`[0.793184,1,0.407157]`、`[0.61244,1,0.583144]`，保留1101/76个负 AP1 样本。
没有额外补曝光、统一 WB 或启用去闪。
