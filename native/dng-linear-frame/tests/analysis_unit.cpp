// AC1 measurement-math tests on synthetic buffers plus semantic separation
// checks for the camera-domain mask and delivered-domain clipping series.
#include "analysis.h"
#include <array>
#include <cmath>
#include <functional>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <string>
#include <vector>
using namespace frame;
namespace {
void check(bool ok, const std::string &what) {
  if (!ok)
    throw std::runtime_error("assertion failed: " + what);
}
void close(double a, double b, double tolerance, const std::string &what) {
  if (!(std::abs(a - b) <= tolerance))
    throw std::runtime_error("assertion failed: " + what + " (" +
                             std::to_string(a) + " vs " + std::to_string(b) +
                             ")");
}
const Mat identity{{{1, 0, 0}, {0, 1, 0}, {0, 0, 1}}};
using Pixel = std::array<float, 3>;
struct Sample {
  Decoded d;
  std::vector<float> scene;
};
Sample build(int w, int h, const std::function<Pixel(int, int)> &camera,
             const std::function<Pixel(int, int)> &scene) {
  Sample s;
  s.d.width = uint32_t(w);
  s.d.height = uint32_t(h);
  s.d.camera_to_ap1 = identity;
  s.d.camera.resize(size_t(w) * size_t(h) * 3);
  s.scene.resize(size_t(w) * size_t(h) * 3);
  for (int y = 0; y < h; ++y)
    for (int x = 0; x < w; ++x) {
      auto c = camera(x, y);
      auto v = scene(x, y);
      size_t i = (size_t(y) * w + size_t(x)) * 3;
      for (int k = 0; k < 3; ++k) {
        s.d.camera[i + size_t(k)] = c[size_t(k)];
        s.scene[i + size_t(k)] = v[size_t(k)];
      }
    }
  return s;
}
Sample uniform(int w, int h, Pixel camera, double gain = 1.0,
               const Mat &matrix = identity) {
  return build(
      w, h, [&](int, int) { return camera; },
      [&](int, int) {
        Vec scene = multiply(matrix, Vec{camera[0], camera[1], camera[2]});
        return Pixel{float(scene[0] * gain), float(scene[1] * gain),
                     float(scene[2] * gain)};
      });
}
void expect_roi_too_small(int w, int h, const analysis::Roi &roi) {
  try {
    analysis::make_grid(w, h, roi);
    check(false, "ROI_TOO_SMALL expected");
  } catch (const Error &e) {
    check(e.code == 4, "ROI_TOO_SMALL exit code");
  }
}
} // namespace
int main() {
  using namespace analysis;
  // 5%/5% trimmed linear mean, including negatives that are NOT pre-clamped.
  std::vector<double> ramp(100);
  for (size_t i = 0; i < ramp.size(); ++i)
    ramp[i] = double(i);
  close(trimmed_linear_mean(ramp), 49.5, 1e-12, "trimmed ramp mean");
  close(trimmed_linear_mean(std::vector<double>(10, 0.18)), 0.18, 1e-15,
        "trimmed constant mean");
  check(std::isnan(trimmed_linear_mean({})), "empty trimmed mean is NaN");
  check(std::isnan(trimmed_linear_mean({1.0, std::numeric_limits<double>::quiet_NaN()})),
        "nonfinite trimmed mean is NaN");
  std::vector<double> noise(100, -0.01);
  noise.insert(noise.end(), 900, 0.11);
  close(trimmed_linear_mean(noise), 93.0 / 900.0, 1e-12,
        "negative noise trimmed, not pre-clamped");

  // Grid geometry: reduction, ROI partition, centers, and the tile gates.
  auto grid = make_grid(4000, 2256, {});
  check(grid.width == 1024 && grid.height == 578, "long edge <= 1024");
  check(grid.nx == 12 && grid.ny == 8, "full grid");
  auto scaled = make_grid(512, 256, {});
  check(scaled.width == 512 && scaled.height == 256, "identity geometry");
  check(scaled.xs.size() == 13 && scaled.xs.front() == 0 &&
            scaled.xs.back() == 512,
        "x boundaries");
  check(scaled.ys.size() == 9 && scaled.ys.front() == 0 &&
            scaled.ys.back() == 256,
        "y boundaries");
  auto roi = make_grid(512, 256, {0.5, 0.25, 0.5, 0.25});
  check(roi.x0 == 256 && roi.x1 == 512 && roi.y0 == 64 && roi.y1 == 128,
        "ROI pixel bounds");
  check(roi.nx == 12 && roi.ny == 4, "ROI reduced grid");
  expect_roi_too_small(512, 256, {0, 0, 0.01, 0.01});
  expect_roi_too_small(64, 64, {0, 0, 0.25, 0.25});
  try {
    make_grid(64, 64, {0, 0, 1.2, 1});
    check(false, "invalid ROI expected");
  } catch (const Error &e) {
    check(e.code == 3, "invalid ROI exit code");
  }

  // Deterministic integer-weighted BOX downsample: identity and scaled paths.
  std::vector<double> flat{1, 2, 3, 4};
  auto same = box_downsample(2, 2, 2, 2,
                             [&](size_t i) { return flat[i]; });
  for (size_t i = 0; i < flat.size(); ++i)
    close(same[i], flat[i], 0, "identity downsample");
  std::vector<double> row{1, 2, 3, 4, 5};
  auto half = box_downsample(5, 1, 2, 1, [&](size_t i) { return row[i]; });
  close(half[0], 1.8, 1e-15, "fractional box left");
  close(half[1], 4.2, 1e-15, "fractional box right");
  auto quad = box_downsample(4, 4, 2, 2, [](size_t i) { return double(i); });
  close(quad[0], 2.5, 1e-15, "2x2 box top-left");
  close(quad[1], 4.5, 1e-15, "2x2 box top-right");
  close(quad[2], 10.5, 1e-15, "2x2 box bottom-left");
  close(quad[3], 12.5, 1e-15, "2x2 box bottom-right");

  // Saturation occupancy gate: <= 0.005 camera-domain occupancy stays valid.
  for (int count = 1; count <= 2; ++count) {
    auto sample = build(
        64, 64,
        [&](int x, int y) {
          bool hot = y == 0 && x < count;
          Pixel p{0.5f, 0.5f, 0.5f};
          if (hot)
            p[0] = 0.99f;
          return p;
        },
        [](int, int) { return Pixel{0.5f, 0.5f, 0.5f}; });
    auto result = measure(sample.d, sample.scene, {}, 0);
    check(result.tiles.size() == 16, "4x4 grid");
    if (count == 1) {
      check(result.tiles[0].valid, "one hot pixel is below the gate");
      close(result.tiles[0].saturation_fraction, 1.0 / 256.0, 1e-15,
            "one hot pixel occupancy");
    } else {
      check(!result.tiles[0].valid, "two hot pixels reject the tile");
      close(result.tiles[0].saturation_fraction, 2.0 / 256.0, 1e-15,
            "two hot pixel occupancy");
    }
  }
  auto saturated = uniform(64, 64, Pixel{0.99f, 0.99f, 0.99f});
  auto all_hot = measure(saturated.d, saturated.scene, {}, 0);
  for (const auto &tile : all_hot.tiles) {
    check(!tile.valid, "fully saturated tiles are invalid");
    close(tile.saturation_fraction, 1.0, 0, "full saturation occupancy");
    check(!tile.has_log_luminance, "invalid tiles have no log luminance");
  }

  // Floor gate: mean must exceed max(1e-5, 8/16384), strictly.
  auto at_floor = uniform(64, 64, Pixel{0.5f, 0.5f, 0.5f});
  for (float &v : at_floor.scene)
    v = float(kFloor);
  auto floor_result = measure(at_floor.d, at_floor.scene, {}, 0);
  for (const auto &tile : floor_result.tiles)
    check(!tile.valid, "mean equal to floor is invalid");
  auto above_floor = at_floor;
  for (float &v : above_floor.scene)
    v = std::nextafter(float(kFloor), 1.0f);
  auto above_result = measure(above_floor.d, above_floor.scene, {}, 0);
  for (const auto &tile : above_result.tiles)
    check(tile.valid, "mean above floor is valid");

  // Structure stride sampling and channel means (identity color transform).
  auto plain = uniform(64, 64, Pixel{0.25f, 0.25f, 0.25f});
  auto plain_result = measure(plain.d, plain.scene, {}, 0);
  check(plain_result.tiles.size() == 16, "tile count");
  for (const auto &tile : plain_result.tiles) {
    check(tile.valid, "uniform .25 tiles valid");
    close(tile.log_luminance, -2.0, 1e-12, "uniform log luminance");
    check(tile.structure_samples.size() == 16, "::4 structure count");
    for (double v : tile.structure_samples)
      close(v, -2.0, 1e-12, "structure sample log2");
    for (int c = 0; c < 3; ++c)
      close(*tile.channel_log_means[size_t(c)], -2.0, 1e-12,
            "channel log mean");
    close(tile.analysis_highlight_fraction, 0.0, 0, "no delivered clipping");
  }
  // Channel-mean floor: log2(max(mean, 1e-9)).
  auto blue = uniform(64, 64, Pixel{0.0f, 0.5f, 0.5f});
  auto blue_result = measure(blue.d, blue.scene, {}, 0);
  close(*blue_result.tiles[0].channel_log_means[0], std::log2(1e-9), 1e-12,
        "channel mean floor");

  // Highlight threshold preserves the legacy gate numerically without quantizing pixels.
  float exact = float(kHighlightThreshold);
  check(analysis_highlight(8.0f) && !analysis_highlight(-0.01f),
        "highlight predicate accepts unbounded signed FP32");
  check(analysis_highlight(exact), "exact threshold code counts");
  check(!analysis_highlight(std::nextafter(exact, 0.0f)),
        "one float step below threshold does not count");

  // EV equivariance plus two-series separation (camera mask vs delivered clip).
  auto dark = uniform(64, 64, Pixel{0.2f, 0.2f, 0.2f});
  auto dark_up = uniform(64, 64, Pixel{0.2f, 0.2f, 0.2f}, std::sqrt(2.0));
  auto dark_down = uniform(64, 64, Pixel{0.2f, 0.2f, 0.2f}, std::sqrt(0.5));
  auto dark0 = measure(dark.d, dark.scene, {}, 0);
  auto dark_up_result = measure(dark_up.d, dark_up.scene, {}, 0.5);
  auto dark_down_result = measure(dark_down.d, dark_down.scene, {}, -0.5);
  for (size_t i = 0; i < dark0.tiles.size(); ++i) {
    check(dark0.tiles[i].valid && dark_up_result.tiles[i].valid,
          "unsaturated tiles valid at both EVs");
    close(dark_up_result.tiles[i].log_luminance -
              dark0.tiles[i].log_luminance,
          0.5, 1e-6, "EV equivariance");
    close(dark_down_result.tiles[i].log_luminance -
              dark0.tiles[i].log_luminance,
          -0.5, 1e-6, "negative EV equivariance");
    close(dark_up_result.tiles[i].saturation_fraction,
          dark0.tiles[i].saturation_fraction, 0,
          "saturation mask is EV invariant");
  }
  auto near_white = uniform(64, 64, Pixel{0.7f, 0.7f, 0.7f});
  auto near_white_up =
      uniform(64, 64, Pixel{0.7f, 0.7f, 0.7f}, std::sqrt(2.0));
  auto nw0 = measure(near_white.d, near_white.scene, {}, 0);
  auto nw_up = measure(near_white_up.d, near_white_up.scene, {}, 0.5);
  for (size_t i = 0; i < nw0.tiles.size(); ++i) {
    close(nw0.tiles[i].analysis_highlight_fraction, 0.0, 0, "no clip at 0 EV");
    close(nw_up.tiles[i].analysis_highlight_fraction, 1.0, 0,
          "+0.5 EV clips near-white tiles");
    close(nw_up.tiles[i].saturation_fraction, 0.0, 0,
          "camera mask unaffected by gain");
  }
  // Any delivered channel at or above the code threshold counts.
  auto mixed = build(
      64, 64, [](int, int) { return Pixel{0.9f, 0.9f, 0.9f}; },
      [](int, int) { return Pixel{0.0f, 0.0f, float(16056.5 / 16384.0)}; });
  auto mixed_result = measure(mixed.d, mixed.scene, {}, 0);
  close(mixed_result.tiles[0].analysis_highlight_fraction, 1.0, 0,
        "single clipped channel occupies the tile");

  auto sidecar = sidecar_json(plain_result, "abc");
  check(sidecar.text.find("\"saturation_mask_quality\":\"camera_domain\"") !=
            std::string::npos,
        "sidecar mask label");
  check(sidecar.text.find("\"schema_version\":2") != std::string::npos,
        "sidecar schema");
  check(sidecar.text.find("quantum_linear") == std::string::npos &&
        sidecar.text.find("output_clip") == std::string::npos,
        "analysis does not claim integer precision or final output clipping");
  check(sidecar.text.find("analysis_ap1_threshold") != std::string::npos,
        "analysis-only highlight contract");
  std::cout << "analysis measurement, downsample, gate and EV checks passed\n";
  return 0;
}
