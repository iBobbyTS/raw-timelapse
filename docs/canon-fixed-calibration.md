# Canon CR3 固定曝光标定

本修复以51a7807的profile HueSatMap路线为基础，适用于两个rev-eng Canon Log选项。白皮书和其他厂商选项保持原行为。native camera algorithm revision=3，Rust color pipeline revision=4，必须配套构建。

## 数值合同

原CR3由LibRaw读取元数据（仅open_file，不使用LibRaw解码像素）：

- source maximum：ADC满量程，必须为2^n−1。
- black=max(LibRaw color.black, Canon.AverageBlackLevel)。
- white=Canon.NormalWhiteLevel，不用SpecularWhite，不做图像直方图统计。

图像仍由Adobe DNG Converter线性显影、DNG SDK归一化并执行现有CameraToPCS/HueSatMap。固定标定保留SDK的黑位处理和sensor clipping mask，不修改DNG WhiteLevel或重做WB。

令 DNG 白位为Dw、黑位Db，原ADC满量程M、选定黑位B、NormalWhite W：

```
scale = (Dw+1)/(M+1)
normal_white_gain = (Dw-Db)/(scale*(W-B))
native_scene_gain = 1.8 * normal_white_gain
native_pixel = profile_corrected_AP1 * native_scene_gain * 2^user_EV
reflection_CinemaGamut = AP1_to_CinemaGamut(native_pixel) * 0.9 * log_reference_gain
signal = existing_Log2_or_Log3(reflection_CinemaGamut)
```

Native保留signed FP32，校正先于用户EV且analysis/pixels共用同一次标定。现有Log函数系数、反射率定义和原SDK oracle保持不变；Rust的convert_f32_frame和encode_linear负责scene→reflection的.9，审计反解除以.9和用户reference_gain各一次。默认reference_gain=1现在只是额外用户倍率，不再承担默认标定。

不叠加DNG BaselineExposure，不加入逐帧自动归一化，不增加饱和度系数。

## 尺度检查与失败策略

当前固定标定接受经过验证的16bit LinearDNG、均匀黑白端点、无空间黑位delta：Dw=65535。必须满足source black/white有效、W>ADC满量程一半，并且Db与B*scale相差至多一个原RAW码（容纳R7 ISO100的511/512舍入差）。不匹配时明确失败，禁止套用猜测倍率。其他位深/转换器的不同像素尺度须新增证据和适配后再支持。

只在native dng-camera-to-pcs的原始vendor CR3输入上应用标定。原CR3元数据缺失、不支持或非Canon时失败。直接DNG开发探针仍可跑颜色算法，但applied=false，不能伪装成已经标定的CR3；Rust的rev-eng入口本来就限定原CR3，并严格检查标定合同、倍率及applied状态。

`color.exposure_calibration`记录来源、LibRaw版本、原始/转换端点、storage_scale、各层倍率、自动归一化false、BaselineExposure false。旧native、跳过标定、错误倍率、自动归一化合同均被Rust拒绝。

## 本机两帧验证

|CR3|源black/NormalWhite|DNG black/white|native scene gain|scene→reflection后净倍率|
|---|---|---|---:|---:|
|R7_L2219|2047/13135|8188/65535|2.3273944805|2.0946550325|
|R7_L2220|511/12535|2048/65535|2.3760104790|2.1384094311|

这比研究报告使用原14bit端点的简化式更精确：明确计入ADC的16bit端点和黑位舍入。并非写死两张照片的校正值，实际每帧都从元数据计算。

新ProRes与Resolve AutoTone关闭的实拍对照：线性亮度中位差−2.59%/−0.023%，即−0.03793/−0.00033EV；曝光对齐后的ΔE76中位3.05/1.08。色差指标沿用此前平滑区/相对Lab统计，不是色卡精度。ProRes相对native的亮度差仅+0.00013/+0.00074EV。

## 构建和复现

新增构建/运行依赖LibRaw（本机0.21.4；仅元数据读取）。现有SDK/JPEG/JXL等依赖不变：

```sh
brew install libraw
cmake -S native/dng-linear-frame -B native/build
cmake --build native/build -j 6
ctest --test-dir native/build --output-on-failure
cargo test --manifest-path pipeline/Cargo.toml
cargo build --release --manifest-path pipeline/Cargo.toml
pipeline/target/release/rawtl-linear-video --input /Users/ibobby/Temp/tl/pair/test \
 --output /tmp/rawtl-fixed-canon-check-new \
 --preview-profile canon-log3-cinema-gamut-rev-eng-crm-dev \
 --white-balance per-frame --strength 0 --fps 30
```

标准CLI仍发布H264 output.mp4。用户要求的ProRes4444是独立高位深对照导出，不将旧RGB8视频转码。复现脚本在研究目录 `analysis/cause-investigation/delivery/export_fixed_prores.py`：读取本次CLI manifest、再次从原CR3调用新native，要求两次FP32 payload hash完全一致；使用相同CinemaGamut矩阵/Log3曲线和.9单位转换，FP32 planar GBR经zscale到limited YUV444 10bit，prores_ks profile4。解码报告12bit，不表示源交接有效精度超过编码器接受的10bit。脚本默认临时输出目录必须不存在，防止覆盖。

证据来源和完整白位/单位/受控开关推导：`/Users/ibobby/Projects/rev-crm-2/analysis/cause-investigation/calibration-confirmed.md`。产品验证结果见canon-fixed-calibration-validation.json。无需读取历史对话即可按源文件和命令复现。
