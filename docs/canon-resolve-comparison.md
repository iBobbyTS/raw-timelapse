# Canon R7 CR3 与 Resolve C-Log 2／3 对照

> 当前 Canon 输出 ID 为 `clog2` 和 `clog3`。输入路由已独立于输出：`--camera-color auto` 对原始 Canon CR3 选择 NormalWhite × 固定 1.8 标定，适用于所有 Log 和 sRGB 输出。下文保留当时的 C-Log 2/3 实测与变更记录。见 [CLI 使用说明](../pipeline/README.md)。

2026-11-17。源文件 R7_L2220.CR3；对照为用户本次导出的 `clog2_00086400.tif`、`clog3_00086400.tif`。原始文件保持只读。基线提交 `05f342b`；本次直接修改，无 SFD。

## 结论与“哪条正确”

没有发现本样本下项目 ForwardMatrix＋HSM 路径存在明显实现错误。它与 Resolve 采用不同的相机色彩表征，因此能产生不同结果。DNG 合规、接近 Resolve、接近真实物体颜色，是三个不同的评价目标；本次自然场景能强验证前两者之间的差别，不能给出第三者的色卡准确度结论。也不能据此声称两条路径具有相同的色彩准确度。

已确认的差异来源有三层：

1. **输入标定／单位约定**：旧 C-Log 2 与 C-Log 3 的自动配方及 scene→reflection 输入尺度不一致。本次已统一；这属于应修正的项目行为不一致。
2. **镜头处理条件**：项目默认应用 LCP 暗角与畸变校正，Resolve 这两份参考基本保留未校正几何与亮度。关闭项目镜头模块作为受控对照后，亮度误差大幅下降。镜头校正本身没有因此被判错，产品默认保留。
3. **相机颜色表征**：Adobe Standard 的 ForwardMatrix＋HSM 与 Resolve 保存的 R7 `rgb_cam`／目标矩阵不同。直接使用研究仓库保存的源矩阵预测新参考，几乎消除了剩余色差，且没有拟合本次参考的颜色。这是本轮最有区分力的证据。

## 方法与可比性

两份参考均为 6960×4640、RGB16 TIFF。虽然 RAW 面板指定 Cinema Gamut／Canon Log，文件附带的 ICC 却写 Rec.709。本次直接读取 RGB 码值÷65535，忽略 ICC，按各自 Canon full-code 曲线反解；不经过系统看图软件或显示截图取色。

项目从原始 CR3 经 Adobe DNG Converter 18.7、DNG SDK 1.7.1 显影，保留有符号 FP32 AP1。使用所选 Adobe Standard 配置的 ForwardMatrix 与固有 HSM；LookTable、ToneCurve、BaselineExposure 不应用。Native 原图 6984×4660，比较时按 DNG DefaultCropOrigin=(12,12)、DefaultCropSize=(6960,4640) 裁切。

先逐像素反 Log，再做 4×4 线性平均。每个候选独立做仿射几何配准，不拟合亮度曲线。共同平滑、非极暗、未裁切支持区域划分为 20×20 小像素块，按 4×4 块棋盘空间拆分训练与验证。共 1097 个验证块，其中 460 个彩色块；曝光使用 634 个训练近中性块，只估计一个通道共用标量。WB 与拟合矩阵候选另作明确标注。

亮度为线性 Cinema Gamut→XYZ D65 的 Y。色度 C18 为每块 XYZ 统一到 Y=0.18 后的 CIELAB C*，用于剥离曝光影响；它是色度幅度指标，不是 HSV saturation。色相为同一 Lab 平面的角度。ΔE76 仅为共同场景归一条件下的相对比较，不是色卡精度。

两份参考的纯曲线交叉验证：Log2 反解后再编码 Log3，与 Log3 参考的误差中位/P95 为 1.60/13.06 个 uint16 码；额外做一次 legal→full 展开则为 995/1376 码。数据支持 full-code 解释，不能通过删除黑位、重复 range 拉伸修复本次差异。Canon 公开曲线及 full-code 定义见[官方白皮书](https://downloads.canon.com/nw/pdfs/camcorders/whitepapers/canon-log-gamma-curves-white-paper.pdf)。

## 实际导出文件回读结果

下表来自交付 TIFF 的实际回读；所有行均已统一曝光。亮度列为绝对 EV 误差的中位/P95；色度和色相列均为彩色验证块的绝对误差中位数。

| 编码 | 处理 | 亮度误差中位／P95 EV | C18 绝对相对误差 | 色相绝对误差 |
| --- | --- | --- | --- | --- |
| C-Log 2 | 项目默认镜头校正，统一曝光 | 0.05577 / 0.15906 | 3.736% | 3.244° |
| C-Log 2 | 项目显影，镜头校正关闭 | 0.00263 / 0.05219 | 3.829% | 3.167° |
| C-Log 2 | 训练区域拟合 3×3 | 0.00194 / 0.03534 | 4.444% | 0.714° |
| C-Log 2 | 研究仓库保存的 Resolve 矩阵 | 0.00107 / 0.00497 | 0.172% | 0.269° |
| C-Log 3 | 项目默认镜头校正，统一曝光 | 0.05584 / 0.15904 | 3.729% | 3.251° |
| C-Log 3 | 项目显影，镜头校正关闭 | 0.00264 / 0.05192 | 3.801% | 3.147° |
| C-Log 3 | 训练区域拟合 3×3 | 0.00190 / 0.03494 | 4.432% | 0.717° |
| C-Log 3 | 研究仓库保存的 Resolve 矩阵 | 0.00112 / 0.00530 | 0.185% | 0.275° |

产品 FFV1 的实际解码也已核对：C-Log 2／3 的 native 输入像素 hash 完全相同，均为 `ef327e22d6d0ad0994f1c05a12ed54ba2bb8fff1ee18cab4bf01688611b35325`。解码后的 12-bit YUV 与当前产品转换器输出逐字节一致；两次完整 CLI 运行及 `--verify-output` 均成功，零输出裁切。FFV1 量化后的亮度误差中位约 0.0558 EV，色相约 3.24°，与 TIFF/native 对照一致，排除了视频编码为主要来源。

## 亮度分布与曝光

保留默认镜头校正时，C-Log 2／3 的 Canon 标定输出还需约 0.9382 倍（−0.092 EV）与参考近中性训练块对齐；这只是本次对照标量，不写入产品默认。关闭镜头模块后，未标定 generic 数据需约 2.3694 倍，与元数据计算的 native Canon 标定 2.376010479 很接近，剩余仅约 −0.004 EV。

LCP 暗角增益范围约 1～1.2822；同时做了畸变校正。默认配准相关系数 0.99410，关闭镜头模块后 0.99981。统一整体曝光后，默认残差中位 0.05577 EV，关闭镜头模块降到 0.00263 EV；因此不能将局部亮度分布变化误判成 Log 对比度曲线错误。

| 处理 | 参考亮度区间 EV18 | 验证块数 | 残差中位 EV | 绝对残差 P95 EV |
| --- | --- | --- | --- | --- |
| 默认镜头校正 | -8～-4 | 1 | -0.02162 | 0.02162 |
| 默认镜头校正 | -4～-2 | 26 | +0.05042 | 0.10802 |
| 默认镜头校正 | -2～0 | 440 | -0.04115 | 0.18179 |
| 默认镜头校正 | 0～2 | 630 | -0.00576 | 0.15832 |
| 镜头条件统一 | -8～-4 | 1 | -0.01415 | 0.01415 |
| 镜头条件统一 | -4～-2 | 26 | -0.02852 | 0.07057 |
| 镜头条件统一 | -2～0 | 440 | -0.04343 | 0.12640 |
| 镜头条件统一 | 0～2 | 630 | -0.00004 | 0.01191 |
| Resolve 矩阵验证 | -8～-4 | 1 | +0.01530 | 0.01530 |
| Resolve 矩阵验证 | -4～-2 | 26 | +0.00513 | 0.00798 |
| Resolve 矩阵验证 | -2～0 | 440 | +0.00201 | 0.00584 |
| Resolve 矩阵验证 | 0～2 | 630 | +0.00000 | 0.00255 |

## 饱和度与色彩倾向

统一镜头条件后的项目 C-Log 2 相对 Resolve：

| 参考色相组 | 验证块数 | C18 幅度变化中位 | 色相偏移中位 |
| --- | --- | --- | --- |
| red | 33 | -19.16% | -3.97° |
| yellow | 397 | +3.70% | -2.98° |
| yellow-green | 2 | -1.26% | -1.20° |
| blue | 20 | -6.38% | -5.51° |
| magenta | 8 | -5.67% | -3.65° |

红色明显偏淡，黄色略浓，蓝紫偏淡，并存在分色相的旋转；近中性区域的残差却很小。C-Log 3 得到几乎同样的趋势。这说明主要差别在共同的线性相机色彩处理之前／之中，而不是两种 Log 的曲线各有错误。黄色／木色占本场景彩色验证块的 397/460，绿色只有 2 块；总体中位数不能代表均匀色卡表现。

## 改进方向的反事实实测

| 假设／干预 | 结果 | 判断 |
| --- | --- | --- |
| 高光恢复 | 报告涉及 1 像素，但开／关 FP32 payload hash 完全相同 | 排除为本图来源 |
| 输出 CAT02 替换 Bradford | 默认条件下色相误差约 3.25°→3.37° | 不能照搬 D-Log 的改进结论，Canon 默认保留 Bradford |
| 近中性训练块拟合 WB，再实际 native 显影 | neutral 从 [.61244,1,.583144] 变 [.612692,1,.582564]；匹配镜头条件后色相约 3.17°→3.19° | 整体 WB 不是主要问题 |
| 诊断 DNG 副本中的 HSM 换为有效恒等表 | C18 中位变化 −46.09%，色相误差 7.43° | 当前配套 HSM 必须保留；未增加产品关闭选项 |
| 诊断副本隐藏 ForwardMatrix，SDK 走 ColorMatrix 回退 | C18 中位变化 +45.04%；亮度 P95 变差到 0.274 EV | 不能用回退矩阵替换当前路线来追求单项指标 |
| 全局色度缩放 .96865，训练块确定 | 总体 C18 绝对误差约 3.83%→0.64%，但红色 −19.16%→−21.72%，蓝色 −6.38%→−8.76%；色相变差 | 单一总体统计会误导；不作为默认修复 |
| 保中性 3×3，彩色训练块拟合，空间独立验证 | 色相约 3.17°→0.71°；但黄色偏浓加重，红色仍约 −17% | 支持矩阵／相机配置方向，不能视为通用 R7 校准 |
| 研究仓库保存的 Resolve R7 相机矩阵，颜色零拟合 | C18 绝对误差约 3.83%→0.17%，色相约 3.17°→0.27°，亮度 P95 约 .0522→.0050 EV | 强支持相机颜色表征是主要来源 |

最后一行复用了 `rev-crm-2/analysis/cause-investigation/exposure/captured-matrix-comparison.json` 中同一 R7_L2220 的 camera→Cinema Gamut 矩阵，结合本次实际 DNG 黑白位和原 CR3 NormalWhite。相机观测从有效恒等 HSM 的诊断副本恢复，不反演非线性 HSM。保存的 `params-2220-on.bin` 内两级矩阵经固定 1.8 换算，亦与该研究矩阵在最大 0.000324 内相符。完整来源 hash、矩阵和每项倍率在 runtime-matrix-control.json 与 runtime-source-confirmation.json。

该验证仍使用 ADC 去马赛克，几乎重现 Resolve 的新 TIFF；因此剩余差异可以在不换 demosaic 的条件下大幅消失。不能再主要归因于去马赛克、锐化、Log、range 或显示 ICC。约 0.27° 的余量仍包含低色度角度敏感性、不同 demosaic／锐化／舍入等，未逐项唯一分离。

## 官方资料、DCTL 与研究证据

[Adobe DNG 1.7.1](https://helpx.adobe.com/content/dam/help/en/camera-raw/digital-negative/jcr_content/root/content/flex/items/position/position-par/download_section_733958301/download-1/DNG_Spec_1_7_1_0.pdf) 的颜色处理流程将 HueSatMap 作为所选相机配置的颜色校正；LookTable 则是另外的外观阶段。这支持项目将有效 HSM 作为内在显影步骤，而不是和 Log 类型绑定。配置中的矩阵与 HSM 应整体使用。

[Canon 白皮书](https://downloads.canon.com/nw/pdfs/camcorders/whitepapers/canon-log-gamma-curves-white-paper.pdf) 给出 Log2／Log3 的 full-code 曲线、反射率和参考灰码。[OpenColorIO 原始 Canon 实现](https://raw.githubusercontent.com/AcademySoftwareFoundation/OpenColorIO/main/src/OpenColorIO/transforms/builtins/CanonCameras.cpp) 也包含相同的 0.9 反射率约定，Cinema Gamut 使用 D65；其 ACES 转换采用 CAT02。这些资料定义曲线与色域转换，不能自动替代 R7 的 camera→PCS 表征。

公开 [ACES DCTL 库](https://raw.githubusercontent.com/baldavenger/ACES_DCTL/master/ACES_LIB.h) 中的 CanonLog2_to_lin 与白皮书曲线一致；它还包含按 C300 等机型／光源命名的历史 IDT 包装，有额外 64/876 信号映射和专用相机矩阵。本 TIFF 的 full-code 实测不支持盲目套用其包装。DCTL 是实现交叉参考，不能将特定 C300 的矩阵当成 R7 校准。

本地研究的重要原始依据是 `rev-crm-2/analysis/cause-investigation/calibration-confirmed.md`：NormalWhite 选择、AutoTone 关闭时固定 1.8、scene 与 reflection 的区别已分别由保存的静态与运行时证据验证。`log-domains/findings.md` 的三套 Canon SDK 检查、官方 LUT 中性轴与本次两份新 TIFF 的交叉验证相互支持。NormalWhite×1.8 是 Resolve 固定照片路径的兼容约定，不是所有相机的物理反射率／灰卡标定证明。

## 本次产品修改与后续方向

- C-Log 2 唯一 ID 为 `canon-log2-cinema-gamut`，显示名 `clog2`；移除第二个 rev-eng ID 和重复 transfer 实现。公开曲线与恢复的 SDK 定义在 69633 个有符号样点上连续误差小于 1e-8。
- 原始 Canon CR3 的 C-Log 2／3 默认共用 `canon-cr3`：NormalWhite／黑位缩放 × 固定 1.8，随后在曲线边界做一次 0.9 转换。显式选择器由 canon-clog3 改为 canon-cr3；默认 auto 无需额外选项。
- 有效 HSM 继续在所有输出类型应用，未提供关闭路径。保留镜头默认、Bradford 输出、既有 Log3 曲线；未把本图拟合或保存的 Resolve 矩阵写入通用默认。
- 更好的通用显影应以已知色值、多光源、多曝光／ISO 样本校准完整输入配置（矩阵＋配套 HSM），并按色相均衡评价。若目标为严格复现 Resolve，应建立明确的输入表征来源和验证范围，而不是在输出 Log 中添加针对这张图的色彩补偿。
- 输入显影、曝光标定与输出编码还可进一步解耦。当前已统一 Canon Log 2/3；跨所有其他输出 profile 的相机曝光语义属于后续边界，不能把固定 1.8 的兼容约定直接推广为普适显影。

已通过 407 项 Rust 测试（另 2 项既有 ignored）和 14 项 native 检查，完成 native／Rust release 构建；两个真实原始 CR3 产品导出成功，FFV1 回读精确；八个 TIFF 回读 RGB16 逐像素精确、均零输出裁切。D-Log 2 的 5 项交付测试也已通过。沙箱内进程查询测试曾因 EPERM 失败，授权环境重跑后通过。

## 交付与复现

完整分析、图表、脚本、命令、来源 hash、metadata、精度验证和导出文件都在 `outputs/canon-resolve-comparison/`。HTML 报告为其中 report.html。

TIFF 均为 6960×4640 RGB16，无 ICC；导入调色软件时指定 Canon Cinema Gamut 与相应 Canon Log 2／3，按 full signal 解释。文件类型：

- `exposure_matched`：项目默认镜头路径，仅统一整体曝光。
- `reference_conditions`：关闭项目镜头模块，统一整体曝光，供直接观察相机颜色差异。
- `experimental_matrix`：空间训练块拟合 3×3，保留作为成功与失败并存的实验。
- `resolve_matrix_control`：复用研究仓库保存的 R7 矩阵；颜色无本图拟合，曝光统一。这是来源验证版。
- 根目录两个 `project_default.mkv`：实际 CLI 输出，12-bit FFV1，源几何 6984×4660，默认参考增益 1，未加入分析中的曝光拟合。

复现顺序：准备 CR3（保存的 prepare*.log／frame-lens.context），运行 decode.py；构建 exporter；使用保存的 baseline-profile-matrices.json 保留合并前对照矩阵；analyze.py → fit-controls.py → decode.py wb → profile-controls.py → analyze.py → runtime-matrix-control.py → run-product.py → export-deliverables.py → export-runtime-control.py → verify-product.py → verify-tiffs.py → preview-final.py／plot-candidates.py。原始输入和研究矩阵路径记录在 input-identities.json、product-commands.json、各 *-command.json 及 runtime-matrix-control.json。重新执行导出应使用新的输出目录，避免覆盖本轮证据。
