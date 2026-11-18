#include "lens_profile.h"
#include "frame.h"
#include "lens_xml.h"
#include <algorithm>
#include <charconv>
#include <cctype>
#include <cmath>
#include <cstdlib>
#include <functional>
#include <libxml/xmlreader.h>
#include <set>
#include <sstream>
namespace frame::lens {
namespace {
const std::string camera_ns =
    "http://ns.adobe.com/photoshop/1.0/camera-profile";
using Fields = std::map<std::string, std::string>;
std::string normalized(std::string s) {
  std::string r;
  for (unsigned char c : s)
    if (std::isalnum(c))
      r += char(std::tolower(c));
  return r;
}
Fields fields(xmlNode *n) {
  Fields f;
  auto take = [&](xmlNode *node) {
    for (auto *a = node->properties; a; a = a->next)
      if (a->ns && a->ns->href &&
          camera_ns == reinterpret_cast<const char *>(a->ns->href)) {
        auto *s = xmlNodeListGetString(node->doc, a->children, 1);
        f[reinterpret_cast<const char *>(a->name)] =
            s ? reinterpret_cast<const char *>(s) : "";
        xmlFree(s);
      }
    for (auto *c = node->children; c; c = c->next)
      if (c->type == XML_ELEMENT_NODE && xml::ns(c) == camera_ns &&
          c->children && c->children->type == XML_TEXT_NODE)
        f[xml::name(c)] = xml::value(c);
  };
  take(n);
  for (auto *c = n->children; c; c = c->next)
    if (c->type == XML_ELEMENT_NODE && xml::name(c) == "Description")
      take(c);
  return f;
}
double number(const Fields &f, const std::string &k, double fallback = 0) {
  auto it = f.find(k);
  if (it == f.end())
    return fallback;
  size_t end = 0;
  double v;
  try {
    v = std::stod(it->second, &end);
  } catch (...) {
    throw std::runtime_error("invalid LCP " + k);
  }
  if (end != it->second.size() || !std::isfinite(v))
    throw std::runtime_error("invalid LCP " + k);
  return v;
}
std::string text(const Fields &f, const std::string &k) {
  auto it = f.find(k);
  return it == f.end() ? "" : it->second;
}
std::optional<std::array<double, 4>> lens_spec(const std::string &value) {
  std::istringstream input(value);
  auto positive = [](std::string_view token) {
    double number = 0;
    const auto end = token.data() + token.size();
    const auto result = std::from_chars(token.data(), end, number);
    return result.ec == std::errc{} && result.ptr == end && std::isfinite(number) && number > 0
        ? number : 0.;
  };
  std::array<double, 4> spec;
  for (double &part : spec) {
    std::string token;
    if (!(input >> token))
      return std::nullopt;
    const auto slash = token.find('/');
    const std::string_view text = token;
    const auto numerator = positive(text.substr(0, slash));
    const auto denominator = slash == std::string::npos ? 1 : positive(text.substr(slash + 1));
    if (numerator == 0 || denominator == 0)
      return std::nullopt;
    part = numerator / denominator;
    if (!std::isfinite(part))
      return std::nullopt;
  }
  std::string extra;
  if (input >> extra)
    return std::nullopt;
  return spec;
}
xmlNode *descendant(xmlNode *n, const std::string &name) {
  for (auto *c = n->children; c; c = c->next)
    if (c->type == XML_ELEMENT_NODE) {
      if (xml::name(c) == name && xml::ns(c) == camera_ns)
        return c;
      if (auto *r = descendant(c, name))
        return r;
    }
  return nullptr;
}
Perspective perspective(xmlNode *n, double focal, double crop, bool vignette) {
  auto f = fields(n);
  Perspective p;
  p.fx = number(f, "FocalLengthX", focal * crop / 35.);
  p.fy = number(f, "FocalLengthY", p.fx);
  p.cx = number(f, "ImageXCenter", .5);
  p.cy = number(f, "ImageYCenter", .5);
  p.scale = number(f, "ScaleFactor", 1);
  for (int i = 0; i < 3; ++i)
    p.k[i] =
        number(f, (vignette ? "VignetteModelParam" : "RadialDistortParam") +
                      std::to_string(i + 1));
  p.k[3] = number(f, "TangentialDistortParam1");
  p.k[4] = number(f, "TangentialDistortParam2");
  return p;
}
// Read only the header for indexing; do not load thousands of calibration
// tables.
Fields identity_header(const std::filesystem::path &path, std::vector<std::string> &aliases) {
  auto *r = xmlReaderForFile(path.c_str(), nullptr,
                             XML_PARSE_NONET | XML_PARSE_NOERROR |
                                 XML_PARSE_NOWARNING);
  if (!r)
    throw std::runtime_error("cannot open LCP header: " + path.string());
  struct Guard {
    xmlTextReader *r;
    ~Guard() { xmlFreeTextReader(r); }
  } guard{r};
  Fields f;
  int count = 0, status, alternate_depth = -1;
  while ((status = xmlTextReaderRead(r)) == 1) {
    if (++count > 4096)
      throw std::runtime_error("LCP header exceeds bound");
    int type = xmlTextReaderNodeType(r);
    if (type == XML_READER_TYPE_DOCUMENT_TYPE ||
        type == XML_READER_TYPE_ENTITY_REFERENCE)
      throw std::runtime_error("LCP DTD/entities forbidden");
    if (type == XML_READER_TYPE_END_ELEMENT &&
        xmlTextReaderDepth(r) == alternate_depth)
      alternate_depth = -1;
    if (type != XML_READER_TYPE_ELEMENT)
      continue;
    auto local = reinterpret_cast<const char *>(xmlTextReaderConstLocalName(r));
    const auto *element_ns = xmlTextReaderConstNamespaceUri(r);
    if (std::string(local) == "AlternateLensNames" && element_ns &&
        camera_ns == reinterpret_cast<const char *>(element_ns))
      alternate_depth = xmlTextReaderDepth(r);
    else if (alternate_depth >= 0 && std::string(local) == "li" && element_ns &&
             std::string(reinterpret_cast<const char *>(element_ns)) ==
                 "http://www.w3.org/1999/02/22-rdf-syntax-ns#") {
      auto *value = xmlTextReaderReadString(r);
      if (value) {
        aliases.emplace_back(reinterpret_cast<const char *>(value));
        xmlFree(value);
      }
      if (aliases.size() > 128)
        throw std::runtime_error("LCP alias count exceeds bound");
    }
    if (std::string(local) == "PerspectiveModel" ||
        std::string(local) == "FisheyeModel")
      break;
    if (xmlTextReaderMoveToFirstAttribute(r) == 1) {
      do {
        const auto *ns = xmlTextReaderConstNamespaceUri(r);
        if (ns && camera_ns == reinterpret_cast<const char *>(ns))
          f[reinterpret_cast<const char *>(xmlTextReaderConstLocalName(r))] =
              reinterpret_cast<const char *>(xmlTextReaderConstValue(r));
      } while (xmlTextReaderMoveToNextAttribute(r) == 1);
      xmlTextReaderMoveToElement(r);
    }
    const auto *ns = xmlTextReaderConstNamespaceUri(r);
    if (ns && camera_ns == reinterpret_cast<const char *>(ns)) {
      std::string key = local;
      if (key == "Make" || key == "Model" || key == "Lens" ||
          key == "CameraRawProfile") {
        auto *v = xmlTextReaderReadString(r);
        if (v) {
          f[key] = reinterpret_cast<const char *>(v);
          xmlFree(v);
        }
      }
    }
  }
  if (status < 0)
    throw std::runtime_error("malformed LCP header: " + path.string());
  return f;
}
Model blend(const Model &a, const Model &b, double t) {
  if (t <= 0)
    return a;
  if (t >= 1)
    return b;
  Model m = a;
  m.terms.clear();
  auto append = [&](const Model &endpoint, double weight) {
    if (endpoint.terms.empty())
      m.terms.push_back({weight, endpoint.plane});
    else
      for (auto term : endpoint.terms) {
        term.weight *= weight;
        m.terms.push_back(std::move(term));
      }
  };
  // Interpolate attenuation or complete pixel-coordinate maps, not independent
  // coefficients whose units change with focal normalization and principal point.
  append(a, 1 - t);
  append(b, t);
  if (m.terms.size() > 8)
    throw std::runtime_error("LCP interpolation exceeds trilinear support");
  return m;
}
// Nested clamped interpolation handles ragged focal/aperture/focus grids
// without inventing extrapolated coefficients. Missing focus uses the farthest
// sample.
Model interpolate(std::vector<const ProfileSample *> rows, int component,
                  int dimension, const std::array<double, 3> &target) {
  if (rows.empty())
    return {};
  auto axis = [&](const ProfileSample *s) {
    return dimension == 0   ? std::log(s->focal)
           : dimension == 1 ? s->aperture
                            : std::log(std::max(.001, s->focus));
  };
  if (dimension == 3)
    return rows.front()->model[component];
  std::map<double, std::vector<const ProfileSample *>> groups;
  for (auto *s : rows)
    groups[axis(s)].push_back(s);
  double x = target[dimension];
  auto hi = groups.lower_bound(x);
  if (hi == groups.end())
    hi = std::prev(groups.end());
  auto lo = hi;
  if (hi != groups.begin() && hi->first > x)
    --lo;
  auto a = interpolate(lo->second, component, dimension + 1, target);
  if (lo == hi)
    return a;
  auto b = interpolate(hi->second, component, dimension + 1, target);
  return blend(a, b, (x - lo->first) / (hi->first - lo->first));
}
} // namespace
Profile read_profile(const std::filesystem::path &path) {
  const auto bytes = xml::read(path);
  auto d = xml::parse(bytes);
  Profile p;
  p.path = std::filesystem::canonical(path).string();
  Sha256 hash;
  hash.update(bytes.data(), bytes.size());
  p.fingerprint = hash.hex();
  std::function<void(xmlNode *)> visit = [&](xmlNode *n) {
    for (; n; n = n->next) {
      if (n->type != XML_ELEMENT_NODE)
        continue;
      auto f = fields(n);
      if (f.count("Lens") && f.count("FocalLength")) {
        if (p.lens.empty()) {
          p.make = text(f, "Make");
          p.camera = text(f, "Model");
          p.lens = text(f, "Lens");
          p.lens_info = text(f, "LensInfo");
          p.pretty_name = text(f, "LensPrettyName");
          if (auto *alternates = descendant(n, "AlternateLensNames")) {
            std::function<void(xmlNode *)> names = [&](xmlNode *node) {
              for (; node; node = node->next) {
                if (node->type != XML_ELEMENT_NODE)
                  continue;
                if (xml::name(node) == "li" && xml::ns(node) ==
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                  p.aliases.push_back(xml::value(node));
                else
                  names(node->children);
              }
            };
            names(alternates->children);
            if (p.aliases.size() > 128)
              throw std::runtime_error("LCP alias count exceeds bound");
          }
          p.raw = text(f, "CameraRawProfile") == "True";
          p.crop = number(f, "SensorFormatFactor", 1);
          double h = number(f, "ImageLength");
          if (h > 0)
            p.aspect = number(f, "ImageWidth") / h;
        }
        if (text(f, "Make") != p.make || text(f, "Lens") != p.lens)
          throw std::runtime_error("mixed identities in LCP");
        ProfileSample s;
        s.focal = number(f, "FocalLength");
        s.aperture = number(f, "ApertureValue");
        s.focus = number(f, "FocusDistance");
        if (s.focal <= 0 || s.focus < 0)
          throw std::runtime_error("invalid LCP sample coordinates");
        auto *base = descendant(n, "PerspectiveModel");
        if (!base) {
          if (descendant(n, "FisheyeModel"))
            p.unsupported = true;
          continue;
        }
        const double version = number(fields(base), "Version", 2);
        if (version != 1 && version != 2) {
          p.unsupported = true;
          continue;
        }
        auto make = [&](xmlNode *node, bool vign) {
          Model m;
          m.kind = Kind::perspective;
          m.source = p.path;
          m.fingerprint = p.fingerprint;
          m.plane.fill(perspective(node, s.focal, p.crop, vign));
          return m;
        };
        s.model[1] = make(base, false);
        if (auto *v = descendant(base, "VignetteModel"))
          s.model[0] = make(v, true);
        auto *r = descendant(base, "ChromaticRedGreenModel"),
             *g = descendant(base, "ChromaticGreenModel"),
             *b = descendant(base, "ChromaticBlueGreenModel");
        if (r && g && b) {
          s.model[2] = make(g, false);
          s.model[2].plane[0] = perspective(r, s.focal, p.crop, false);
          s.model[2].plane[2] = perspective(b, s.focal, p.crop, false);
        }
        p.samples.push_back(s);
        if (p.samples.size() > 10000)
          throw std::runtime_error("LCP sample count exceeds bound");
      } else
        visit(n->children);
    }
  };
  visit(xmlDocGetRootElement(d.get()));
  if (p.crop <= 0 || !std::isfinite(p.crop) || p.aspect <= 0)
    throw std::runtime_error("invalid LCP sensor geometry");
  return p;
}
std::array<Model, 3> interpolate_profile(const Profile &p, double focal,
                                         double fnumber, double focus,
                                         double crop) {
  std::array<Model, 3> out;
  if (focal <= 0 || crop <= 0)
    return out;
  for (int i = 0; i < 3; ++i) {
    std::vector<const ProfileSample *> rows;
    double far = 0;
    for (const auto &s : p.samples)
      if (s.model[i].kind != Kind::none) {
        rows.push_back(&s);
        far = std::max(far, s.focus);
      }
    if (i == 0 && fnumber <= 0)
      continue;
    out[i] =
        interpolate(rows, i, 0,
                    {std::log(focal), fnumber > 0 ? 2 * std::log2(fnumber) : 0,
                     std::log(std::max(.001, focus > 0 ? focus : far))});
    double ratio = crop / p.crop;
    auto adjust = [&](auto &planes) {
      for (auto &a : planes) {
        a.fx *= ratio;
        a.fy *= ratio;
        a.cx = .5 + (a.cx - .5) * ratio;
        a.cy = .5 + (a.cy - .5) * ratio;
      }
    };
    adjust(out[i].plane);
    for (auto &term : out[i].terms)
      adjust(term.plane);
  }
  return out;
}
std::vector<std::filesystem::path> default_profile_roots() {
  std::vector<std::filesystem::path> roots;
#ifdef _WIN32
  if (auto *v = std::getenv("ProgramData"))
    roots.emplace_back(std::filesystem::path(v) /
                       "Adobe/CameraRaw/LensProfiles/1.0");
  if (auto *v = std::getenv("ProgramFiles"))
    roots.emplace_back(
        std::filesystem::path(v) /
        "Adobe/Adobe Lightroom Classic/Resources/LensProfiles/1.0");
#else
  if (auto *v = std::getenv("HOME"))
    roots.emplace_back(
        std::filesystem::path(v) /
        "Library/Application Support/Adobe/CameraRaw/LensProfiles/1.0");
  roots.emplace_back(
      "/Library/Application Support/Adobe/CameraRaw/LensProfiles/1.0");
  roots.emplace_back("/Applications/Adobe Lightroom Classic/Adobe Lightroom "
                     "Classic.app/Contents/Resources/LensProfiles/1.0");
#endif
  return roots;
}
ProfileCatalog::ProfileCatalog(std::vector<std::filesystem::path> roots)
    : roots_(std::move(roots)) {}
const Profile *ProfileCatalog::match(Calibration &c, double crop,
                                     double aspect, std::string &reason,
                                     const std::string &lens_info) {
  const auto make = normalized(c.make);
  const auto camera = normalized(c.camera);
  // Use body identity only for verified fixed-lens models. The same optics
  // specification can describe several lenses on an interchangeable body.
  const bool fixed = c.lens.empty() && make == "dji" && camera == "ac004";
  const auto original_spec = fixed ? lens_spec(lens_info) : std::nullopt;
  if ((c.lens.empty() && !original_spec) || c.make.empty() || c.focal <= 0) {
    reason = "missing-lens-or-focal-metadata";
    return {};
  }
  if (!indexed_) {
    std::set<std::filesystem::path> seen;
    for (const auto &root : roots_) {
      if (!std::filesystem::exists(root))
        continue;
      for (const auto &entry :
           std::filesystem::recursive_directory_iterator(root)) {
        check_cancelled();
        if (!entry.is_regular_file() ||
            normalized(entry.path().extension().string()) != "lcp")
          continue;
        auto path = std::filesystem::canonical(entry.path());
        if (!seen.insert(path).second)
          continue;
        std::vector<std::string> aliases;
        auto f = identity_header(path, aliases);
        if (text(f, "CameraRawProfile") != "True")
          continue;
        index_[normalized(text(f, "Make")) + "/body/" +
               normalized(text(f, "Model"))].push_back(path);
        aliases.push_back(text(f, "Lens"));
        std::set<std::string> keys;
        for (const auto &name : aliases) {
          auto key = normalized(name);
          if (!key.empty() && keys.insert(key).second)
            index_[normalized(text(f, "Make")) + "/" + key].push_back(path);
        }
      }
    }
    indexed_ = true;
  }
  auto it = index_.find(make + (fixed ? "/body/" + camera : "/" + normalized(c.lens)));
  if (it == index_.end()) {
    reason = "no-matching-lcp";
    return {};
  }
  std::vector<const Profile *> candidates;
  int best = -1;
  std::set<std::string> hashes;
  for (const auto &path : it->second) {
    auto stored = parsed_.find(path);
    if (stored == parsed_.end())
      stored = parsed_.emplace(path, read_profile(path)).first;
    const auto &p = stored->second;
    const auto lens_key = normalized(c.lens);
    const bool named = normalized(p.lens) == lens_key ||
        std::any_of(p.aliases.begin(), p.aliases.end(), [&](const auto &alias) {
          return normalized(alias) == lens_key;
        });
    const auto profile_spec = fixed ? lens_spec(p.lens_info) : std::nullopt;
    const bool fixed_match = original_spec && profile_spec &&
        normalized(p.camera) == camera &&
        std::equal(original_spec->begin(), original_spec->end(), profile_spec->begin(),
                   [](double a, double b) { return std::abs(a - b) < .001; });
    if (!p.raw || p.unsupported || p.samples.empty() ||
        normalized(p.make) != make || !(fixed ? fixed_match : named))
      continue;
    // Calibration axes are sensor axes, not EXIF display orientation. Without
    // an explicit axis transform, a transposed aspect is not compatible.
    if (crop <= 0 || std::abs(aspect - p.aspect) > .03)
      continue;
    int score = camera == normalized(p.camera) ? 1 : 0;
    if (score > best) {
      candidates.clear();
      hashes.clear();
      best = score;
    }
    if (score == best && hashes.insert(p.fingerprint).second)
      candidates.push_back(&p);
  }
  if (candidates.empty()) {
    reason = "unsupported-lcp-or-unknown-sensor-geometry";
    return {};
  }
  if (candidates.size() > 1) {
    reason = "ambiguous-lcp-match";
    return {};
  }
  const auto &selected = *candidates.front();
  if (fixed) {
    c.lens = selected.pretty_name.empty() ? selected.lens : selected.pretty_name;
    reason = "match=fixed-camera-lens-spec; ";
  } else {
    reason = normalized(selected.lens) == normalized(c.lens)
        ? "match=primary-lens; " : "match=official-alias; ";
  }
  reason += "lcp-image-field-log-focal-apex-aperture-log-focus; missing-focus=far";
  return &selected;
}
std::array<Model, 3> ProfileCatalog::resolve(Calibration &c, double crop,
                                             double aspect, std::string &reason,
                                             const std::string &lens_info) {
  const auto *profile = match(c, crop, aspect, reason, lens_info);
  return profile ? interpolate_profile(*profile, c.focal, c.aperture, c.focus, crop)
                 : std::array<Model, 3>{};
}
} // namespace frame::lens
