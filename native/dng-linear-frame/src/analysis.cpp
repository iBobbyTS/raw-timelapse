#include "analysis.h"
#include <algorithm>
#include <cmath>
#include <iomanip>
#include <limits>
#include <locale>
#include <sstream>
namespace frame {
namespace analysis {
namespace {
double mean_of(std::vector<double> values) {
  if (values.empty())
    return std::numeric_limits<double>::quiet_NaN();
  double sum = 0;
  for (double v : values)
    sum += v;
  return sum / double(values.size());
}
std::vector<int> axis(int first, int last, int count) {
  std::vector<int> out(size_t(count) + 1);
  for (int k = 0; k < count; ++k)
    out[size_t(k)] =
        int(std::rint(double(first) + double(last - first) * k / count));
  out[size_t(count)] = last;
  return out;
}
Json number(double v) {
  if (!std::isfinite(v))
    return Json();
  std::ostringstream o;
  o.imbue(std::locale::classic());
  o << std::setprecision(10) << v;
  Json j;
  j.text = o.str();
  return j;
}
} // namespace
void validate_roi(const Roi &roi) {
  if (!std::isfinite(roi.x) || !std::isfinite(roi.y) ||
      !std::isfinite(roi.w) || !std::isfinite(roi.h))
    throw Error(3, "analysis ROI must be finite");
  if (roi.x < 0 || roi.y < 0 || roi.w <= 0 || roi.h <= 0 ||
      roi.x + roi.w > 1 || roi.y + roi.h > 1)
    throw Error(3, "analysis ROI must satisfy 0<=x, 0<=y, w>0, h>0, x+w<=1, "
                   "y+h<=1");
}
Grid make_grid(int source_width, int source_height, const Roi &roi) {
  validate_roi(roi);
  if (source_width < 1 || source_height < 1)
    throw Error(30, "analysis requires a nonempty image");
  double scale = std::min(
      1.0, double(kMaxEdge) / double(std::max(source_width, source_height)));
  Grid g;
  g.width = std::max(1, int(std::rint(source_width * scale)));
  g.height = std::max(1, int(std::rint(source_height * scale)));
  g.x0 = int(std::rint(roi.x * g.width));
  g.y0 = int(std::rint(roi.y * g.height));
  g.x1 = int(std::rint((roi.x + roi.w) * g.width));
  g.y1 = int(std::rint((roi.y + roi.h) * g.height));
  int w = g.x1 - g.x0, h = g.y1 - g.y0;
  g.nx = std::min(kGridNx, w / kMinTileEdge);
  g.ny = std::min(kGridNy, h / kMinTileEdge);
  if (g.nx < 2 || g.ny < 2 || g.nx * g.ny < kMinTiles)
    throw Error(4, "ROI_TOO_SMALL: need >=8 tiles of at least 16x16 analysis "
                   "pixels");
  g.xs = axis(g.x0, g.x1, g.nx);
  g.ys = axis(g.y0, g.y1, g.ny);
  return g;
}
double trimmed_linear_mean(std::vector<double> values, double fraction) {
  if (values.empty() || !std::all_of(values.begin(), values.end(),
                                     [](double v) { return std::isfinite(v); }))
    return std::numeric_limits<double>::quiet_NaN();
  size_t n = size_t(double(values.size()) * fraction);
  if (n == 0)
    return mean_of(std::move(values));
  std::sort(values.begin(), values.end());
  std::vector<double> middle(values.begin() + std::ptrdiff_t(n),
                             values.end() - std::ptrdiff_t(n));
  return mean_of(std::move(middle));
}
bool analysis_highlight(float value) { return value >= kHighlightThreshold; }
Analysis measure(const Decoded &d, const std::vector<float> &gained_scene,
                 const Roi &roi, double exposure_ev) {
  const size_t pixels = size_t(d.width) * size_t(d.height);
  if (d.width < 1 || d.height < 1 ||
      d.camera.size() != pixels * size_t(3) ||
      gained_scene.size() != pixels * size_t(3))
    throw Error(40, "analysis input geometry mismatch");
  Analysis out;
  out.grid = make_grid(int(d.width), int(d.height), roi);
  out.roi = roi;
  out.exposure_ev = exposure_ev;
  out.source_width = d.width;
  out.source_height = d.height;
  const Grid &g = out.grid;
  const int width = g.width, height = g.height;
  std::array<std::vector<double>, 3> channels;
  auto luminance = box_downsample(
      int(d.width), int(d.height), width, height, [&](size_t i) {
        const float *p = &gained_scene[i * 3];
        return kLumaR * double(p[0]) + kLumaG * double(p[1]) +
               kLumaB * double(p[2]);
      });
  for (int c = 0; c < 3; ++c)
    channels[size_t(c)] = box_downsample(
        int(d.width), int(d.height), width, height,
        [&](size_t i) { return double(gained_scene[i * 3 + size_t(c)]); });
  // Camera-domain mask: post Linearize, pre white balance/color. 1.0 equals
  // WhiteLevel, so this mask does not move with the applied EV.
  auto saturation = box_downsample(
      int(d.width), int(d.height), width, height, [&](size_t i) {
        const float *p = &d.camera[i * 3];
        return (p[0] >= kSaturationThreshold || p[1] >= kSaturationThreshold ||
                p[2] >= kSaturationThreshold)
                   ? 1.0
                   : 0.0;
      });
  // Analysis-only AP1 highlight mask. Does not predict final profile clipping.
  auto clipping = box_downsample(
      int(d.width), int(d.height), width, height, [&](size_t i) {
        const float *p = &gained_scene[i * 3];
        return (analysis_highlight(p[0]) || analysis_highlight(p[1]) ||
                analysis_highlight(p[2]))
                   ? 1.0
                   : 0.0;
      });
  out.tiles.reserve(size_t(g.nx) * size_t(g.ny));
  for (int j = 0; j < g.ny; ++j) {
    for (int i = 0; i < g.nx; ++i) {
      const int x0 = g.xs[size_t(i)], x1 = g.xs[size_t(i) + 1];
      const int y0 = g.ys[size_t(j)], y1 = g.ys[size_t(j) + 1];
      Tile tile;
      tile.center_x = (double(i) + 0.5) / double(g.nx);
      tile.center_y = (double(j) + 0.5) / double(g.ny);
      std::vector<double> block;
      block.reserve(size_t(x1 - x0) * size_t(y1 - y0));
      double saturation_sum = 0, clipping_sum = 0;
      bool finite = true;
      for (int y = y0; y < y1; ++y) {
        for (int x = x0; x < x1; ++x) {
          double v = luminance[size_t(y) * width + x];
          finite = finite && std::isfinite(v);
          block.push_back(v);
          saturation_sum += saturation[size_t(y) * width + x];
          clipping_sum += clipping[size_t(y) * width + x];
        }
      }
      const double count = double(block.size());
      const double coverage = count > 0 ? saturation_sum / count : 1.0;
      const double clip = count > 0 ? clipping_sum / count : 0.0;
      const double mean = trimmed_linear_mean(block);
      tile.saturation_fraction = coverage;
      tile.analysis_highlight_fraction = clip;
      tile.valid = block.size() >= 256 && finite && std::isfinite(coverage) &&
                   std::isfinite(clip) && coverage <= 0.005 &&
                   std::isfinite(mean) && mean > kFloor;
      if (tile.valid) {
        tile.log_luminance = std::log2(mean);
        tile.has_log_luminance = true;
      }
      // Deterministic strided structure probe in the log2 domain.
      for (int y = y0; y < y1; y += kStructureStride)
        for (int x = x0; x < x1; x += kStructureStride)
          tile.structure_samples.push_back(
              std::log2(std::max(luminance[size_t(y) * width + x], 1e-9)));
      for (int c = 0; c < 3; ++c) {
        std::vector<double> plane;
        plane.reserve(size_t(x1 - x0) * size_t(y1 - y0));
        for (int y = y0; y < y1; ++y)
          for (int x = x0; x < x1; ++x)
            plane.push_back(channels[size_t(c)][size_t(y) * width + x]);
        double m = trimmed_linear_mean(std::move(plane));
        if (std::isfinite(m))
          tile.channel_log_means[size_t(c)] = std::log2(std::max(m, 1e-9));
      }
      out.tiles.push_back(std::move(tile));
    }
  }
  return out;
}
Json sidecar_json(const Analysis &analysis, const std::string &source_sha256) {
  const Grid &g = analysis.grid;
  std::vector<Json> tiles;
  tiles.reserve(analysis.tiles.size());
  for (const Tile &tile : analysis.tiles) {
    std::vector<Json> samples;
    samples.reserve(tile.structure_samples.size());
    for (double v : tile.structure_samples)
      samples.push_back(number(v));
    std::vector<Json> channels;
    for (const auto &value : tile.channel_log_means)
      channels.push_back(value ? number(*value) : Json());
    tiles.emplace_back(Json{
        {"center", std::vector<Json>{number(tile.center_x),
                                     number(tile.center_y)}},
        {"log_luminance",
         tile.has_log_luminance ? number(tile.log_luminance) : Json()},
        {"valid", tile.valid},
        {"saturation_fraction", number(tile.saturation_fraction)},
        {"analysis_highlight_fraction", number(tile.analysis_highlight_fraction)},
        {"structure_samples", samples},
        {"channel_log_means", channels}});
  }
  return Json{
      {"schema_version", 2},
      {"kind", "rawtl-analysis-sidecar"},
      {"source_sha256", source_sha256},
      {"analysis",
       Json{{"max_edge", kMaxEdge},
            {"downsampled_width", g.width},
            {"downsampled_height", g.height},
            {"grid_nx", g.nx},
            {"grid_ny", g.ny},
            {"roi", std::vector<Json>{number(analysis.roi.x),
                                      number(analysis.roi.y),
                                      number(analysis.roi.w),
                                      number(analysis.roi.h)}},
            {"roi_pixel_bounds",
             std::vector<Json>{g.x0, g.y0, g.x1, g.y1}},
            {"tile_layout", "row-major (j*grid_nx+i)"},
            {"analysis_domain", "gained ACEScg AP1/D60 scene-linear FP32"},
            {"luma_weights", std::vector<Json>{number(kLumaR), number(kLumaG),
                                               number(kLumaB)}},
            {"highlight_threshold_linear", number(kHighlightThreshold)},
            {"floor", number(kFloor)},
            {"saturation_mask_quality", "camera_domain"},
            {"saturation_threshold", number(kSaturationThreshold)},
            {"highlight_quality", "analysis_ap1_threshold"},
            {"structure_stride", kStructureStride},
            {"exposure_ev", number(analysis.exposure_ev)}}},
      {"tiles", tiles}};
}
} // namespace analysis
} // namespace frame
