#pragma once
// Independent camera-linear lens correction contract. No SDK, graph, colour
// recipe or highlight-reconstruction dependencies.
#include "json.h"
#include <array>
#include <cstdint>
#include <span>
#include <string>
#include <vector>
namespace frame::lens {
enum class Component { vignette, distortion, ca };
enum class Upstream { unknown, not_reported, applied, sdk, embedded };
enum class Kind { none, sony, perspective };
struct Perspective {
  double fx = 1, fy = 1, cx = .5, cy = .5, scale = 1;
  std::array<double, 5> k{};
};
// A bounded convex combination in image coordinates. Each term retains its
// own normalization, including the green coordinate system for relative CA.
struct PerspectiveTerm {
  double weight = 1;
  std::array<Perspective, 3> plane;
};
struct Model {
  Kind kind = Kind::none;
  std::string source, fingerprint;
  // Sony: correction factors at (i+.5)/(N-1) of the half diagonal.
  std::array<std::vector<double>, 3> table;
  // LCP: normalised focal lengths (long edge), centres (width/height).
  std::array<Perspective, 3> plane;
  std::vector<PerspectiveTerm> terms;
};
struct Calibration {
  bool collected = false;
  std::string make, camera, lens, evidence, selection;
  double focal = 0, aperture = 0, focus = 0;
  std::array<Upstream, 3> upstream{};
  std::array<Model, 3> embedded, fallback;
};
enum class Sampling { bilinear, quality };
struct Policy {
  std::array<bool, 3> components{true, true, true};
  double vignette_strength = 1;
  // quality is the product default; bilinear remains the explicit low-cost path.
  Sampling sampling = Sampling::quality;
};
void validate_policy(const Policy &);
struct Request {
  const Calibration *calibration = nullptr;
  bool enabled = false;
  Policy policy;
};
struct Decision {
  std::string status, source, fingerprint;
  const Model *model = nullptr;
};
struct Result {
  // Empty means exactly unchanged. The caller retains ownership of
  // observations.
  std::vector<float> pixels;
  std::array<Decision, 3> decisions;
  uint64_t outside_samples = 0;
  double gain_min = 1, gain_max = 1;
};
std::array<Decision, 3> decide(const Calibration &, bool enabled, const Policy & = {});
Result correct(std::span<const float> rgb, uint32_t width, uint32_t height,
               const Calibration &, bool enabled, void (*cancel)() = nullptr,
               const Policy & = {});
// Normalized pixel-centre rectangle certified for all three channels and the
// selected sampler footprint. The output/sequence layer owns its application.
std::array<double, 4> safe_area(const Calibration &, uint32_t width, uint32_t height,
                                bool enabled, const Policy & = {}, void (*cancel)() = nullptr);
void validate(const Calibration &);
void validate_domain(const Calibration &, uint32_t width, uint32_t height,
                     const Policy & = {}, void (*cancel)() = nullptr);
std::string serialize(const Calibration &);
Calibration deserialize(const std::string &);
Json report(const Calibration &, const Result &, bool enabled, const Policy & = {});
} // namespace frame::lens
