#include "highlight.h"
#include "frame.h"
#include <algorithm>
#include <cmath>
#include <limits>

namespace frame::highlight {
namespace {
constexpr double floor_signal = 1e-5;
struct Cell {
  double weight = 0;
  std::array<double, 3> sum{}, square{};
};
struct Level {
  uint32_t width, height, step;
  std::vector<Cell> cells;
};
double reliability(double value, double ceiling, double transition) {
  const double t = std::clamp((ceiling - value) / transition, 0., 1.);
  return t * t * (3 - 2 * t);
}
std::array<double, 3> chroma(const float *p, const std::array<double, 3> &n) {
  std::array<double, 3> z{};
  double mean = 0;
  for (int c = 0; c < 3; ++c) mean += (z[c] = std::log(double(p[c]) / n[c])) / 3;
  for (auto &v : z) v -= mean;
  return z;
}
void add(Cell &a, const Cell &b) {
  a.weight += b.weight;
  for (int c = 0; c < 3; ++c) { a.sum[c] += b.sum[c]; a.square[c] += b.square[c]; }
}
}
Result reconstruct(std::span<const float> input, uint32_t width, uint32_t height,
                   std::array<double, 3> neutral, const Evidence &e, void (*cancel)()) {
  if (!width || !height || uint64_t(width) * height * 3 != input.size())
    throw Error(40, "highlight input geometry mismatch");
  const double max_neutral = *std::max_element(neutral.begin(), neutral.end());
  for (auto &n : neutral) {
    if (!std::isfinite(n) || n <= 0) throw Error(40, "invalid highlight camera neutral");
    n /= max_neutral;
  }
  Result out;
  if (!e.supported) return out;
  for (int c = 0; c < 3; ++c)
    if (!std::isfinite(e.ceiling[c]) || e.ceiling[c] <= 0 ||
        !std::isfinite(e.transition[c]) || e.transition[c] <= 0 ||
        e.transition[c] >= e.ceiling[c]) throw Error(40, "invalid highlight endpoints");
  bool affected = false;
  for (size_t i = 0; i < input.size(); ++i) {
    if (!std::isfinite(input[i])) throw Error(40, "nonfinite highlight observation");
    affected |= input[i] > e.ceiling[i % 3] - e.transition[i % 3];
  }
  if (!affected) return out;

  // Immutable multiscale sufficient statistics: recovered pixels never seed
  // another recovery. 16-pixel cells bound memory, not output resolution.
  constexpr uint32_t step = 16;
  std::vector<Level> pyramid;
  pyramid.push_back({(width + step - 1) / step, (height + step - 1) / step, step, {}});
  auto &base = pyramid.back(); base.cells.resize(size_t(base.width) * base.height);
  for (uint32_t y = 0; y < height; ++y) {
    if ((y & 63) == 0) { if (cancel) cancel(); }
    for (uint32_t x = 0; x < width; ++x) {
      const float *p = input.data() + (size_t(y) * width + x) * 3;
      bool seed = true;
      for (int c = 0; c < 3; ++c)
        seed &= p[c] > floor_signal * e.ceiling[c] && p[c] <= e.ceiling[c] - e.transition[c];
      if (!seed) continue;
      double seed_weight = 1;
      for (int c = 0; c < 3; ++c) {
        const double high = std::clamp((e.ceiling[c] - e.transition[c] - p[c]) / (4 * e.transition[c]), 0., 1.);
        const double low = std::clamp((p[c] / e.ceiling[c] - floor_signal) / floor_signal, 0., 1.);
        seed_weight *= high * high * (3 - 2 * high) * low;
      }
      if (seed_weight == 0) continue;
      const auto z = chroma(p, neutral);
      auto &cell = base.cells[size_t(y / step) * base.width + x / step];
      // Equal votes, avoiding preference for one channel's brightness/color.
      cell.weight += seed_weight;
      for (int c = 0; c < 3; ++c) { cell.sum[c] += seed_weight * z[c]; cell.square[c] += seed_weight * z[c] * z[c]; }
    }
  }
  // Finite spatial support: at most roughly 1/16 of the long edge per cell.
  const uint32_t max_step = std::max(16u, std::max(width, height) / 16);
  while (pyramid.back().step * 2 <= max_step) {
    const auto &prev = pyramid.back();
    Level next{(prev.width + 1) / 2, (prev.height + 1) / 2, prev.step * 2, {}};
    next.cells.resize(size_t(next.width) * next.height);
    for (uint32_t y = 0; y < prev.height; ++y)
      for (uint32_t x = 0; x < prev.width; ++x)
        add(next.cells[size_t(y / 2) * next.width + x / 2], prev.cells[size_t(y) * prev.width + x]);
    pyramid.push_back(std::move(next));
  }
  out.camera.assign(input.begin(), input.end());
  for (uint32_t y = 0; y < height; ++y) {
    if ((y & 31) == 0) { if (cancel) cancel(); }
    for (uint32_t x = 0; x < width; ++x) {
      const size_t at = (size_t(y) * width + x) * 3;
      const float *p = input.data() + at;
      std::array<double, 3> r{}, observed{};
      bool change = false; int usable = 0;
      for (int c = 0; c < 3; ++c) {
        r[c] = reliability(p[c], e.ceiling[c], e.transition[c]);
        change |= r[c] < 1;
        usable += r[c] > 0;
        observed[c] = std::log(std::max(double(p[c]) / neutral[c], floor_signal));
      }
      if (!change) continue;
      ++out.statistics.affected;
      if (usable) ++out.statistics.partial; else ++out.statistics.full;
      std::array<double, 3> sum{}; double mass = 0;
      for (const auto &level : pyramid) {
        const int cx = int(x / level.step), cy = int(y / level.step);
        for (int dy = -1; dy <= 1; ++dy) for (int dx = -1; dx <= 1; ++dx) {
          const int ix = cx + dx, iy = cy + dy;
          if (ix < 0 || iy < 0 || ix >= int(level.width) || iy >= int(level.height)) continue;
          const auto &cell = level.cells[size_t(iy) * level.width + ix];
          if (!cell.weight) continue;
          std::array<double, 3> mu{}; double variance = 0, mismatch = 0;
          for (int c = 0; c < 3; ++c) {
            mu[c] = cell.sum[c] / cell.weight;
            variance += std::max(0., cell.square[c] / cell.weight - mu[c] * mu[c]);
          }
          // Reliable channel ratios protect colored edges. With one reliable
          // channel the color is inherently underdetermined; variance/distance
          // must reduce confidence rather than inventing an edge observation.
          for (int c = 0; c < 3; ++c) for (int d = c + 1; d < 3; ++d) {
            const double delta = observed[c] - observed[d] - mu[c] + mu[d];
            mismatch += r[c] * r[d] * delta * delta;
          }
          const double sx = (x + .5) / level.step - (ix + .5);
          const double sy = (y + .5) / level.step - (iy + .5);
          const double occupancy = cell.weight / (double(level.step) * level.step);
          // Compact C1 kernel reaches zero before a cell leaves the 3x3 query;
          // changing grid cells must not abruptly drop a nonzero seed weight.
          const double wx = std::max(0., 1 - sx * sx / 2.25);
          const double wy = std::max(0., 1 - sy * sy / 2.25);
          const double spatial = wx * wx * wy * wy;
          const double scale_weight = double(step) / level.step;
          const double weight = occupancy * spatial * scale_weight * std::exp(-8 * mismatch - 4 * variance);
          mass += weight;
          for (int c = 0; c < 3; ++c) sum[c] += weight * mu[c];
        }
      }
      // Continuous neutral fallback. Agreement, support and distance all matter;
      // a single distant colored seed cannot tint an otherwise unknown region.
      const double support_fraction = std::clamp(mass / .5, 0., 1.);
      const double confidence = support_fraction * support_fraction * (3 - 2 * support_fraction);
      std::array<double, 3> q{};
      for (int c = 0; c < 3; ++c)
        q[c] = mass > 0 ? std::exp(confidence * sum[c] / mass) : 1.;
      if (confidence >= .5) ++out.statistics.spatial; else ++out.statistics.fallback;
      // Weighted least squares for brightness in WB-normalized camera space.
      // Only original usable channels constrain it; never the clipped values.
      double numerator = 0, denominator = 0, minimum = 0;
      for (int c = 0; c < 3; ++c) {
        const double v = double(p[c]) / neutral[c];
        numerator += r[c] * q[c] * v;
        denominator += r[c] * q[c] * q[c];
        minimum = std::max(minimum, (1 - r[c]) * std::max(0., double(p[c])) / (neutral[c] * q[c]));
      }
      // As observations vanish, choose the minimum neutral/chromatic extension
      // consistent with decoder-domain ceilings; no extrapolated texture/EV.
      const double anchor = std::max(minimum, numerator / std::max(denominator, 1e-30));
      const double strength = std::min(1., denominator / .05);
      const double level = denominator > 0 ? strength * anchor + (1 - strength) * minimum : minimum;
      for (int c = 0; c < 3; ++c) {
        if (r[c] == 1) continue; // Bit-exact reliable samples, including negatives.
        const double estimate = std::max(double(p[c]), neutral[c] * q[c] * level);
        const double value = r[c] * p[c] + (1 - r[c]) * estimate;
        if (!std::isfinite(value) || value > std::numeric_limits<float>::max())
          throw Error(40, "nonfinite highlight reconstruction");
        out.camera[at + c] = float(value);
        out.statistics.max_ratio = std::max(out.statistics.max_ratio, value / std::max(double(p[c]), floor_signal));
      }
    }
  }
  return out;
}
}
