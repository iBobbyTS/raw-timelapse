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
    near(result[0].plane[0].k[0], std::log(20.) + 4 + std::log(std::sqrt(10.)));
    auto clamped = interpolate_profile(p, 1, 4, .1, 1);
    near(clamped[0].plane[0].k[0], std::log(10.) + 4);
    auto missing = interpolate_profile(p, 20, 0, 0, 1);
    require(missing[0].kind == Kind::none,
            "missing aperture invented vignette");
    near(missing[1].plane[0].k[0], std::log(20.) + 2 + std::log(10.));
    auto crop = interpolate_profile(p, 20, 4, 1, 2);
    near(crop[1].plane[0].fx, 2);
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
