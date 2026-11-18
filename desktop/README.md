# RAW Timelapse 桌面端

Qt 6 Quick / QML 桌面工作区。打开 RAW 目录后，预览进程显影当前帧，右侧设置保存到本次导出的处理 profile。导出队列调用既有 CLI 并按顺序逐个运行。输出沿用 CLI 命名，保存在输入目录旁边。

## 在 macOS 构建和运行

需要 CMake 3.24+、Rust、含 Quick、Quick Controls、Quick Dialogs 和 SVG 的 Qt 6.8+、ExifTool、FFmpeg 和 FFprobe。使用 macOS 26 SDK 的 Xcode 建议使用 Qt 6.9+。先按 [显影适配器说明](../native/dng-linear-frame/README.md) 构建 `native/build/dng-linear-frame`。需要转换 RAW 时，安装 Adobe DNG Converter。

在仓库根目录运行：

```sh
cmake -S desktop -B desktop/build -DCMAKE_BUILD_TYPE=Release -DCMAKE_PREFIX_PATH=/path/to/Qt/macos
cmake --build desktop/build --parallel
open desktop/build/raw-timelapse.app
```

CMake 同时编译既有的导出 CLI 和私有预览 worker。用 `cmake --install desktop/build --prefix desktop/build/dist` 可以部署 Qt 运行库和这两个 Rust 可执行文件。FFmpeg、ExifTool、Adobe DNG Converter 和显影适配器作为外部依赖。

默认会查找应用旁边的工具、构建目录中的管线工具、仓库的 `native/build/dng-linear-frame` 和系统工具。也可从终端用环境变量传入工具的完整路径：

| 环境变量 | 工具 |
| --- | --- |
| `RAW_TIMELAPSE_NATIVE` | `dng-linear-frame` |
| `RAW_TIMELAPSE_EXIFTOOL` | ExifTool |
| `RAW_TIMELAPSE_FFMPEG` | FFmpeg |
| `RAW_TIMELAPSE_FFPROBE` | FFprobe |
| `RAW_TIMELAPSE_CONVERTER` | Adobe DNG Converter |
| `RAW_TIMELAPSE_CLI` | `rawtl-linear-video` |
| `RAW_TIMELAPSE_WORKER` | `rawtl-desktop-worker` |

## Windows 状态

Qt 工作区、队列和打开输出目录使用跨平台接口。Windows 上使用本机 Qt Kit 和 Rust 工具链，从相同的 `desktop` CMake 目录构建。仓库中现有的 `dng-linear-frame` 仍只能在 macOS 构建，Windows 的 RAW 预览和导出还需要原生适配器移植。

## 预览与交付

预览保留最近成功的画面，异步合并快速拖动产生的过期请求。新目录通过校验后释放旧目录的显影缓存。预览使用相机线性缩小后的数据做镜头和色彩处理，完整导出使用原来的全尺寸处理。初版不做显示器色彩管理、整段去闪预览或整段 Log 自动增益扫描。
