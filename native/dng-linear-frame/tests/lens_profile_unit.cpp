#include "lens_profile.h"
#include <chrono>
#include <cmath>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <stdexcept>
using namespace frame::lens;
namespace {
void require(bool value, const char *message) {
  if (!value)
    throw std::runtime_error(message);
}
double coefficient(const Model &m) {
  if (m.terms.empty())
    return m.plane[0].k[0];
  double sum = 0;
  for (const auto &term : m.terms)
    sum += term.weight * term.plane[0].k[0];
  return sum;
}
void near(double a, double b) {
  require(std::abs(a - b) < 1e-10, "profile interpolation truth mismatch");
}
std::string document(double k, int version = 2) {
  return "<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' "
         "xmlns:c='http://ns.adobe.com/photoshop/1.0/"
         "camera-profile'><rdf:li><rdf:Description c:Make='Test' "
         "c:Model='Camera' c:Lens='Known Lens' c:CameraRawProfile='True' "
         "c:SensorFormatFactor='1' c:ImageWidth='6000' c:ImageLength='4000' "
         "c:FocalLength='20' c:ApertureValue='4' "
         "c:FocusDistance='10'><c:PerspectiveModel c:Version='" +
         std::to_string(version) +
         "' c:FocalLengthX='1' c:FocalLengthY='1' c:RadialDistortParam1='" +
         std::to_string(k) + "'/></rdf:Description></rdf:li></rdf:RDF>";
}
} // namespace
int main(int argc, char **argv) {
  try {
    require(argc == 2, "test artifact directory required");
    Profile p;
    p.crop = 1;
    for (double focal : {10., 40.})
      for (double aperture : {2., 6.})
        for (double focus : {1., 10.}) {
          ProfileSample s;
          s.focal = focal;
          s.aperture = aperture;
          s.focus = focus;
          for (auto &m : s.model) {
            m.kind = Kind::perspective;
            m.source = "test";
            m.plane[0].k[0] = std::log(focal) + aperture + std::log(focus);
          }
          p.samples.push_back(s);
        }
    auto result = interpolate_profile(p, 20, 4, std::sqrt(10.), 1);
    near(coefficient(result[0]), std::log(20.) + 4 + std::log(std::sqrt(10.)));
    auto clamped = interpolate_profile(p, 1, 4, .1, 1);
    near(coefficient(clamped[0]), std::log(10.) + 4);
    auto missing = interpolate_profile(p, 20, 0, 0, 1);
    require(missing[0].kind == Kind::none,
            "missing aperture invented vignette");
    near(coefficient(missing[1]), std::log(20.) + 2 + std::log(10.));
    auto crop = interpolate_profile(p, 20, 4, 1, 2);
    near(crop[1].plane[0].fx, 2);
    for (const auto &term : crop[1].terms)
      near(term.plane[0].fx, 2);
    // Equivalent physical fields with different normalizations must survive
    // the actual Prepare interpolation -> frozen transport -> pixel kernel.
    Profile equivalent;
    for (int i = 0; i < 2; ++i) {
      ProfileSample s;
      s.focal = i ? 40 : 10;
      s.aperture = 4;
      s.focus = 10;
      for (int component : {0, 1, 2}) {
        auto &m = s.model[component];
        m.kind = Kind::perspective;
        m.source = "synthetic:equivalent-field";
        for (auto &plane : m.plane) {
          plane.fx = plane.fy = i ? 2 : 1;
          plane.k[0] = i ? -.8 : -.2;
        }
        if (component == 2) {
          // Green coefficients do not act as additional base distortion.
          m.plane[1].k[0] = 0;
          m.plane[2].k[0] = i ? -.4 : -.1;
        }
      }
      equivalent.samples.push_back(s);
    }
    const uint32_t w = 65, h = 49;
    std::vector<float> pixels(size_t(w) * h * 3);
    for (uint32_t y = 0; y < h; ++y)
      for (uint32_t x = 0; x < w; ++x)
        for (int channel = 0; channel < 3; ++channel)
          pixels[(size_t(y) * w + x) * 3 + channel] = float(x + 2 * y + channel - 30.);
    for (int component : {0, 1, 2}) {
      Calibration c;
      c.collected = true;
      c.upstream.fill(Upstream::not_reported);
      auto run = [&](double focal) {
        c.fallback[component] = interpolate_profile(equivalent, focal, 4, 10, 1)[component];
        return correct(pixels, w, h, deserialize(serialize(c)), true);
      };
      auto left = run(10), right = run(40);
      for (double focal : {10., 12., 20., 30., 40.}) {
        auto middle = run(focal);
        require(middle.pixels.size() == pixels.size(), "equivalent field not applied");
        for (size_t j = 0; j < pixels.size(); ++j) {
          require(std::abs(left.pixels[j] - right.pixels[j]) < 1e-5,
                  "equivalent endpoints disagree");
          require(std::abs(left.pixels[j] - middle.pixels[j]) < 1e-5,
                  "interpolation changed an equivalent physical field");
        }
      }
    }
    namespace fs = std::filesystem;
    auto work =
        fs::path(argv[1]) /
        std::to_string(
            std::chrono::steady_clock::now().time_since_epoch().count());
    fs::create_directories(work);
    auto file = work / "arbitrary.lcp";
    std::ofstream(file) << document(-.1);
    auto parsed = read_profile(file);
    require(parsed.samples.size() == 1 && parsed.raw, "LCP schema not parsed");
    near(parsed.samples[0].model[1].plane[0].k[0], -.1);
    Calibration c;
    c.collected = true;
    c.make = "TEST";
    c.camera = "Camera";
    c.lens = "Known-Lens";
    c.focal = 20;
    c.aperture = 4;
    std::string reason;
    ProfileCatalog catalog({work});
    auto model = catalog.resolve(c, 1, 1.5, reason);
    require(model[1].kind == Kind::perspective && !model[1].fingerprint.empty(),
            "exact normalized match failed");
    require(catalog.resolve(c, 1, 2. / 3., reason)[1].kind == Kind::none,
            "transposed sensor axes accepted without a coordinate transform");
    std::ofstream(work / "duplicate.lcp") << document(-.1);
    ProfileCatalog duplicates({work});
    require(duplicates.resolve(c, 1, 1.5, reason)[1].kind == Kind::perspective,
            "byte-identical installed copies were ambiguous");
    std::ofstream(work / "conflicting.lcp") << document(-.2);
    ProfileCatalog conflict({work});
    require(conflict.resolve(c, 1, 1.5, reason)[1].kind == Kind::none &&
                reason == "ambiguous-lcp-match",
            "ambiguous profile arbitrarily selected");
    auto alias_directory = work / "official-aliases";
    fs::create_directories(alias_directory);
    auto alias_xml = document(-.1);
    auto insert = alias_xml.find("<c:PerspectiveModel");
    alias_xml.insert(insert,
        "<c:AlternateLensNames><rdf:Seq><rdf:li>Exact | Art Lens</rdf:li>"
        "<rdf:li>Second Official Name</rdf:li></rdf:Seq></c:AlternateLensNames>");
    std::ofstream(alias_directory / "alias.lcp") << alias_xml;
    c.lens = "Exact | Art Lens";
    ProfileCatalog alias_catalog({alias_directory});
    require(alias_catalog.resolve(c, 1, 1.5, reason)[1].kind == Kind::perspective &&
                reason.find("match=official-alias") != std::string::npos,
            "declared RDF alias did not match with provenance");
    c.lens = "Exact";
    require(alias_catalog.resolve(c, 1, 1.5, reason)[1].kind == Kind::none,
            "alias was split on a legitimate lens-name pipe");
    c.lens = "Second Official Name";
    require(alias_catalog.resolve(c, 1, 1.5, reason)[1].kind == Kind::perspective,
            "second declared alias omitted");
    auto conflicting_alias = alias_xml;
    conflicting_alias.replace(conflicting_alias.find("-0.100000"), 9, "-0.200000");
    std::ofstream(alias_directory / "other.lcp") << conflicting_alias;
    ProfileCatalog ambiguous_alias({alias_directory});
    require(ambiguous_alias.resolve(c, 1, 1.5, reason)[1].kind == Kind::none &&
                reason == "ambiguous-lcp-match", "alias bypassed ambiguity protection");
    const auto fixed_directory = work / "fixed-lens";
    fs::create_directories(fixed_directory);
    auto fixed_xml = document(-.1);
    const std::string original_identity = "c:Make='Test' c:Model='Camera' c:Lens='Known Lens'";
    fixed_xml.replace(fixed_xml.find(original_identity), original_identity.size(),
        "c:Make='DJI' c:Model='AC004' c:Lens='3.3 mm f/2.8' "
        "c:LensInfo='333/100 333/100 28/10 28/10' c:LensPrettyName='DJI Osmo Action 5 Pro'");
    std::ofstream(fixed_directory / "fixed.lcp") << fixed_xml;
    ProfileCatalog fixed_catalog({fixed_directory});
    Calibration fixed;
    fixed.make = "DJI";
    fixed.camera = "AC004";
    fixed.focal = 3.33;
    require(fixed_catalog.resolve(fixed, 1, 1.5, reason, "3.33 3.33 4 4")[1].kind == Kind::none
                && fixed.lens.empty(), "fixed lens matched a different optical specification");
    require(fixed_catalog.resolve(fixed, 1, 1.5, reason, "3.33 3.33 2.8 2.8")[1].kind == Kind::perspective
                && fixed.lens == "DJI Osmo Action 5 Pro", "fixed lens match or display name missing");
    auto invalid = work / "version.xml";
    std::ofstream(invalid) << document(-.1, 99);
    require(read_profile(invalid).unsupported,
            "unknown model version accepted");
    std::ofstream(work / "external.xml")
        << "<!DOCTYPE x [<!ENTITY leak SYSTEM "
           "'file:///etc/passwd'>]><x>&leak;</x>";
    bool rejected = false;
    try {
      read_profile(work / "external.xml");
    } catch (const std::exception &) {
      rejected = true;
    }
    require(rejected, "external XML entities accepted");
    std::cout
        << "PASS LCP numerical interpolation, bounds, missing metadata, crop, "
           "exact matching, duplicate/ambiguous profiles and XML safety"
        << std::endl;
    return 0;
  } catch (const std::exception &e) {
    std::cerr << e.what() << std::endl;
    return 1;
  }
}
