#pragma once
// Fixed-grid robust photometry for the deflicker node (legacy V4 port).
// The analysis runs on gained ACEScg scene-linear samples; the saturation mask
// is evaluated in the normalized camera domain, so it is invariant to EV.
#include "frame.h"
#include <algorithm>
#include <array>
#include <cstdint>
#include <optional>
#include <string>
#include <utility>
#include <vector>
namespace frame {
namespace analysis {
constexpr int kMaxEdge = 1024;
constexpr int kGridNx = 12;
constexpr int kGridNy = 8;
constexpr int kMinTileEdge = 16;
constexpr int kMinTiles = 8;
constexpr int kStructureStride = 4;
constexpr double kLumaR = 0.2126;
constexpr double kLumaG = 0.7152;
constexpr double kLumaB = 0.0722;
// 16-bit delivered scale; floor = max(1e-5, 8 * quantum).
constexpr double kQuantum = 1.0 / 16384.0;
constexpr double kFloor = 0.00048828125;
constexpr double kSaturationThreshold = 0.98;
constexpr int kOutputClipCode = 16057; // smallest code with code/16384 >= 0.98
struct Roi {
  double x = 0, y = 0, w = 1, h = 1;
};
// Rejects non-finite / out-of-range ROIs before decode-side work is used.
void validate_roi(const Roi &roi);
struct Grid {
  int width = 0, height = 0; // downsampled analysis dimensions
  int x0 = 0, y0 = 0, x1 = 0, y1 = 0;
  int nx = 0, ny = 0;
  std::vector<int> xs, ys; // nx+1 / ny+1 tile boundaries
};
// Deterministic geometry: long edge <= 1024, grid min(12,8), >= 8 tiles of at
// least 16x16 measured pixels. Throws Error(4) for ROI_TOO_SMALL.
Grid make_grid(int source_width, int source_height, const Roi &roi);
// 5%/5% trimmed mean in the linear domain. Negative noise is not clamped.
// Returns NaN for empty input or any non-finite value.
double trimmed_linear_mean(std::vector<double> values, double fraction = 0.05);
// Delivered-domain clip: the exact quantization-code predicate code >= 16057.
bool delivered_clip(float value);
namespace detail {
inline std::vector<std::vector<std::pair<int, int>>>
box_weights(int source, int dest) {
  std::vector<std::vector<std::pair<int, int>>> map(
      static_cast<size_t>(source));
  for (int d = 0; d < dest; ++d) {
    int64_t start = int64_t(d) * source;
    int64_t end = int64_t(d + 1) * source;
    int first = int(start / dest);
    int last = int((end + dest - 1) / dest);
    for (int s = first; s < last; ++s) {
      int64_t lo = std::max<int64_t>(start, int64_t(s) * dest);
      int64_t hi = std::min<int64_t>(end, int64_t(s + 1) * dest);
      if (hi > lo)
        map[size_t(s)].emplace_back(d, int(hi - lo));
    }
  }
  return map;
}
} // namespace detail
// Exact area (BOX) downsample with integer coverage weights. Source pixel
// coverage is accumulated in double and normalized once by (sw*sh).
template <typename F>
std::vector<double> box_downsample(int sw, int sh, int dw, int dh, F value) {
  if (sw == dw && sh == dh) {
    std::vector<double> out(size_t(sw) * size_t(sh));
    for (int y = 0; y < sh; ++y)
      for (int x = 0; x < sw; ++x)
        out[size_t(y) * dw + x] = value(size_t(y) * sw + x);
    return out;
  }
  auto xw = detail::box_weights(sw, dw);
  auto yw = detail::box_weights(sh, dh);
  std::vector<double> horizontal(size_t(sh) * size_t(dw), 0.0);
  for (int y = 0; y < sh; ++y) {
    double *row = &horizontal[size_t(y) * dw];
    for (int x = 0; x < sw; ++x) {
      double v = value(size_t(y) * sw + x);
      for (auto [d, weight] : xw[size_t(x)])
        row[d] += v * weight;
    }
  }
  std::vector<double> out(size_t(dw) * size_t(dh), 0.0);
  for (int y = 0; y < sh; ++y)
    for (auto [d, weight] : yw[size_t(y)]) {
      const double *row = &horizontal[size_t(y) * dw];
      double *dest = &out[size_t(d) * dw];
      for (int x = 0; x < dw; ++x)
        dest[x] += row[x] * weight;
    }
  double normalize = double(sw) * double(sh);
  for (double &v : out)
    v /= normalize;
  return out;
}
struct Tile {
  double center_x = 0, center_y = 0;
  bool valid = false;
  bool has_log_luminance = false;
  double log_luminance = 0;
  double saturation_fraction = 1;
  double output_clip_fraction = 0;
  std::vector<double> structure_samples; // log2 domain, tile ::4 stride
  std::array<std::optional<double>, 3> channel_log_means;
};
struct Analysis {
  Grid grid;
  Roi roi;
  double exposure_ev = 0;
  uint32_t source_width = 0, source_height = 0;
  std::vector<Tile> tiles; // row-major (j * nx + i)
};
// d.camera must be the normalized camera-domain FP32 data (post Linearize,
// pre white balance / color transform). gained_scene is the post-gain ACEScg
// FP32 delivery data, size width*height*3.
Analysis measure(const Decoded &d, const std::vector<float> &gained_scene,
                 const Roi &roi, double exposure_ev);
Json sidecar_json(const Analysis &analysis, const std::string &source_sha256);
} // namespace analysis
} // namespace frame
