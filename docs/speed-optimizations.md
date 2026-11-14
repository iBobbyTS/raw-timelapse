# 速度优化成果总览

日期：2026-11-14。候选：main `2d30458` + feature/adc-batch-prepare `41d2536`（5 提交，未合并）。
测量主机：Mac16,5 Apple M4 Max（14 逻辑核 / 36 GiB），macOS 27.0.1；ADC 18.7 (2743)，FFmpeg 8.0.1；素材为 Canon EOS R7 32MP CR3（476 帧 = 13.4 GiB）。所有数字附原始收据路径；加载交互主机、SMB 源、无固定加速比承诺。

## 成果一览

| # | 优化 | 状态 | 实测收益（决定性观察） | 收据 |
|---|---|---|---|---|
| 1 | 有序节点并发（P4/S8/D4） | 已合入 main | 476 帧全片 CLI 墙钟 2239.1s（N=1）→ 1484.1s（−33.7%）；476 帧解码哈希逐一相同 | 旧任务归档 tmp/ordered-node-concurrency/s02/ |
| 2 | 输出验证默认仅元数据 | 已合入 main（2d30458） | 10.86 GiB 成品上：旧默认（-count_frames 223.0s + 整段解码 22.3s ≈ 245s）→ 新默认 0.9s；每全片省 ≈248s（当时 1484s 的 16.7%） | tmp/native-overhead-probe/probe-all.log D1–D3 |
| 3 | ADC 单进程批量转换（-mp，B32） | feature 分支 41d2536 | ADC 调用 476→15；干净条件三尺度全占优：19 帧 22.3 vs 23.7–29.7s，240 帧 222.1 vs 233.1s，476 帧 394.5 vs 454.6s（−13.2%），两视频 SHA-256 一致 | evidence/adc-batch-prepare/full-pair-repeat/ + tmp/adc-batch-prepare/ |

## 1. 有序节点并发（基础，已交付）

- 机制：每节点 `concurrency`，在途容量=并发数，逐帧有序提交；solve 仍全序列屏障。
- 476 帧：N=1 2239.068s / RSS 4.371 GiB → 4/8/4 1484.127s / RSS 7.774 GiB（含旧全量验证 ~249s 于两侧）。
- 权衡：内存上升；本项为后续两项的承载底座。

## 2. 输出验证默认仅元数据（已交付）

- 机制：默认只做容器元数据校验（ffprobe 无 `-count_frames`、不解码）；`--verify-output` 恢复逐帧计数+整段解码。
- 实测（真实 10.86 GiB ProRes 成品）：元数据探测 0.90s；`-count_frames` 223.02s；整段解码 22.29s。旧默认 ≈245s（与战役实测 249.1s 交叉吻合）→ 新默认 0.9s。
- 诚实限制：MOV/ProRes 不声明帧数，默认模式跳过帧数核对（日志用编码器计数）；编解码/像素格式/几何/颜色标签仍实测核对。

## 3. ADC 批量转换（本次新增，含安全就绪架构）

- 机制：prepare 节点 `batch_size=32`（默认图 P1/B32/S8/D4）；native `--prepare-batch` 多文件 `-c -l -d DIR -mp` 共享一次 ADC；就绪边界=进程组终止+全批输出/结构/哈希核验+原子发布；任一失败整批零提交（半写/未退出文件在图消费层证实零读取）。
- ADC 调用：476 → 15（14×32 + 1×28）。
- 阶段账（476 帧换序重跑，同窗）：批适配器总计 217.8s（其中转换 165.4s + 输出核验 22.5s）vs 逐帧 prepare 帧时长总和 1118.6s —— prepare 侧工作量 −80%；develop 两模式逐帧分布完全一致（均值 1.391 vs 1.392s，无尖峰）。

### 端到端测量（同一窗口、同图参数、同 SMB 源、同输出卷、同二进制）

| 规模 | 逐帧（P4/B1） | 批（P1/B32） | 差异 |
|---|---|---|---|
| 19 帧 | 29.72 / 23.74s（两轮） | 23.37 / 22.30s | 批快 6–21% |
| 240 帧 | 233.07s | 222.15s | 批快 4.7% |
| 476 帧（换序重跑） | 454.65s | 394.49s | **批快 13.2%**；两视频字节一致（SHA-256 da71dc1d…） |

- 叠加效果（同日同窗）：默认全流程（并发+批量+元数据验证）476 帧 = 394.5s。与逐帧基线的历史数字（N=1+全量验证 2239s，另一天、含旧验证）相比 ≈5.7×，属跨条件对比，仅供参考。
- 权衡（如实）：RSS 峰值更高（476 帧首对 5.42→7.89 GiB；19 帧 4.8–5.0→5.9–6.0 GiB）；首帧可测量时间更晚（19 帧 3.5→9.3s；全片首对 12.3→53.8s）——需等首批整批完成。

### 重要测量教训：环境扰动

首轮全片对（14:20–15:41 窗口）批 3155.3s vs 逐帧 1670.5s（批"慢 88.9%"）。诊断（换序重跑+240 帧配对+逐帧分布分析）判定为该窗口外部 I/O 扰动：同时段逐帧侧也被拖慢 3.6×（prepare 10.2→2.35s/帧），批侧 develop 出现 30–105s U 态尖峰（重跑中完全消失，p99 1.92s）。结论：非批机制固有行为；长计时测量需警惕此类窗口。诊断报告：`.agent-work/evidence/adc-batch-prepare/full-pair-repeat/DIAGNOSIS.md`。

## 未采纳/暂缓的方向

- measure+develop 双解码（全片 611s+962s 各解码同一 DNG 一次）：受 solve 屏障内存约束，降采样测量会改变估计器语义——产品决策，暂缓。
- 更细并发/批大小搜索：按用户指示"暂时不做"；B32 为工程初值。
- 常驻转换服务、文件监听式提前发布：设计阶段排除（正确性边界不可靠）。

## 收据索引

- 并发基线：`.agent-work/archive`（ordered-node-concurrency）与 tmp/ordered-node-concurrency/s02/（全片对、等价哈希）。
- 验证探测：`.agent-work/tmp/native-overhead-probe/probe-all.log`（D1/D2/D3）。
- 批量实现/评审/测试：`.agent-work/tmp/adc-batch-prepare/{s01,s01-repair,s02,s02-simp2,s02-full}/`。
- 批量性能与诊断：`.agent-work/tmp/adc-batch-prepare/s02/perf-final-*`（19 帧×4 轮）、`.agent-work/evidence/adc-batch-prepare/full-pair-repeat/`（240/476 配对+诊断）；视频在 `/Volumes/4T/Projects/raw-timelapse/adc-batch-prepare-*`。
