# 双 Canon Log 2 实施验证

> 当前实现已统一所有 profile 为 FP32，最新结果见本文末尾“统一 FP32 验证”。下方原始双路线结果是迁移前基线，不代表当前 white paper 的数据格式。机器可读记录的 `fp32_migration` 保存最新证据。

日期：2026-11-12。直接在当前分支实施，起始 HEAD 为 `87df3aa56365f156bbba484f4ce4a7c5a0139d60`，未创建 worktree 或切换分支。

完整算法、输入尺度、协议和可复现命令见 [接入说明](canon-log2-rev-eng.md)。结构化结果、源文件哈希和最终构建哈希见 [验证数据](canon-log2-validation.json)。测试和临时实拍产物路径也记录在该 JSON。

## 自动检查

| 检查 | 结果 |
| --- | --- |
| Rust 库单元测试 | 142 通过 |
| Rust 进程/审核/视频模块单元测试 | 63 通过 |
| CLI 集成测试 | 38 通过 |
| Rust 文档编译测试 | 3 通过 |
| native CTest | 4 通过 |
| Release 构建、`git diff --check` | 通过 |

Rust 合计246个测试。完整测试之后修正了新预设 manifest 输入合同的描述，再通过142个库测试及真实CR3完整运行确认：新合同声明 signed FP32，不再误写 code/16384。该修正不改变像素算法。

新增测试涵盖两个精确名称/ID、独立 CRD 正反式、灰码、负 AP1 反例、参考增益一次、原 v1/RGB48 兼容、v2版本错配、记录/长度/hash/finite错误、缺源hash、实际颜色方法/相机型号校验、混合型号拒绝、有FM/无FM/奇异FM、固定及逐帧WB的全部去闪趟次和真实H264审核。旧预设既有 golden、全预设已编码色块/标签检查继续通过。

硬件视频测试在沙箱内遇到 VideoToolbox `cannot create compression session: -12908`。相同测试在沙箱外用原硬件编码参数通过；未切换编码器或引入软件降级。完整硬件测试使用 `--test-threads=1`。

## 真实 Canon CR3

只读源：`/Users/ibobby/Temp/tl/pair/test/R7_L2219.CR3`，Canon EOS R7，SHA256：

```text
26c4447b73d85c0f03ba6511bd8c5ea3dcb9b6685a9ef7a526864100847ae868
```

通过临时目录中的符号链接运行，原始照片未改。两预设均使用 `--strength 0`、同一 as-shot neutral `[0.793184,1,0.407157]`，隔离去闪影响。新预设 reference gain=1、状态unverified。

| 观测 | clog2(white paper) | clog2(rev-eng crm-dev) |
| --- | --- | --- |
| 实际相机矩阵 | legacy-inverse-cm | dng-camera-to-pcs-forward-matrix |
| native交接 | RGB48LE / RAWTLST1 | signed FP32 / RAWTLST2 |
| native源尺寸 | 6984×4660 | 6984×4660 |
| 观察到的AP1范围 | -0.01102362..2.98776746 | -0.00001945..2.34015608 |
| 负通道样本 | 70878，旧量化裁零 | 445，保留 |
| 最终Log域裁切 | 旧路线未新增统计 | 上、下限均0 |
| 交付 | 1帧，4096×2734，H264 | 1帧，4096×2734，H264 |
| ffprobe及完整解码 | 通过 | 通过 |

两个实际 camera→XYZ D50 和 camera→AP1 矩阵分别与独立SDK探针的对应路线比较，最大差均为0。两条路线的native像素hash不同，这是不同矩阵/不同数据合同的预期结果。新预设最终运行的FP32像素SHA256：

```text
114167e19f1dd7a8d446cf69a7b406ee75803e5a42c2d0bc5aba947ecc7e7881
```

产物：

- `/private/tmp/rawtl-canon-log2-validation-20261112/white-paper/output.mp4`
- `/private/tmp/rawtl-canon-log2-validation-20261112/rev-eng-final/output.mp4`

相邻 manifest 保存选项身份、实际矩阵、参考尺度、裁切统计和编码验证。new路径的完整像素payload为390545280字节，接收端SHA256核验通过；未生成PNG/FP32/TIFF序列。

## 证据边界

实拍验证为同一张R7照片，两预设各完成一帧视频交付。多帧去闪、增益探测、反馈和两种WB选取通过受控生产者及真实编码器集成测试覆盖；它们不构成完整实拍时序或色卡测量。

负通道数量差异首先反映两种相机矩阵的不同，不能直接换算为画质收益。尚未测量色卡ΔE、参考灰定标或Canon官方照片显影的匹配程度。H264仍为8bit 4:2:0有损交付，浮点交接改善不等于高位深母版交付。

## 统一 FP32 验证（2026-11-12）

基线提交 `5d8b152743fa022b08c8b2a1fed7b96202ea3338`，当前分支 `feature/streaming-encode-pipeline`。所有7个profile均走 signed FP32 AP1/D60 → 目标色域/曲线 → RGB8 → 原硬件编码器。完整 Rust 回归：143库 + 63主程序 + 38 CLI + 3文档 = 247通过；native 4组通过。全profile的86个协议/颜色合同破坏案例均拒绝发布；现有固定/逐帧WB、去闪probe/application/audit、异常取消/子进程回收测试通过。

重复命令：

```sh
cmake --build native/build -j4
ctest --test-dir native/build --output-on-failure
cargo test --manifest-path pipeline/Cargo.toml -- --test-threads=1
cargo build --release --manifest-path pipeline/Cargo.toml
pipeline/target/release/rawtl-linear-video --input /path/to/canon-cr3 \
  --output /path/to/empty-output --preview-profile canon-log2-cinema-gamut \
  --strength 0
```

同一只读R7原片（哈希见上文）分别运行两个Log2及sRGB，输出位于 `/private/tmp/rawtl-fp32-validation-20261112/{white-paper,rev-eng,srgb}`。三次均经实际ffprobe和完整解码验证为1帧4096×2734 H264。native每份帧390545280字节；输入格式均为RAWTLST2/rgbf32le。

| 观测 | white paper | rev-eng crm-dev | sRGB |
| --- | --- | --- | --- |
| 矩阵 | legacy-inverse-cm | SDK ForwardMatrix | legacy-inverse-cm |
| 负通道样本，全部保留到目标转换 | 70878 | 445 | 70878 |
| 最终transfer定义域裁切：低/高 | 0/0 | 0/0 | 294239/7815 |
| MP4字节数 | 151338 | 142489 | 261163 |

white paper和sRGB的native像素SHA256一致，为 `9bd5688b5faf231c551704bdb81948a0e3731f10ce88ba0343929ce6e3b28f06`。rev-eng仍为 `114167e19f1dd7a8d446cf69a7b406ee75803e5a42c2d0bc5aba947ecc7e7881`，与迁移前的FP32结果一致。旧white paper原先提前裁掉的负数现在保留，因此旧MP4不保证逐像素兼容。sRGB最终裁切属于其目标色域/显示范围，不是AP1提前裁切。

manifest、exposure report和analysis JSON schema为2，颜色pipeline_revision为2；上游metadata/颜色contract本身仍为1，传输记录版本和JSON schema独立。统计字段迁移详见算法文档，native/Rust须配套更新。三次真实样片均为strength=0，不声称验证了实拍时序稳定性、参考灰标定或色卡ΔE；这些限制与原基线一致。
