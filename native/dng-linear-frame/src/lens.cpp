#include "lens.h"
#include "lens_interval.h"
#include "lens_exact_sum.h"
#include <algorithm>
#include <cmath>
#include <iomanip>
#include <limits>
#include <sstream>
#include <stdexcept>
namespace frame::lens {
namespace {
constexpr size_t calibration_transport_limit = 60000;
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
  if (m.kind==Kind::sony) {
    for(const auto &table:m.table) for(double v:table) if(v!=1)return false;
    return true;
  }
  if (m.kind!=Kind::perspective) return true;
  auto simple=[&](const auto &planes) {
    if(component==2) {
      const auto &g=planes[1];
      for(size_t channel:{size_t(0),size_t(2)}) {
        const auto &p=planes[channel];
        if(p.fx!=g.fx||p.fy!=g.fy||p.cx!=g.cx||p.cy!=g.cy||p.scale!=1)return false;
        for(double v:p.k)if(v!=0)return false;
      }
      return true;
    }
    const auto &p=planes[0];
    if(component==1&&p.scale!=1)return false;
    const size_t count=component==0?3:5;
    return std::all_of(p.k.begin(),p.k.begin()+count,[](double v){return v==0;});
  };
  struct Support {double weight;const std::array<Perspective,3> *planes;};
  std::vector<Support> support;
  if(m.terms.empty())support.push_back({1,&m.plane});
  else for(const auto &term:m.terms)support.push_back({term.weight,&term.plane});
  if(std::all_of(support.begin(),support.end(),[&](const auto &s){return simple(*s.planes);}))return true;
  auto input=[&](size_t i)->const Perspective& {return (*support[i].planes)[component==2?1:0];};
  const auto &reference=input(0);
  for(size_t i=1;i<support.size();++i)
    if(input(i).cx!=reference.cx||input(i).cy!=reference.cy)return false;
  // In a shared-centre image basis, all coefficients are sums of rational
  // products. Clear their positive focal denominators, then certify zero with
  // error-free expansions. No rounded accumulator (including long double) is
  // allowed to bypass model-domain validation.
  auto cancels=[&](size_t channel) {
    auto output=[&](size_t i)->const Perspective& {return (*support[i].planes)[component==2?channel:0];};
    auto coefficient=[&](int x_power,int y_power,auto numerator,bool subtract_identity=false) {
      detail::ExactSum sum;
      for(size_t i=0;i<support.size();++i) {
        auto factors=numerator(i);
        for(size_t j=0;j<support.size();++j)if(j!=i) {
          for(int k=0;k<x_power;++k)factors.push_back(input(j).fx);
          for(int k=0;k<y_power;++k)factors.push_back(input(j).fy);
        }
        sum.add(factors);
        if(subtract_identity) {
          std::vector<double> negative{-support[i].weight};
          for(size_t j=0;j<support.size();++j) {
            for(int k=0;k<x_power;++k)negative.push_back(input(j).fx);
            for(int k=0;k<y_power;++k)negative.push_back(input(j).fy);
          }
          sum.add(negative);
        }
      }
      return sum.is_zero();
    };
    if(component!=0) {
      detail::ExactSum tx,ty;
      for(size_t i=0;i<support.size();++i) {
        tx.add({support[i].weight,output(i).cx});tx.add({-support[i].weight,input(i).cx});
        ty.add({support[i].weight,output(i).cy});ty.add({-support[i].weight,input(i).cy});
      }
      if(!tx.is_zero()||!ty.is_zero())return false;
      if(!coefficient(1,0,[&](size_t i){return std::vector<double>{support[i].weight,output(i).scale,output(i).fx};},true)||
         !coefficient(0,1,[&](size_t i){return std::vector<double>{support[i].weight,output(i).scale,output(i).fy};},true))return false;
    }
    constexpr int choose[4][4]={{1,0,0,0},{1,1,0,0},{1,2,1,0},{1,3,3,1}};
    for(int n=1;n<=3;++n)for(int j=0;j<=n;++j) {
      if(!coefficient(2*j+(component==0?0:1),2*(n-j),[&](size_t i){
          std::vector<double> f{support[i].weight,output(i).k[n-1],double(choose[n][j])};
          if(component!=0){f.push_back(output(i).scale);f.push_back(output(i).fx);}return f;
        }))return false;
      if(component!=0&&!coefficient(2*j,2*(n-j)+1,[&](size_t i){return std::vector<double>{support[i].weight,output(i).scale,output(i).fy,output(i).k[n-1],double(choose[n][j])};}))return false;
    }
    if(component==0)return true;
    auto tangent=[&](int xp,int yp,bool horizontal,int index,double multiplier){
      return coefficient(xp,yp,[&](size_t i){return std::vector<double>{support[i].weight,output(i).scale,horizontal?output(i).fx:output(i).fy,output(i).k[index],multiplier};});
    };
    return tangent(2,0,true,4,3)&&tangent(1,1,true,3,2)&&tangent(0,2,true,4,1)&&
           tangent(2,0,false,3,1)&&tangent(1,1,false,4,2)&&tangent(0,2,false,3,3);
  };
  return component==2?cancels(0)&&cancels(2):cancels(0);
}
double sample_table(const std::vector<double> &t, double r) {
  if (t.empty())
    return 1;
  const double position =
      std::clamp(r * (t.size() - 1) - .5, 0., double(t.size() - 1));
  size_t lo = size_t(position), hi = std::min(lo + 1, t.size() - 1);
  return t[lo] + (t[hi] - t[lo]) * (position - lo);
}
domain::Point sample_table(const std::vector<double> &t, domain::Point r) {
  if (t.empty()) return 1;
  const double at = r.value * (t.size() - 1) - .5;
  if (at <= 0) return t.front();
  if (at >= t.size() - 1) return t.back();
  const size_t lo = size_t(at);
  const double derivative = (t[lo + 1] - t[lo]) * (t.size() - 1);
  return {sample_table(t, r.value), derivative * r.dx, derivative * r.dy};
}
domain::Jet sample_table(const std::vector<double> &t, const domain::Jet &r) {
  using domain::Interval;
  if (t.empty())
    return 1;
  double low = sample_table(t, r.value.lo), high = sample_table(t, r.value.hi);
  if (low > high)
    std::swap(low, high);
  double slope_low = std::numeric_limits<double>::infinity(), slope_high = -slope_low;
  const double n = t.size() - 1;
  auto slope = [&](double v) {
    slope_low = std::min(slope_low, v);
    slope_high = std::max(slope_high, v);
  };
  if (r.value.lo <= .5 / n || r.value.hi >= (n + .5) / n)
    slope(0);
  for (size_t i = 0; i + 1 < t.size(); ++i) {
    const double a = (i + .5) / n, b = (i + 1.5) / n;
    if (r.value.lo <= b && r.value.hi >= a) {
      slope((t[i + 1] - t[i]) * n);
      low = std::min({low, t[i], t[i + 1]});
      high = std::max({high, t[i], t[i + 1]});
    }
  }
  if (slope_low == 0 && slope_high == 0)
    return {Interval(domain::down(low), domain::up(high)), 0, 0};
  Interval derivative(domain::down(slope_low), domain::up(slope_high));
  return {Interval(domain::down(low), domain::up(high)), derivative * r.dx,
          derivative * r.dy};
}
template <class T>
std::array<T, 2> perspective_offset(const Perspective &p, const Perspective &g,
                                   T x, T y, uint32_t w, uint32_t h) {
  const double edge=std::max(w,h), cx=p.cx*w-.5, cy=p.cy*h-.5,
               gx=g.cx*w-.5, gy=g.cy*h-.5;
  const auto u=(x-gx)/(g.fx*edge), v=(y-gy)/(g.fy*edge);
  const auto r2=domain::squared(u)+domain::squared(v);
  const auto radial=r2*(p.k[0]+r2*(p.k[1]+r2*p.k[2]));
  const auto common=radial+2*(p.k[3]*v+p.k[4]*u);
  // Form the displacement directly. Averaging absolute coordinates can round
  // an exactly cancelling field just outside x=0/y=0 and manufacture black.
  return {(cx-gx)+(p.fx/g.fx*p.scale-1)*(x-gx)+p.fx*edge*p.scale*(u*common+p.k[4]*r2),
          (cy-gy)+(p.fy/g.fy*p.scale-1)*(y-gy)+p.fy*edge*p.scale*(v*common+p.k[3]*r2)};
}
template <class T>
std::array<T, 2> perspective_map(const Model &m, T x, T y,
                                uint32_t w, uint32_t h, size_t channel, bool relative) {
  auto evaluate=[&](const auto &planes) {
    const auto &p=planes[relative?channel:0], &g=planes[relative?1:0];
    return perspective_offset(p,g,x,y,w,h);
  };
  std::array<T,2> delta{};
  if (m.terms.empty()) delta=evaluate(m.plane);
  else for (const auto &term:m.terms) {
    const auto q=evaluate(term.plane);
    delta[0]=delta[0]+term.weight*q[0];
    delta[1]=delta[1]+term.weight*q[1];
  }
  return {x+delta[0],y+delta[1]};
}
template <class T>
std::array<T, 2> source_map(const Model *dist, const Model *ca, size_t channel,
                          T x, T y, uint32_t w, uint32_t h) {
  const double cx = (w - 1) * .5, cy = (h - 1) * .5,
               half_diagonal = std::hypot(w, h) * .5;
  auto apply = [&](const Model &m, size_t plane, bool relative) {
    if (m.kind == Kind::sony) {
      const auto f = sample_table(m.table[plane],
          domain::radius(x - cx, y - cy) / half_diagonal);
      return std::array<T, 2>{x + (x - cx) * (f - 1), y + (y - cy) * (f - 1)};
    }
    return perspective_map(m, x, y, w, h, plane, relative);
  };
  if (dist) {
    auto q = apply(*dist, 0, false);
    x = q[0]; y = q[1];
  }
  if (ca && channel != 1) {
    auto q = apply(*ca, channel, true);
    x = q[0]; y = q[1];
  }
  return {x, y};
}
void validate_geometry(const Model *dist, const Model *ca, uint32_t w, uint32_t h, void (*cancel)()) {
  if (!dist && !ca)
    return;
  struct Box { double x0, x1, y0, y1; unsigned depth; };
  std::vector<Box> pending{{0, double(w - 1), 0, double(h - 1), 0}};
  size_t visited = 0;
  while (!pending.empty()) {
    if (cancel) cancel();
    const auto b = pending.back(); pending.pop_back();
    bool certified = true;
    for (size_t channel = 0; channel < 3; ++channel) {
      domain::Jet x{{b.x0, b.x1}, 1, 0}, y{{b.y0, b.y1}, 0, 1};
      const auto q = source_map(dist, ca, channel, x, y, w, h);
      const auto determinant = q[0].dx * q[1].dy - q[0].dy * q[1].dx;
      certified &= std::isfinite(determinant.lo) && std::isfinite(determinant.hi) &&
                   determinant.lo > 0;
    }
    if (certified)
      continue;
    if (++visited > 100000 || b.depth >= 20)
      throw std::runtime_error("lens geometry domain cannot be certified non-folding");
    // Adaptive interval subdivision, not a sparse Jacobian sample. Failure to
    // certify is explicit and never silently treated as a usable model.
    if (b.x1 - b.x0 >= b.y1 - b.y0) {
      const double mid = (b.x0 + b.x1) * .5;
      pending.push_back({b.x0, mid, b.y0, b.y1, b.depth + 1});
      pending.push_back({mid, b.x1, b.y0, b.y1, b.depth + 1});
    } else {
      const double mid = (b.y0 + b.y1) * .5;
      pending.push_back({b.x0, b.x1, b.y0, mid, b.depth + 1});
      pending.push_back({b.x0, b.x1, mid, b.y1, b.depth + 1});
    }
  }
}
double sinc(double x) {
  constexpr double pi = 3.14159265358979323846;
  if (x == 0) return 1;
  if (x == std::trunc(x)) return 0;
  return std::sin(pi * x) / (pi * x);
}
struct Tap { size_t index; double weight; };
struct TapSet {
  std::array<Tap, 49> entries{};
  size_t count = 0;
  const Tap *begin() const { return entries.data(); }
  const Tap *end() const { return entries.data() + count; }
};
TapSet quality_taps(double position, double scale, uint32_t extent) {
  if (!std::isfinite(scale) || scale > 8)
    throw std::runtime_error("lens quality resampling footprint exceeds supported scale 8");
  scale = std::max(1., scale);
  TapSet taps;
  double total = 0;
  for (int64_t i = int64_t(std::ceil(position - 3 * scale));
       i <= int64_t(std::floor(position + 3 * scale)); ++i) {
    const double d = (i - position) / scale;
    if (std::abs(d) >= 3) continue;
    const double weight = sinc(d) * sinc(d / 3);
    if (weight == 0) continue;
    taps.entries[taps.count++] = {size_t(std::clamp<int64_t>(i, 0, extent - 1)), weight};
    total += weight;
  }
  if (!std::isfinite(total) || std::abs(total) < 1e-12)
    throw std::runtime_error("invalid lens quality sampling weights");
  for (size_t i = 0; i < taps.count; ++i) taps.entries[i].weight /= total;
  return taps;
}
float quality_sample(std::span<const float> rgb, uint32_t w, uint32_t h,
                     const std::array<domain::Point, 2> &q, size_t channel) {
  const auto xs = quality_taps(q[0].value, std::hypot(q[0].dx, q[0].dy), w),
             ys = quality_taps(q[1].value, std::hypot(q[1].dx, q[1].dy), h);
  double sum = 0, low = std::numeric_limits<double>::infinity(), high = -low;
  for (const auto &y : ys)
    for (const auto &x : xs) {
      const double value = rgb[(y.index * w + x.index) * 3 + channel];
      sum += value * x.weight * y.weight;
      low = std::min(low, value); high = std::max(high, value);
    }
  if (!std::isfinite(sum))
    throw std::runtime_error("nonfinite lens quality sample");
  // Anti-ringing uses this signed/HDR footprint's own extrema, not a display
  // range or arbitrary gain cap. Geometry-outside samples remain black above.
  return float(std::clamp(sum, low, high));
}
double attenuation_delta(const Perspective &p, double x, double y, uint32_t w,
                   uint32_t h) {
  const double edge = std::max(w, h);
  const double u = (x - (p.cx * w - .5)) / (p.fx * edge),
               v = (y - (p.cy * h - .5)) / (p.fy * edge);
  const double r2 = u * u + v * v;
  return r2 * (p.k[0] + r2 * (p.k[1] + r2 * p.k[2]));
}
double gain(const Model *m, double x, double y, uint32_t w, uint32_t h) {
  if (!m)
    return 1;
  if (m->kind == Kind::sony)
    return sample_table(m->table[0],
                        std::hypot(x - (w - 1) * .5, y - (h - 1) * .5) /
                            (std::hypot(w, h) * .5));
  double delta = 0;
  if (m->terms.empty())
    delta = attenuation_delta(m->plane[0], x, y, w, h);
  else
    for (const auto &term : m->terms)
      delta += term.weight * attenuation_delta(term.plane[0], x, y, w, h);
  const double a = 1 + delta;
  if (!std::isfinite(a) || a <= 0)
    throw std::runtime_error("invalid lens vignette attenuation");
  return 1 / a;
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
      if (m.terms.size() > 8 || (!m.terms.empty() && m.kind != Kind::perspective))
        throw std::runtime_error("invalid lens interpolation support");
      double weights = 0;
      std::vector<Perspective> planes(m.plane.begin(), m.plane.end());
      for (const auto &term : m.terms) {
        if (!std::isfinite(term.weight) || term.weight <= 0 || term.weight > 1)
          throw std::runtime_error("invalid lens interpolation weight");
        weights += term.weight;
        planes.insert(planes.end(), term.plane.begin(), term.plane.end());
      }
      if (!m.terms.empty() && std::abs(weights - 1) > 1e-12)
        throw std::runtime_error("lens interpolation weights do not sum to one");
      for (const auto &p : planes) {
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
void validate_policy(const Policy &p) {
  if (!std::isfinite(p.vignette_strength) || p.vignette_strength < 0 || p.vignette_strength > 1 ||
      (p.sampling != Sampling::bilinear && p.sampling != Sampling::quality))
    throw std::runtime_error("invalid lens delivery policy");
}
std::array<Decision, 3> decide(const Calibration &c, bool enabled, const Policy &policy) {
  validate(c);
  validate_policy(policy);
  std::array<Decision, 3> out;
  for (size_t i = 0; i < 3; ++i) {
    auto &d = out[i];
    if (!enabled || !policy.components[i] || (i == 0 && policy.vignette_strength == 0))
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
        if (i != 1 && d.status == "applied" &&
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
void validate_domain(const Calibration &c, uint32_t w, uint32_t h,
                     const Policy &policy, void (*cancel)()) {
  if (!w || !h)
    throw std::runtime_error("invalid lens domain extent");
  auto decisions = decide(c, true, policy);
  validate_geometry(decisions[1].model, decisions[2].model, w, h, cancel);
  const auto *vign = decisions[0].model;
  if (!vign || vign->kind != Kind::perspective)
    return;
  auto positive = [&](const Perspective &p) {
    const double edge = std::max(w, h), cx = p.cx * w - .5, cy = p.cy * h - .5;
    const double ux = std::max(std::abs(cx), std::abs(w - 1 - cx)) / (p.fx * edge),
                 vy = std::max(std::abs(cy), std::abs(h - 1 - cy)) / (p.fy * edge);
    const double rmax = ux * ux + vy * vy;
    // Checking the complete radius interval (including subpixel positions)
    // catches negative annuli that pixel/grid-only tests can miss.
    auto check = [&](double r) {
      if (r < 0 || r > rmax)
        return;
      const double a = 1 + r * (p.k[0] + r * (p.k[1] + r * p.k[2]));
      if (!std::isfinite(a) || a <= 0 || !std::isfinite(1 / a))
        throw std::runtime_error("invalid lens vignette attenuation in image domain");
    };
    check(0);
    check(rmax);
    const double a = 3 * p.k[2], b = 2 * p.k[1], d = p.k[0];
    if (a == 0) {
      if (b != 0)
        check(-d / b);
    } else {
      const double discriminant = b * b - 4 * a * d;
      if (!std::isfinite(discriminant))
        throw std::runtime_error("lens vignette domain polynomial overflow");
      if (discriminant >= 0) {
        const double q = -.5 * (b + std::copysign(std::sqrt(discriminant), b));
        if (q == 0)
          check(-b / (2 * a));
        else {
          check(q / a);
          check(d / q);
        }
      }
    }
  };
  if (vign->terms.empty())
    positive(vign->plane[0]);
  else
    for (const auto &term : vign->terms)
      positive(term.plane[0]);
}
std::string domain_token(uint32_t width, uint32_t height, const Policy &policy) {
  unsigned components = (policy.components[0] && policy.vignette_strength > 0 ? 1 : 0)
                        | (policy.components[1] ? 2 : 0) | (policy.components[2] ? 4 : 0);
  return std::to_string(width) + "x" + std::to_string(height) + ":" + std::to_string(components);
}
std::array<double, 4> safe_area(const Calibration &c, uint32_t w, uint32_t h,
                               bool enabled, const Policy &policy, void (*cancel)(), bool domain_certified) {
  if (!w || !h) throw std::runtime_error("invalid lens safe-area extent");
  const auto d = decide(c, enabled, policy);
  const Model *dist = d[1].model, *ca = d[2].model;
  if (!dist && !ca) return {0, 0, 1, 1};
  if (!domain_certified) validate_domain(c, w, h, policy, cancel);
  auto inside = [&](double ratio) {
    struct Box { double x0, x1, y0, y1; unsigned depth; };
    std::vector<Box> pending{{(w-1)*(.5-.5*ratio), (w-1)*(.5+.5*ratio),
                              (h-1)*(.5-.5*ratio), (h-1)*(.5+.5*ratio), 0}};
    size_t visited = 0;
    while (!pending.empty()) {
      if (cancel) cancel();
      auto b = pending.back(); pending.pop_back();
      bool certified = true;
      for (size_t channel=0; channel<3; ++channel) {
        domain::Jet x{{b.x0,b.x1},1,0}, y{{b.y0,b.y1},0,1};
        const auto q = source_map(dist,ca,channel,x,y,w,h);
        double mx=0,my=0;
        if (policy.sampling == Sampling::quality) {
          auto bound = [](domain::Interval v) { return std::max(std::abs(v.lo),std::abs(v.hi)); };
          mx=3*std::max(1.,std::hypot(bound(q[0].dx),bound(q[0].dy)));
          my=3*std::max(1.,std::hypot(bound(q[1].dx),bound(q[1].dy)));
        }
        certified &= std::isfinite(q[0].value.lo) && std::isfinite(q[0].value.hi) &&
            std::isfinite(q[1].value.lo) && std::isfinite(q[1].value.hi) &&
            std::isfinite(mx) && std::isfinite(my) && q[0].value.lo >= mx &&
            q[0].value.hi <= w-1-mx && q[1].value.lo >= my && q[1].value.hi <= h-1-my;
        const auto point = source_map(dist,ca,channel,(b.x0+b.x1)*.5,(b.y0+b.y1)*.5,w,h);
        if (point[0]<0 || point[0]>w-1 || point[1]<0 || point[1]>h-1) return false;
      }
      if (certified) continue;
      if (++visited>20000 || b.depth>=20) return false;
      if (b.x1-b.x0>=b.y1-b.y0) {
        double mid=(b.x0+b.x1)*.5;
        pending.push_back({b.x0,mid,b.y0,b.y1,b.depth+1});
        pending.push_back({mid,b.x1,b.y0,b.y1,b.depth+1});
      } else {
        double mid=(b.y0+b.y1)*.5;
        pending.push_back({b.x0,b.x1,b.y0,mid,b.depth+1});
        pending.push_back({b.x0,b.x1,mid,b.y1,b.depth+1});
      }
    }
    return true;
  };
  if (inside(1)) return {0,0,1,1};
  if (!inside(0)) throw std::runtime_error("lens mapping has no certified central safe area");
  double lo=0,hi=1;
  for (int i=0;i<20;++i) {double mid=(lo+hi)*.5; if(inside(mid))lo=mid;else hi=mid;}
  if (lo*(w-1)<1 || lo*(h-1)<1)
    throw std::runtime_error("lens safe area is smaller than two pixels");
  return {(.5-.5*lo),(.5-.5*lo),(.5+.5*lo),(.5+.5*lo)};
}
Result correct(std::span<const float> rgb, uint32_t w, uint32_t h,
               const Calibration &c, bool enabled, void (*cancel)(), const Policy &policy, bool domain_certified) {
  if (!w || !h || uint64_t(w) * h > 200000000 ||
      rgb.size() != size_t(w) * h * 3)
    throw std::runtime_error("invalid lens RGB extent");
  Result out;
  out.decisions = decide(c, enabled, policy);
  if (enabled && !domain_certified)
    validate_domain(c, w, h, policy, cancel);
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
        const double full_gain = gain(vign, x, y, w, h);
        const double g = policy.vignette_strength == 1 ? full_gain : std::pow(full_gain, policy.vignette_strength);
        if (x == 0 && y == 0) out.gain_min = out.gain_max = g;
        out.gain_min = std::min(out.gain_min, g);
        out.gain_max = std::max(out.gain_max, g);
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
  for (uint32_t y = 0; y < h; ++y) {
    if (cancel)
      cancel();
    for (uint32_t x = 0; x < w; ++x)
      for (size_t ch = 0; ch < 3; ++ch) {
        const auto position = source_map(dist, ca, ch, double(x), double(y), w, h);
        const double sx = position[0], sy = position[1];
        auto &value = out.pixels[(size_t(y) * w + x) * 3 + ch];
        if (!std::isfinite(sx) || !std::isfinite(sy))
          throw std::runtime_error("nonfinite lens map");
        // Fixed output extent; no implicit auto-crop or edge replication.
        if (sx < 0 || sx > w - 1 || sy < 0 || sy > h - 1) {
          value = 0;
          ++out.outside_samples;
          continue;
        }
        if (policy.sampling == Sampling::quality) {
          const auto q = source_map(dist, ca, ch, domain::Point(double(x), 1, 0),
                                    domain::Point(double(y), 0, 1), w, h);
          value = quality_sample(rgb, w, h, q, ch);
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
  o << std::setprecision(17) << "rawtl-lens-calibration/2 " << c.collected
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
      auto write_planes = [&](const auto &planes) {
        for (const auto &p : planes) {
          o << p.fx << ' ' << p.fy << ' ' << p.cx << ' ' << p.cy << ' ' << p.scale << ' ';
          for (double v : p.k)
            o << v << ' ';
        }
      };
      write_planes(m.plane);
      o << m.terms.size() << ' ';
      for (const auto &term : m.terms) {
        o << term.weight << ' ';
        write_planes(term.plane);
      }
    }
  auto text=o.str();
  if (text.size()>calibration_transport_limit)
    throw std::runtime_error("lens calibration exceeds transport bound");
  return text;
}
Calibration deserialize(const std::string &text) {
  if (text.size() > calibration_transport_limit)
    throw std::runtime_error("lens calibration exceeds transport bound");
  std::istringstream in(text);
  std::string header;
  int flag = -1;
  Calibration c;
  in >> header >> flag;
  if (header != "rawtl-lens-calibration/2" || (flag != 0 && flag != 1))
    throw std::runtime_error("unsupported lens calibration contract; run Prepare again");
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
      auto read_planes = [&](auto &planes) {
        for (auto &p : planes) {
          in >> p.fx >> p.fy >> p.cx >> p.cy >> p.scale;
          for (double &v : p.k)
            in >> v;
        }
      };
      read_planes(m.plane);
      size_t support = 99;
      in >> support;
      if (support > 8)
        throw std::runtime_error("invalid lens interpolation support");
      m.terms.resize(support);
      for (auto &term : m.terms) {
        in >> term.weight;
        read_planes(term.plane);
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
Json report(const Calibration &c, const Result &r, bool enabled, const Policy &policy) {
  std::vector<Json> components;
  for (size_t i = 0; i < 3; ++i) {
    const auto &d = r.decisions[i];
    components.emplace_back(Json{{"component", names[i]},
                                 {"status", d.status},
                                 {"requested", enabled && policy.components[i] && (i != 0 || policy.vignette_strength > 0)},
                                 {"upstream", upstream_name(c.upstream[i])},
                                 {"source", d.source},
                                 {"fingerprint", d.fingerprint}});
  }
  return Json{{"revision", 2},
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
              {"vignette_strength", policy.vignette_strength},
              {"gain_min", r.gain_min}, {"gain_max", r.gain_max},
              {"resampling", policy.sampling == Sampling::bilinear ? "bilinear" : "quality"},
              {"extent_policy", "fixed; outside samples black; no auto-crop"},
              {"upstream_reversible", false}};
}
} // namespace frame::lens
