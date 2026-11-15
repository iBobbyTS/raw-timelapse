# 镜头校正：数据契约、执行边界与能力范围

## 系统设计

镜头校正属于 Develop 内部的独立模块，不新增节点。图调度、相机颜色配方、高光重建均不拥有镜头模型知识。

- **Prepare 元数据适配器**：只读原始 RAW 和已验证的 LinearRaw，采集拍摄参数、原始校准与分项上游状态；读取本机已安装的 Adobe LCP，冻结匹配与插值结果。
- **交接契约**：prepared context 中可选的 lens_calibration 字段承载 rawtl-lens-calibration/1，含原始 make/camera/lens、焦距/光圈/对焦距离、证据、三项上游状态、内嵌模型、LCP 回退模型与来源指纹。字段仍由 prepared context 的原始/缓存 SHA-256 身份校验约束。无该字段的旧诊断 context 只能关闭新增校正，不能请求 auto。
- **独立数值核**：只接收 camera-linear interleaved RGB、尺寸、Calibration、策略及取消回调；不读文件、不调用 SDK/ExifTool、不认识 CameraRecipe、Decoded、图节点或曝光解算。独立 CMake lens_core 目标可无 SDK 进行数值单测。
- **Develop 组合层**：传入冻结的校准，在既有高光重建之后调用镜头核，随后调用既有 WB/矩阵/HueSatMap/标定/EV。各模块通过像素缓冲与自身契约组合，不互相调用算法；组合层只维护通用 delivery_camera，颜色消费者无需识别前面执行过哪些图像模块。
- **Rust 消费者**：传递设置并校验原生返回的 lens revision/请求/三项状态；单份运行日志逐帧写 kind=lens，不解释厂商系数。

执行顺序：SDK camera observations → highlight delivery → lens vignette → one combined distortion/CA sample → WB/matrix → HSM → scene calibration → EV → AP1 FP32。

原始观测保留；analysis 禁用新增镜头校正和高光重建，保持当前测光合同。新增校正不会改变去闪观测，但原本已经由 Converter/SDK 烘焙的处理同样无法撤销。

## 分项决策

暗角、几何畸变、横向色差分别选择：

1. off：不执行新增处理，保留上游来源状态。
2. Converter 明确标记 AlreadyApplied：不重复应用；标记不等于模型具有非零效应。
3. 原始 DNG 含 opcode：由 Converter/SDK 占用，不再叠加 LCP。当前对整组 opcode 保守占用三项，不声称已逐条证明非零效果；直接 LinearRaw 的现存列表也由 SDK 负责。
4. 直接 LinearRaw 无可确认状态：unknown-upstream，跳过而非猜测未处理。
5. 可执行内嵌模型优先，**恒等模型仍然优先**，不会因没有像素变化而转向 LCP。
6. 未提供/暂不支持的内嵌模型，仅在上游状态允许且坐标可解释时使用 LCP；无可用模型明确报告。若上游已执行几何处理而暗角未做，当前没有逆坐标映射，报告 upstream-geometry-unmapped，不把原始坐标暗角叠到未知几何结果上。

对于经 Converter 的文件，未出现分项 applied 标记记为 **not-reported**，不是数学意义的未处理保证。auto 在这一条件下允许模型回退；日志保存这一证据强度，不能把它写成所有品牌/Converter 版本的永久定律。批量验收应继续覆盖新 RAW。

## 首轮模型能力

- **Sony ARW**：支持 SR2SubIFD 优先、SubIFD 次之的 16 点暗角/畸变和 32 点 R/B 色差数组；保留普通 MakerNotes 作为证据，不将其不同格式数组误读成 SR2 参数。参数转换与半对角线半结点插值采用已公开的 Sony 模型数学语义；每帧记录冻结模型的指纹。
- **DNG embedded opcode**：继续由 Adobe 链路消费；不另写 GainMap/warp 的第二份执行器。
- **Nikon 私有畸变/暗角**：保留版本和系数。Required 文件已有处理时跳过；Optional 未处理且无可用 LCP 时报告 unsupported-embedded。本轮没有猜测未经验证的私有模型公式。
- **其他厂商私有模型**：已有分项 applied 标记受到保护；未支持的内嵌模型不冒充已执行。Fuji 等样本已由 Converter 烘焙的三项不重复应用。
- **LCP**：支持 RAW perspective 版本1/2、径向 r²/r⁴/r⁶、切向、vignette 和 relative R/G/B CA 模型；不支持 fisheye 或未知版本。保留浮点符号与 HDR，不套显示曲线。

Sony 模型数学与 LCP 坐标语义参考公开资料：[RawTherapee lens metadata](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/lensmetadata.cc)、[RawTherapee LCP](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/lcp.cc)。本项目采用独立的校准结构、决策核、受控采样及回归测试，不依赖其运行时。

## LCP 选择与插值

默认查用户、Camera Raw、Lightroom 安装目录；不会复制 Adobe profile 到仓库。显式 --lens-profile-dir 替换默认根目录，可重复传入。未安装默认目录可以跳过；显式错误路径是配置失败。

匹配 make + lens 的大小写/字母数字规范化结果，**不以共享数字 LensID 单独匹配、不做模糊相似匹配**；精确相机型号优先。相同 SHA-256 的两份安装副本合并；同优先级不同 profile 不任意挑选，报告 ambiguous-lcp-match。当前未引入可变全局目录缓存；一次 Prepare 批次建立一个轻量 header index，完整解析只针对候选。

参数按 log 焦距、APEX 光圈和 log 对焦距离进行分层、端点夹紧插值，不外推；缺对焦距离采用最远标定样本，并明确记录。缺光圈不推算暗角。焦平面分辨率/35mm 等效焦距用于判定传感器裁切；无可靠传感器几何或宽高比不匹配时不应用；坐标轴互换也不隐式视为兼容，EXIF 显示方向不能代替校准轴变换。profile 与输出的 sensor crop 比例显式作用于归一化焦距和主点。

每个模型记录 profile 路径和**实际解析字节**的 SHA-256；Develop 不依赖该文件继续存在，不能在同次运行的不同消费阶段重新匹配 profile。

## 像素与资源合同

暗角在输入坐标修正后再做几何采样；畸变和 CA 联合为一次双线性采样，不反复插值。输出尺寸保持不变，越界样本填黑并计数。**不隐式自动放大/裁切**；后续若需要无黑边的自动 crop，应作为单独明确的产品需求，不可偷偷改变此开关含义。

无新增处理时不分配交付像素副本；有处理时额外需要一个输出缓冲，暗角+warp 同时启用时还需要一份临时暗角缓冲。现有 Develop 并发度仍然有效；未承诺大尺寸/高并发下的吞吐或内存上限。

## 配置

产品 CLI 默认 auto，支持 --lens-correction auto|off。

Prepare auto 新增运行依赖 **ExifTool**，默认 /opt/homebrew/bin/exiftool，可用 --exiftool 指定绝对可执行路径。调用通过 argv，无 shell，禁用用户 ExifTool config；XML 通过 LibXml2 有界解析，禁用 DTD/外部实体与网络。任一真正消费的单值字段冲突、工具失败、损坏契约都显式失败；非消费字段的合法多值保留为证据。

native 独立诊断默认 off。显式启用时，--prepare / --prepare-batch 负责采集；--stdout-frame-f32 --prepared 只接受 auto/off，不允许把 ExifTool/LCP 路径交给 Develop。analysis 即使收到 auto 也不新增处理。

## 验证与限制

独立数值测试覆盖真值增益、warp 方向/归一化、R/B-only CA、signed HDR、恒等优先、不重复/未知跳过、坐标保护、越界、取消、严格序列化；profile 测试覆盖已知多维插值、夹紧、缺失元数据、crop、精确匹配、相同副本/歧义、未知版本和 XML 实体拒绝。

真实 SDK 集成覆盖 single/batch 一致、参数冻结后 profile/tool 变化不影响 Develop、测光隔离、EV 等变、原始文件完整性、损坏版本与消费字段冲突拒绝。真实 RAW/实际产品输出的本轮证据在开发工作目录，不能用合成测试代替新相机的画质验收。

当前原生适配器仍只构建 macOS；列出 Windows profile 根目录不等于完成 Windows 编译验证。此功能不复刻 Lightroom 的颜色、裁切和渲染外观，也不保证所有镜头匹配或所有私有内嵌模型已支持。
