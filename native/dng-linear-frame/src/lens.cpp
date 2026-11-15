#include "lens.h"
#include <algorithm>
#include <cmath>
#include <iomanip>
#include <limits>
#include <sstream>
#include <stdexcept>
namespace frame::lens {
namespace {
const char *names[] = {"vignette", "distortion", "ca"};
const char *upstream_name(Upstream u) {
  switch (u) {
  case Upstream::unknown:
    return "unknown";
  case Upstream::not_reported:
    return "not-reported";
  case Upstream::applied:
    return "already-applied";
  case Upstream::sdk:
    return "sdk-owned";
  case Upstream::embedded:
    return "embedded-opcode-owned";
  }
  throw std::runtime_error("invalid lens upstream state");
}
bool identity(const Model &m, size_t component) {
  if (m.kind == Kind::sony) {
    for (const auto &t : m.table)
      for (double v : t)
        if (v != 1)
          return false;
  } else if (m.kind == Kind::perspective) {
    if (component == 2) {
      const auto &g = m.plane[1];
      for (size_t channel : {size_t(0), size_t(2)}) {
        const auto &p = m.plane[channel];
        if (p.fx != g.fx || p.fy != g.fy || p.cx != g.cx || p.cy != g.cy ||
            p.scale != 1)
          return false;
        for (double v : p.k)
          if (v != 0)
            return false;
      }
    } else {
      const auto &p = m.plane[0];
      if (component == 1 && p.scale != 1)
        return false;
      for (double v : p.k)
        if (v != 0)
          return false;
    }
  }
  return true;
}
double sample_table(const std::vector<double> &t, double r) {
  if (t.empty())
    return 1;
  const double position =
      std::clamp(r * (t.size() - 1) - .5, 0., double(t.size() - 1));
  size_t lo = size_t(position), hi = std::min(lo + 1, t.size() - 1);
  return t[lo] + (t[hi] - t[lo]) * (position - lo);
}
std::array<double, 2> map(const Perspective &p, double x, double y, uint32_t w,
                          uint32_t h) {
  const double edge = std::max(w, h), fx = p.fx * edge, fy = p.fy * edge;
  const double cx = p.cx * w - .5, cy = p.cy * h - .5;
  const double u = (x - cx) / fx, v = (y - cy) / fy, r2 = u * u + v * v;
  const double radial = 1 + r2 * (p.k[0] + r2 * (p.k[1] + r2 * p.k[2]));
  const double common = radial + 2 * (p.k[3] * v + p.k[4] * u);
  return {cx + fx * p.scale * (u * common + p.k[4] * r2),
          cy + fy * p.scale * (v * common + p.k[3] * r2)};
}
double gain(const Model *m, double x, double y, uint32_t w, uint32_t h) {
  if (!m)
    return 1;
  if (m->kind == Kind::sony)
    return sample_table(m->table[0],
                        std::hypot(x - (w - 1) * .5, y - (h - 1) * .5) /
                            (std::hypot(w, h) * .5));
  const auto &p = m->plane[0];
  const double edge = std::max(w, h);
  const double u = (x - (p.cx * w - .5)) / (p.fx * edge),
               v = (y - (p.cy * h - .5)) / (p.fy * edge);
  const double r2 = u * u + v * v;
  const double attenuation = 1 + r2 * (p.k[0] + r2 * (p.k[1] + r2 * p.k[2]));
  if (!std::isfinite(attenuation) || attenuation <= 0)
    throw std::runtime_error("invalid lens vignette attenuation");
  return 1 / attenuation;
}
} // namespace
void validate(const Calibration &c) {
  if (c.make.size() > 4096 || c.camera.size() > 4096 || c.lens.size() > 4096 ||
      c.evidence.size() > 32768 || c.selection.size() > 4096)
    throw std::runtime_error("lens calibration text exceeds bounds");
  for (double v : {c.focal, c.aperture, c.focus})
    if (!std::isfinite(v) || v < 0)
      throw std::runtime_error("invalid lens capture value");
  for (auto u : c.upstream)
    (void)upstream_name(u);
  for (const auto *set : {&c.embedded, &c.fallback})
    for (const auto &m : *set) {
      if (m.kind != Kind::none && m.kind != Kind::sony &&
          m.kind != Kind::perspective)
        throw std::runtime_error("unknown lens model");
      if (m.source.size() > 4096 || m.fingerprint.size() > 128)
        throw std::runtime_error("lens provenance exceeds bounds");
      if (m.kind != Kind::none && m.source.empty())
        throw std::runtime_error("lens model requires provenance");
      for (const auto &t : m.table) {
        if (t.size() > 16 || (m.kind == Kind::sony && t.size() < 2))
          throw std::runtime_error("invalid Sony lens knot count");
        for (double v : t)
          if (!std::isfinite(v) || v <= 0 || v > 64)
            throw std::runtime_error("invalid Sony correction factor");
      }
      for (const auto &p : m.plane) {
        if (!std::isfinite(p.fx) || !std::isfinite(p.fy) || p.fx <= 0 ||
            p.fy <= 0 || !std::isfinite(p.cx) || !std::isfinite(p.cy) ||
            p.cx < 0 || p.cx > 1 || p.cy < 0 || p.cy > 1 ||
            !std::isfinite(p.scale) || p.scale <= 0)
          throw std::runtime_error("invalid lens model geometry");
        for (double v : p.k)
          if (!std::isfinite(v))
            throw std::runtime_error("nonfinite lens coefficient");
      }
    }
}
std::array<Decision, 3> decide(const Calibration &c, bool enabled) {
  validate(c);
  std::array<Decision, 3> out;
  for (size_t i = 0; i < 3; ++i) {
    auto &d = out[i];
    if (!enabled)
      d.status = "off";
    else if (!c.collected)
      d.status = "metadata-not-collected";
    else if (c.upstream[i] == Upstream::applied ||
             c.upstream[i] == Upstream::sdk ||
             c.upstream[i] == Upstream::embedded)
      d.status = upstream_name(c.upstream[i]);
    else if (c.upstream[i] == Upstream::unknown)
      d.status = "unknown-upstream";
    else {
      const Model *m =
          c.embedded[i].kind != Kind::none ? &c.embedded[i] : &c.fallback[i];
      if (m->kind == Kind::none) {
        d.source = c.embedded[i].source;
        d.status =
            d.source.empty() ? "no-usable-model" : "unsupported-embedded";
      } else {
        d.source = m->source;
        d.fingerprint = m->fingerprint;
        d.status = identity(*m, i) ? "identity" : "applied";
        if (i == 0 && d.status == "applied" &&
            (c.upstream[1] == Upstream::applied ||
             c.upstream[1] == Upstream::sdk ||
             c.upstream[1] == Upstream::embedded))
          d.status = "upstream-geometry-unmapped";
        if (d.status == "applied")
          d.model = m;
      }
    }
  }
  return out;
}
Result correct(std::span<const float> rgb, uint32_t w, uint32_t h,
               const Calibration &c, bool enabled, void (*cancel)()) {
  if (!w || !h || uint64_t(w) * h > 200000000 ||
      rgb.size() != size_t(w) * h * 3)
    throw std::runtime_error("invalid lens RGB extent");
  Result out;
  out.decisions = decide(c, enabled);
  const Model *vign = out.decisions[0].model, *dist = out.decisions[1].model,
              *ca = out.decisions[2].model;
  if (!vign && !dist && !ca)
    return out;
  // Correct attenuation in source coordinates before the single geometric
  // sample.
  std::vector<float> shaded;
  if (vign) {
    shaded.resize(rgb.size());
    for (uint32_t y = 0; y < h; ++y) {
      if (cancel)
        cancel();
      for (uint32_t x = 0; x < w; ++x) {
        const double g = gain(vign, x, y, w, h);
        for (size_t ch = 0; ch < 3; ++ch) {
          float value = float(rgb[(size_t(y) * w + x) * 3 + ch] * g);
          if (!std::isfinite(value))
            throw std::runtime_error("lens vignette overflow");
          shaded[(size_t(y) * w + x) * 3 + ch] = value;
        }
      }
    }
    rgb = shaded;
  }
  if (!dist && !ca) {
    out.pixels = std::move(shaded);
    return out;
  }
  out.pixels.resize(rgb.size());
  const double cx = (w - 1) * .5, cy = (h - 1) * .5,
               radius = std::hypot(w, h) * .5;
  for (uint32_t y = 0; y < h; ++y) {
    if (cancel)
      cancel();
    for (uint32_t x = 0; x < w; ++x)
      for (size_t ch = 0; ch < 3; ++ch) {
        double sx = x, sy = y;
        if (dist) {
          if (dist->kind == Kind::sony) {
            double f = sample_table(dist->table[0],
                                    std::hypot(sx - cx, sy - cy) / radius);
            sx = cx + (sx - cx) * f;
            sy = cy + (sy - cy) * f;
          } else {
            auto p = map(dist->plane[0], sx, sy, w, h);
            sx = p[0];
            sy = p[1];
          }
        }
        if (ca && ch != 1) {
          if (ca->kind == Kind::sony) {
            double f = sample_table(ca->table[ch],
                                    std::hypot(sx - cx, sy - cy) / radius);
            sx = cx + (sx - cx) * f;
            sy = cy + (sy - cy) * f;
          } else {
            // CA green is a coordinate system, not another base distortion.
            const auto &g = ca->plane[1], &p = ca->plane[ch];
            double edge = std::max(w, h);
            double u = (sx - (g.cx * w - .5)) / (g.fx * edge),
                   v = (sy - (g.cy * h - .5)) / (g.fy * edge);
            auto q = map(p, (p.cx * w - .5) + u * p.fx * edge,
                         (p.cy * h - .5) + v * p.fy * edge, w, h);
            sx = q[0];
            sy = q[1];
          }
        }
        auto &value = out.pixels[(size_t(y) * w + x) * 3 + ch];
        if (!std::isfinite(sx) || !std::isfinite(sy))
          throw std::runtime_error("nonfinite lens map");
        // Fixed output extent; no implicit auto-crop or edge replication.
        if (sx < 0 || sx > w - 1 || sy < 0 || sy > h - 1) {
          value = 0;
          ++out.outside_samples;
          continue;
        }
        size_t x0 = size_t(sx), y0 = size_t(sy),
               x1 = std::min(x0 + 1, size_t(w - 1)),
               y1 = std::min(y0 + 1, size_t(h - 1));
        double a = sx - x0, b = sy - y0;
        auto at = [&](size_t xx, size_t yy) {
          return rgb[(yy * w + xx) * 3 + ch];
        };
        value = float((1 - b) * ((1 - a) * at(x0, y0) + a * at(x1, y0)) +
                      b * ((1 - a) * at(x0, y1) + a * at(x1, y1)));
        if (!std::isfinite(value))
          throw std::runtime_error("nonfinite lens sample");
      }
  }
  return out;
}
std::string serialize(const Calibration &c) {
  validate(c);
  std::ostringstream o;
  o << std::setprecision(17) << "rawtl-lens-calibration/1 " << c.collected
    << ' ';
  for (const auto *s : {&c.make, &c.camera, &c.lens, &c.evidence, &c.selection})
    o << std::quoted(*s) << ' ';
  o << c.focal << ' ' << c.aperture << ' ' << c.focus << ' ';
  for (auto u : c.upstream)
    o << int(u) << ' ';
  for (const auto *set : {&c.embedded, &c.fallback})
    for (const auto &m : *set) {
      o << int(m.kind) << ' ' << std::quoted(m.source) << ' '
        << std::quoted(m.fingerprint) << ' ';
      for (const auto &t : m.table) {
        o << t.size() << ' ';
        for (double v : t)
          o << v << ' ';
      }
      for (const auto &p : m.plane) {
        o << p.fx << ' ' << p.fy << ' ' << p.cx << ' ' << p.cy << ' ' << p.scale
          << ' ';
        for (double v : p.k)
          o << v << ' ';
      }
    }
  return o.str();
}
Calibration deserialize(const std::string &text) {
  if (text.size() > 60000)
    throw std::runtime_error("lens calibration exceeds transport bound");
  std::istringstream in(text);
  std::string header;
  int flag = -1;
  Calibration c;
  in >> header >> flag;
  if (header != "rawtl-lens-calibration/1" || (flag != 0 && flag != 1))
    throw std::runtime_error("unsupported lens calibration contract");
  c.collected = flag;
  for (auto *s : {&c.make, &c.camera, &c.lens, &c.evidence, &c.selection})
    in >> std::quoted(*s);
  in >> c.focal >> c.aperture >> c.focus;
  for (auto &u : c.upstream) {
    int n = -1;
    in >> n;
    if (n < 0 || n > 4)
      throw std::runtime_error("invalid lens upstream");
    u = Upstream(n);
  }
  for (auto *set : {&c.embedded, &c.fallback})
    for (auto &m : *set) {
      int n = -1;
      in >> n;
      if (n < 0 || n > 2)
        throw std::runtime_error("invalid lens model kind");
      m.kind = Kind(n);
      in >> std::quoted(m.source) >> std::quoted(m.fingerprint);
      for (auto &t : m.table) {
        size_t count = 99;
        in >> count;
        if (count > 16)
          throw std::runtime_error("invalid lens knots");
        t.resize(count);
        for (double &v : t)
          in >> v;
      }
      for (auto &p : m.plane) {
        in >> p.fx >> p.fy >> p.cx >> p.cy >> p.scale;
        for (double &v : p.k)
          in >> v;
      }
    }
  if (!in)
    throw std::runtime_error("truncated lens calibration");
  in >> std::ws;
  if (!in.eof())
    throw std::runtime_error("trailing lens calibration data");
  validate(c);
  return c;
}
Json report(const Calibration &c, const Result &r, bool enabled) {
  std::vector<Json> components;
  for (size_t i = 0; i < 3; ++i) {
    const auto &d = r.decisions[i];
    components.emplace_back(Json{{"component", names[i]},
                                 {"status", d.status},
                                 {"upstream", upstream_name(c.upstream[i])},
                                 {"source", d.source},
                                 {"fingerprint", d.fingerprint}});
  }
  return Json{{"revision", 1},
              {"requested", enabled},
              {"collected", c.collected},
              {"make", c.make},
              {"camera", c.camera},
              {"lens", c.lens},
              {"focal_length", c.focal},
              {"f_number", c.aperture},
              {"focus_distance", c.focus},
              {"selection", c.selection},
              {"evidence", c.evidence},
              {"components", components},
              {"outside_samples", double(r.outside_samples)},
              {"extent_policy", "fixed; outside samples black; no auto-crop"},
              {"upstream_reversible", false}};
}
} // namespace frame::lens
