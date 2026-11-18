#include "lens_metadata.h"
#include "lens_identity.h"
#include "frame.h"
#include "lens_profile.h"
#include "lens_xml.h"
#include <algorithm>
#include <cmath>
#include <cctype>
#include <iomanip>
#include <map>
#include <set>
#include <sstream>
#include <unistd.h>
namespace frame::lens {
namespace {
using Tags = std::map<std::string, std::string>;
std::string get(const Tags &t, const std::string &key) {
  if (t.count(key + ".duplicate.1"))
    throw std::runtime_error("conflicting consumed lens metadata: " + key);
  auto i = t.find(key);
  return i == t.end() ? "" : i->second;
}
std::string first(const Tags &t, std::initializer_list<const char *> keys) {
  for (auto k : keys) {
    auto s = get(t, k);
    if (!s.empty())
      return s;
  }
  return "";
}
double numeric(const std::string &s) {
  if (s.empty())
    return 0;
  size_t end = 0;
  double v;
  try {
    v = std::stod(s, &end);
  } catch (...) {
    return 0;
  }
  return end == s.size() && std::isfinite(v) && v > 0 ? v : 0;
}
std::map<std::string, Tags> metadata(const std::string &text) {
  auto d = xml::parse(text);
  std::map<std::string, Tags> result;
  for (auto *n = xmlDocGetRootElement(d.get())->children; n; n = n->next) {
    if (n->type != XML_ELEMENT_NODE || xml::name(n) != "Description")
      continue;
    auto *about =
        xmlGetNsProp(n, BAD_CAST "about",
                     BAD_CAST "http://www.w3.org/1999/02/22-rdf-syntax-ns#");
    if (!about)
      throw std::runtime_error("ExifTool record lacks source identity");
    std::string source = reinterpret_cast<const char *>(about);
    xmlFree(about);
    Tags tags;
    for (auto *c = n->children; c; c = c->next) {
      if (c->type != XML_ELEMENT_NODE)
        continue;
      std::string group = c->ns && c->ns->prefix
                              ? reinterpret_cast<const char *>(c->ns->prefix)
                              : "";
      std::string key = group + ":" + xml::name(c), value = xml::value(c);
      if (key == "ExifTool:Error")
        throw std::runtime_error("ExifTool: " + value);
      auto [it, ok] = tags.emplace(key, value);
      if (!ok && key == "ExifTool:Warning")
        it->second += "; " + value;
      else if (!ok && it->second != value) {
        // ExifTool -a legitimately emits multiple records (e.g. CR3 Track4).
        // Preserve them as evidence. Scalar uniqueness is checked only when a
        // field is actually consumed, never by dropping unrelated metadata.
        size_t duplicate = 1;
        while (tags.count(key + ".duplicate." + std::to_string(duplicate)))
          ++duplicate;
        tags.emplace(key + ".duplicate." + std::to_string(duplicate), value);
      }
    }
    if (!result.emplace(source, std::move(tags)).second)
      throw std::runtime_error("duplicate ExifTool source record");
  }
  return result;
}
std::vector<double> array(const Tags &t, const char *key) {
  std::string s = get(t, key);
  std::istringstream in(s);
  std::vector<double> out;
  double v;
  while (in >> v) {
    if (!std::isfinite(v) || out.size() > 64)
      throw std::runtime_error("invalid embedded lens array");
    out.push_back(v);
  }
  if (!in.eof())
    throw std::runtime_error("invalid embedded lens numbers");
  return out;
}
Model sony(const Tags &tags, int component) {
  const char *keys[] = {"VignettingCorrParams", "DistortionCorrParams",
                        "ChromaticAberrationCorrParams"};
  std::vector<double> a;
  std::string source;
  for (auto group : {"SR2SubIFD:", "SubIFD:"}) {
    source = std::string(group) + keys[component];
    a = array(tags, source.c_str());
    if (!a.empty())
      break;
  }
  size_t n = component == 2 ? 32 : 16;
  if (a.size() != n + 1 || a[0] != double(n))
    return {};
  Model m;
  m.kind = Kind::sony;
  m.source = "embedded:" + source;
  for (auto &t : m.table)
    t.assign(16, 1);
  for (size_t i = 0; i < 16; ++i) {
    if (component == 0)
      m.table[0][i] = std::exp2(std::exp2(a[i + 1] / 8192. - 1) - .5);
    else if (component == 1)
      m.table[0][i] = 1 + a[i + 1] / 16384.;
    else {
      m.table[0][i] = 1 + a[i + 1] / 2097152.;
      m.table[2][i] = 1 + a[i + 17] / 2097152.;
    }
  }
  return m;
}
bool truth(const Tags &tags, const std::string &key) {
  auto s = get(tags, key);
  return s == "True" || s == "true" || s == "1";
}
bool opcodes(const Tags &t) {
  for (auto group : {"SubIFD:", "IFD0:"})
    for (auto key : {"OpcodeList1", "OpcodeList2", "OpcodeList3"})
      if (!get(t, std::string(group) + key).empty())
        return true;
  return false;
}
double sensor_crop(const Tags &t, double focal) {
  double f35 = numeric(first(
      t, {"ExifIFD:FocalLengthIn35mmFormat", "Composite:FocalLength35efl"}));
  if (f35 > 0 && focal > 0)
    return f35 / focal;
  double resolution = numeric(get(t, "ExifIFD:FocalPlaneXResolution")),
         width = numeric(get(t, "ExifIFD:ExifImageWidth"));
  double unit = numeric(get(t, "ExifIFD:FocalPlaneResolutionUnit")),
         mm = unit == 2   ? 25.4
              : unit == 3 ? 10
              : unit == 4 ? 1
              : unit == 5 ? .001
                          : 0;
  if (resolution > 0 && width > 0 && mm > 0) {
    double sensor = width / resolution * mm;
    if (sensor > 1 && sensor < 100)
      return 36 / sensor;
  }
  return 0;
}
} // namespace
void validate_prepare_options(const PrepareOptions &options) {
  validate_policy(options.policy);
  if (!options.collect)
    return;
  if (!std::filesystem::path(options.exiftool).is_absolute() ||
      !std::filesystem::is_regular_file(options.exiftool) ||
      access(options.exiftool.c_str(), X_OK) != 0)
    throw Error(26, "lens auto requires an executable ExifTool absolute path; "
                    "use --exiftool PATH or --lens-correction off");
  for (const auto &root : options.profile_roots)
    if (!std::filesystem::is_directory(root))
      throw Error(26, "lens profile root is not a directory: " + root.string());
}
std::vector<Calibration> collect(const std::vector<SourcePair> &sources,
                                 const std::filesystem::path &scratch,
                                 const PrepareOptions &options, bool in_memory) {
  std::vector<Calibration> out(sources.size());
  if (!options.collect)
    return out;
  const auto directory = scratch / "lens";
  if (!in_memory && !std::filesystem::create_directory(directory))
    throw Error(26, "lens metadata requires a fresh private scratch directory");
  const std::vector<std::string> flags = {
      "-XMP-aux:VignetteCorrectionAlreadyApplied",
      "-XMP-aux:DistortionCorrectionAlreadyApplied",
      "-XMP-aux:LateralChromaticAberrationCorrectionAlreadyApplied"};
  std::vector<std::string> original_tags = {
      "-IFD0:Make", "-XMP-tiff:Make", "-IFD0:Model", "-XMP-tiff:Model",
      "-ExifIFD:LensInfo", "-ExifIFD:FocalLength", "-Sony:FocalLength", "-Nikon:FocalLength",
      "-ExifIFD:FocalLengthIn35mmFormat",
      "-ExifIFD:FocalPlaneXResolution", "-ExifIFD:FocalPlaneResolutionUnit", "-ExifIFD:ExifImageWidth",
      "-SR2SubIFD:VignettingCorrParams", "-SubIFD:VignettingCorrParams",
      "-SR2SubIFD:DistortionCorrParams", "-SubIFD:DistortionCorrParams",
      "-SR2SubIFD:ChromaticAberrationCorrParams", "-SubIFD:ChromaticAberrationCorrParams",
      "-Nikon:VignetteCorrectionVersion", "-Nikon:DistortionCorrectionVersion",
      "-SubIFD:OpcodeList1", "-SubIFD:OpcodeList2", "-SubIFD:OpcodeList3",
      "-IFD0:OpcodeList1", "-IFD0:OpcodeList2", "-IFD0:OpcodeList3"};
  for (auto field : identity_detail::model_fields)
    original_tags.emplace_back(std::string("-") + field);
  for (auto field : identity_detail::vendor_fields)
    original_tags.emplace_back(std::string("-") + field);
  original_tags.insert(original_tags.end(), flags.begin(), flags.end());

  std::set<std::string> originals, generated;
  for (const auto &s : sources) {
    originals.insert(s.source.string());
    if (s.converted)
      generated.insert(s.prepared.string());
  }
  const size_t budget = std::min<size_t>(65536, converter_argv_budget());
  // Bound each invocation as well as argv size for slow network sources.
  constexpr size_t max_paths = 96;
  auto extract = [&](const std::set<std::string> &paths,
                     const std::vector<std::string> &tags,
                     const char *name, bool numeric) {
    std::map<std::string, Tags> result;
    std::vector<std::string> args = {options.exiftool, "-config", "", "-X", "-a", "-G1", "-Error"};
    if (numeric)
      args.emplace_back("-n");
    args.insert(args.end(), tags.begin(), tags.end());
    size_t base_bytes = 0;
    for (const auto &arg : args)
      base_bytes += arg.size() + 1 + sizeof(char *);
    size_t bytes = base_bytes, part = 0;
    auto invocation = args;
    auto flush = [&] {
      const auto stem = directory / (std::string(name) + "-" + std::to_string(part++));
      const auto xml_path = stem.string() + ".xml";
      const auto transcript = stem.string() + ".log";
      auto process = run_process(invocation, transcript, 120, xml_path, {}, {},
                                 in_memory, 16 * 1024 * 1024);
      if (process.timed_out || process.exit_code != 0)
        throw Error(26, (process.timed_out ? "lens metadata extraction timed out: "
                                         : "lens metadata extraction failed: ") +
                            (in_memory ? process.errors : transcript));
      result.merge(metadata(in_memory ? process.output : xml::read(xml_path)));
      invocation = args;
      bytes = base_bytes;
    };
    for (const auto &path : paths) {
      const size_t size = path.size() + 1 + sizeof(char *);
      if (base_bytes + size > budget)
        throw Error(26, "lens metadata source exceeds argv budget");
      if (bytes + size > budget || invocation.size() - args.size() == max_paths)
        flush();
      invocation.push_back(path);
      bytes += size;
    }
    if (invocation.size() > args.size())
      flush();
    return result;
  };

  auto records = extract(originals, original_tags, "metadata", true);
  // Try the decoded composite only when no explicit model was found.
  std::set<std::string> needs_identity;
  for (const auto &[path, tags] : records)
    if (identity_detail::explicit_model(tags).first.empty())
      needs_identity.insert(path);
  for (const auto &[path, tags] : extract(needs_identity, {"-Composite:LensID"}, "identities", false))
    for (const auto &[key, value] : tags)
      records.at(path).emplace("display:" + key, value);
  auto prepared_flags = extract(generated, flags, "prepared", true);
  ProfileCatalog catalog(options.profile_roots.empty() ? default_profile_roots()
                                                       : options.profile_roots);
  struct Fit { size_t frame; const Profile *profile; double crop; };
  std::vector<Fit> fits;
  std::set<std::string> fit_sources;
  for (size_t i = 0; i < sources.size(); ++i) {
    check_cancelled();
    const auto &s = sources[i];
    const auto &original = records.at(s.source.string());
    const auto &prepared = s.converted ? prepared_flags.at(s.prepared.string()) : original;
    auto &c = out[i];
    c.collected = true;
    c.make = first(original, {"IFD0:Make", "XMP-tiff:Make"});
    c.camera = first(original, {"IFD0:Model", "XMP-tiff:Model"});
    const auto lens_identity = identity_detail::resolve(original);
    c.lens = lens_identity.first;
    c.focal =
        numeric(first(original, {"ExifIFD:FocalLength", "Sony:FocalLength",
                                 "Nikon:FocalLength"}));
    std::array<bool, 3> source_coverage{}, prepared_coverage{};
    if (!inspect(s.source, &source_coverage) && opcodes(original))
      source_coverage.fill(true); // Vendor RAW whose opcode data cannot be inspected.
    if (s.prepared != s.source)
      inspect(s.prepared, &prepared_coverage);
    std::ostringstream evidence;
    evidence << "identity-source=" << lens_identity.second << char(10);
    for (const auto &[k, v] : original)
      evidence << k << '=' << v << char(10);
    if (s.converted)
      for (const auto &[k, v] : prepared)
        evidence << "prepared:" << k << '=' << v << char(10);
    c.evidence = evidence.str();
    const char *flags[] = {
        "VignetteCorrectionAlreadyApplied",
        "DistortionCorrectionAlreadyApplied",
        "LateralChromaticAberrationCorrectionAlreadyApplied"};
    for (int j = 0; j < 3; ++j) {
      auto key = std::string("XMP-aux:") + flags[j];
      // Not reported is explicit provenance, not a mathematical no-op claim.
      c.upstream[j] = s.converted ? Upstream::not_reported : Upstream::unknown;
      if (truth(original, key) || truth(prepared, key))
        c.upstream[j] = Upstream::applied;
      else if (prepared_coverage[j])
        c.upstream[j] = Upstream::sdk;
      else if (source_coverage[j])
        c.upstream[j] = s.converted ? Upstream::embedded : Upstream::sdk;
      else if (get(original, key) == "0" || get(original, key) == "False" ||
               get(original, key) == "false")
        c.upstream[j] = Upstream::not_reported;
      if (c.make == "SONY") {
        c.embedded[j] = sony(original, j);
        if (c.embedded[j].kind != Kind::none) {
          std::ostringstream values;
          values << std::setprecision(17) << c.embedded[j].source;
          for (const auto &table : c.embedded[j].table)
            for (double value : table)
              values << ' ' << value;
          const auto bytes = values.str();
          Sha256 hash;
          hash.update(bytes.data(), bytes.size());
          c.embedded[j].fingerprint = hash.hex();
        }
      }
      if (j == 0 && !get(original, "Nikon:VignetteCorrectionVersion").empty())
        c.embedded[j].source = "unsupported:nikon-private-vignette";
      if (j == 1 && !get(original, "Nikon:DistortionCorrectionVersion").empty())
        c.embedded[j].source = "unsupported:nikon-private-distortion";
    }
    bool needs_lcp = false;
    for (int j = 0; j < 3; ++j)
      needs_lcp |= c.upstream[j] == Upstream::not_reported &&
                   c.embedded[j].kind == Kind::none;
    if (needs_lcp) {
      const double crop = sensor_crop(original, c.focal);
      if (const auto *profile = catalog.match(c, crop, double(s.width) / s.height,
                                             c.selection, get(original, "ExifIFD:LensInfo"))) {
        fits.push_back({i, profile, crop});
        fit_sources.insert(s.source.string());
      }
    } else {
      c.selection = "embedded-or-upstream-owned";
    }
  }
  // Aperture and focus feed interpolation only after a profile has been selected.
  auto fit_tags = extract(fit_sources, {"-ExifIFD:FNumber", "-Nikon:FocusDistance",
      "-Sony:FocusDistance2", "-Composite:FocusDistance"}, "interpolation", true);
  for (const auto &fit : fits) {
    const auto &tags = fit_tags.at(sources[fit.frame].source.string());
    auto &c = out[fit.frame];
    c.aperture = numeric(get(tags, "ExifIFD:FNumber"));
    c.focus = numeric(first(tags, {"Nikon:FocusDistance", "Sony:FocusDistance2",
                                  "Composite:FocusDistance"}));
    c.fallback = interpolate_profile(*fit.profile, c.focal, c.aperture, c.focus, fit.crop);
  }
  for (size_t i = 0; i < sources.size(); ++i) {
    validate(out[i]);
    validate_domain(out[i], sources[i].width, sources[i].height, options.policy, check_cancelled);
  }
  return out;
}
} // namespace frame::lens
