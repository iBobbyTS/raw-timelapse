#include "lens.h"
#include <cmath>
#include <iostream>
#include <limits>
#include <stdexcept>
using namespace frame::lens;
namespace {
void require(bool b, const char *message) {
  if (!b)
    throw std::runtime_error(message);
}
void near(double a, double b, double eps = 1e-5) {
  require(std::abs(a - b) < eps, "numeric truth mismatch");
}
Calibration ready() {
  Calibration c;
  c.collected = true;
  c.upstream.fill(Upstream::not_reported);
  return c;
}
Model perspective() {
  Model m;
  m.kind = Kind::perspective;
  m.source = "synthetic:truth";
  return m;
}
void rejects(const auto &f) {
  bool caught = false;
  try {
    f();
  } catch (const std::exception &) {
    caught = true;
  }
  require(caught, "malformed contract accepted");
}
} // namespace
int main() {
  try {
    const uint32_t w = 65, h = 49;
    std::vector<float> rgb(size_t(w) * h * 3);
    for (size_t i = 0; i < rgb.size(); i += 3) {
      rgb[i] = -.25;
      rgb[i + 1] = 2;
      rgb[i + 2] = .5;
    }
    auto c = ready();
    auto r = correct(rgb, w, h, c, true);
    require(r.pixels.empty(), "missing model changed pixels");
    c.fallback[0] = perspective();
    c.fallback[0].plane[0].k[0] = -.4;
    r = correct(rgb, w, h, c, true);
    require(!r.pixels.empty(), "vignette not applied");
    size_t center = (size_t(h / 2) * w + w / 2) * 3;
    near(r.pixels[center], -.25);
    near(r.pixels[center + 1], 2);
    double r2 = (32. * 32 + 24. * 24) / (65. * 65), g = 1 / (1 - .4 * r2);
    near(r.pixels[0], -.25 * g);
    near(r.pixels[1], 2 * g);
    near(r.pixels[2], .5 * g);
    require(correct(rgb, w, h, c, false).pixels.empty(), "off altered pixels");
    for (auto state : {Upstream::applied, Upstream::sdk, Upstream::unknown}) {
      c.upstream[0] = state;
      require(correct(rgb, w, h, c, true).pixels.empty(),
              "unsafe correction reapplied");
    }
    c.upstream[0] = Upstream::not_reported;
    c.embedded[0] = perspective();
    r = correct(rgb, w, h, c, true);
    require(r.pixels.empty() && r.decisions[0].status == "identity",
            "identity embedded did not win over LCP");
    // An affine ramp is exactly reconstructed by bilinear resampling; this
    // proves mapping direction, normalization, red/blue independence and signed
    // HDR.
    c = ready();
    c.fallback[1] = perspective();
    c.fallback[1].plane[0].k[0] = -.2;
    for (uint32_t y = 0; y < h; ++y)
      for (uint32_t x = 0; x < w; ++x)
        for (int ch = 0; ch < 3; ++ch)
          rgb[(size_t(y) * w + x) * 3 + ch] = float(x + 2 * y + ch - 20.);
    r = correct(rgb, w, h, c, true);
    uint32_t x = 10, y = 12;
    double dx = x - 32., dy = y - 24., rad = (dx * dx + dy * dy) / (65. * 65),
           f = 1 - .2 * rad;
    for (int ch = 0; ch < 3; ++ch)
      near(r.pixels[(size_t(y) * w + x) * 3 + ch],
           32 + dx * f + 2 * (24 + dy * f) + ch - 20., 2e-5);
    c = ready();
    c.fallback[2] = perspective();
    c.fallback[2].plane[0].scale = .98;
    c.fallback[2].plane[2].scale = 1.02;
    r = correct(rgb, w, h, c, true);
    for (int ch = 0; ch < 3; ++ch) {
      double scale = ch == 0 ? .98 : ch == 2 ? 1.02 : 1;
      near(r.pixels[(size_t(y) * w + x) * 3 + ch],
           32 + dx * scale + 2 * (24 + dy * scale) + ch - 20., 2e-5);
    }
    require(r.outside_samples > 0, "out-of-frame samples not counted");
    near(r.pixels[2], 0);
    for (size_t i = 1; i < rgb.size(); i += 3)
      near(r.pixels[i], rgb[i]);
    // CA centre/focal changes are not identity even with zero radial
    // coefficients.
    c = ready();
    c.fallback[2] = perspective();
    c.fallback[2].plane[0].cx = .51;
    require(!correct(rgb, w, h, c, true).pixels.empty(),
            "CA centre shift mistaken for identity");
    c = ready();
    c.fallback[0] = perspective();
    c.fallback[0].plane[0].k[0] = -.2;
    c.upstream[1] = Upstream::applied;
    r = correct(rgb, w, h, c, true);
    require(r.pixels.empty() &&
                r.decisions[0].status == "upstream-geometry-unmapped",
            "source vignette applied in unknown warped coordinates");
    // Sony knots use a half-knot origin, and vignette must be sampled before
    // warp.
    c = ready();
    Model sony;
    sony.kind = Kind::sony;
    sony.source = "embedded:synthetic";
    for (auto &t : sony.table)
      t.assign(16, 1);
    for (size_t i = 0; i < 16; ++i)
      sony.table[0][i] = 1 + i * .02;
    c.embedded[0] = sony;
    r = correct(rgb, w, h, c, true);
    near(r.pixels[center], rgb[center]);
    const auto serialized = serialize(c);
    require(serialize(deserialize(serialized)) == serialized,
            "calibration transport not lossless");
    for (auto bad :
         {std::string(""), std::string("rawtl-lens-calibration/2"),
          serialized + " garbage", serialized.substr(0, serialized.size() / 2)})
      rejects([&] { deserialize(bad); });
    c.embedded[0].table[0][0] = std::numeric_limits<double>::infinity();
    rejects([&] { correct(rgb, w, h, c, true); });
    rejects([&] { correct(rgb, 0, h, ready(), true); });
    rejects([&] {
      correct(std::span<const float>(rgb.data(), 3), w, h, ready(), true);
    });
    c = ready();
    c.fallback[1] = perspective();
    c.fallback[1].plane[0].k[0] = -.1;
    rejects([&] {
      correct(rgb, w, h, c, true,
              [] { throw std::runtime_error("cancelled"); });
    });
    std::cout << "PASS lens decisions, known gain/warp/CA truth, signed HDR, "
                 "identity priority, bounds, cancellation and transport"
              << std::endl;
    return 0;
  } catch (const std::exception &e) {
    std::cerr << e.what() << std::endl;
    return 1;
  }
}
