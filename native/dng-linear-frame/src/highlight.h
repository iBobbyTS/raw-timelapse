#pragma once
#include <array>
#include <cstdint>
#include <span>
#include <string>
#include <vector>

namespace frame::highlight {
// Endpoint semantics are supplied by decode, never inferred from a histogram.
struct Evidence {
  bool supported = false;
  std::array<double, 3> ceiling{1, 1, 1};
  std::array<double, 3> transition{0, 0, 0};
  std::string source = "unavailable";
};
struct Statistics {
  uint64_t affected = 0, partial = 0, full = 0, spatial = 0, fallback = 0;
  double max_ratio = 1;
};
struct Result {
  // Empty means exact pass-through; original observations are never overwritten.
  std::vector<float> camera;
  Statistics statistics;
};
Result reconstruct(std::span<const float> camera, uint32_t width, uint32_t height,
                   std::array<double, 3> neutral, const Evidence &evidence,
                   void (*cancel)() = nullptr);
}
