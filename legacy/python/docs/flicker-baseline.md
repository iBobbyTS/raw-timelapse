# 残余闪烁修复基线（F0）

本文件固定 2026-11 起的"真实成片残余闪烁"修复工作的证据基线。审阅基线提交为
<code>8b9b505</code>（<code>--brightness uniform</code> 引入版本）。本仓库不包含任何实拍素材、GPS、设备序列号或本机绝对路径；真实数据的 manifest 与哈希只保存在本地未跟踪目录，公开仓库只保留可复用配置与合成测试。

## 两类证据，不得混淆

| 类别 | 含义 | 位置 |
|---|---|---|
| 用户上传结果 | 用户在提交审阅包之前生成的 <code>video.mp4</code> 及报告（SHA-256 <code>6c324979…9ccb68</code>） | 用户输出目录（本地，永不覆盖） |
| 本地重新运行结果 | 在审阅基线提交上、同一输入与本机环境重新执行的管线与审计 | 本地未跟踪目录，每次运行单独存放 |

上游审阅包对该上传视频的完整解码测量（前 77 帧：相邻共同变化 P95 0.0678 EV、最大 0.0999 EV、三帧残差 RMS 0.0262 EV、9 对 >0.05 EV）是本任务要复现并最终改善的失败基准。关键事件（帧 37、38、40、73、75、76，符号与幅度见审阅包 <code>evidence/selected_events.csv</code>）必须在本地复测中重现后才允许开始算法改动。

## 素材与分段事实

- 119 张 DJI DNG，按文件名排序；正常拍摄间隔 60 秒（中位数）。
- 按拍摄时间连续性（间隔 >1.5× 中位数视为缺口）得到 10 个连续段与 9 个缺口；帧 76→77 缺口为 38400 秒（前一天夜间到次日早晨）。
- 固定基准段：<code>[0,77)</code> 为主验收段，<code>[80,104)</code> 为白天独立测量段；1–7 帧短段只做逐对记录。
- 四个独立验收 ROI（墙面/天花板，归一化 <code>[x0,y0,x1,y1]</code> 坐标）见 <code>benchmarks/dji-ac004-indoor/rois.json</code>；它们不参与任何估计器的拟合（后续任务 F4 起强制）。
- 拍摄时间只用于连续性诊断与统计分段；帧顺序始终按文件名排序，不按时间重排。

## 本地环境指纹（记录于运行时，不在仓库）

Python 3.13.2 / rawpy 0.27.1 / LibRaw 0.22.1 / NumPy 2.3.5 / SciPy 1.17.0 / Pillow 12.3.0 / FFmpeg 8.0.1（含 libx264 与 VideoToolbox H.264 编码器）/ ExifTool 13.50 / macOS arm64。完整字段（含 HEAD、分支、可用编码器清单、上传视频哈希）由本地脚本生成于未跟踪目录。

## 复现命令形制

    # 独立成片审计（F1 起由 rawtl audit-video 提供生产实现；F0 期间用审阅包参考工具）
    python tools/audit_video.py <VIDEO> --report <REPORT.json> --raw-dir <RAW_DIR> \
        --transfer srgb --rois benchmarks/dji-ac004-indoor/rois.json -o <OUT>

    # 旧管线复跑（与上传配置一致；输出目录必须不同于用户上传目录）
    python -m rawtl run <RAW_DIR> -o <NEW_OUT> --ffmpeg <FFMPEG> --brightness uniform

<code>--transfer srgb</code> 是依据当前源代码（sRGB OETF 输出）的显式假设，不是通用默认；视频流缺少 transfer/primaries 标签时必须记录警告（当前上传视频即如此）。

## 基线约束（继承自审阅任务单）

1. 判定"去闪改善"只能依据独立检测器在真实成片上的测量，不得以管线自身统计量（增益探针、冻结目标误差）代替。
2. 优化区域与验收区域分离；四个 ROI 不参与拟合。
3. 统计不得跨越 9 个源时间缺口；短段不得伪造成长时稳定证据。
4. 原用户输出永不覆盖；每次候选渲染落在独立目录并保留 manifest 关联。
