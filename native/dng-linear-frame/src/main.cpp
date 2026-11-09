#include "dng_flags.h"
#include "analysis.h"
#include "frame.h"
#include <CoreFoundation/CoreFoundation.h>
#include <algorithm>
#include <cmath>
#include <fstream>
#include <iostream>
#include <sstream>
#include <unistd.h>
namespace {
using namespace frame;
std::string read_text(const fs::path &p) {
  std::ifstream f(p);
  return {std::istreambuf_iterator<char>(f), {}};
}
void save(const fs::path &p, const std::string &s) {
  std::ofstream f(p);
  f << s << '\n';
  f.close();
  if (!f)
    throw Error(50, "cannot write JSON sidecar");
}
Json vec(const Vec &v) { return std::vector<Json>{v[0], v[1], v[2]}; }
struct Temp {
  fs::path path;
  bool removed = false;
  Temp() {
    auto s = (fs::temp_directory_path() / "dng-linear-frame-XXXXXX").string();
    std::vector<char> b(s.begin(), s.end());
    b.push_back(0);
    auto p = mkdtemp(b.data());
    if (!p)
      throw Error(50, "cannot create run temporary directory");
    path = p;
  }
  void clear() {
    std::error_code e;
    fs::remove_all(path, e);
    if (e)
      throw Error(50, "cannot remove temporary directory: " + e.message());
    removed = true;
    std::cerr << "temporary directory cleaned: " << path << '\n';
  }
  ~Temp() {
    if (removed)
      return;
    std::error_code e;
    fs::remove_all(path, e);
    if (e)
      std::cerr << "temporary cleanup failed: " << path << ": " << e.message()
                << '\n';
    else
      std::cerr << "temporary directory cleaned: " << path << '\n';
  }
};
std::string converter_version(const fs::path &exe) {
  auto bundle_path = exe.parent_path().parent_path().parent_path().string();
  CFURLRef url = CFURLCreateFromFileSystemRepresentation(
      nullptr, (const UInt8 *)bundle_path.data(), bundle_path.size(), true);
  if (!url)
    return "UNKNOWN";
  CFBundleRef bundle = CFBundleCreate(nullptr, url);
  CFRelease(url);
  if (!bundle)
    return "UNKNOWN";
  CFTypeRef value = CFBundleGetValueForInfoDictionaryKey(
      bundle, CFSTR("CFBundleShortVersionString"));
  char text[256]{};
  bool ok = value && CFGetTypeID(value) == CFStringGetTypeID() &&
            CFStringGetCString((CFStringRef)value, text, sizeof text,
                               kCFStringEncodingUTF8);
  CFRelease(bundle);
  return ok ? text : "UNKNOWN";
}
Vec parse_wb(std::string s) {
  std::stringstream in(s);
  Vec v;
  char comma;
  for (int i = 0; i < 3; ++i) {
    if (!(in >> v[i]) || !std::isfinite(v[i]) || v[i] <= 0)
      throw Error(
          2, "white balance requires three positive finite camera neutrals");
    if (i < 2 && (!(in >> comma) || comma != ','))
      throw Error(2, "white balance requires R,G,B");
  }
  in >> std::ws;
  if (!in.eof())
    throw Error(2, "unexpected white balance suffix");
  return v;
}
double parse_ev(const std::string &s) {
  size_t end = 0;
  double value = 0;
  try {
    value = std::stod(s, &end);
  } catch (...) {
    throw Error(3, "invalid exposure EV");
  }
  if (end != s.size() || !std::isfinite(value))
    throw Error(3, "--exposure-ev requires one finite number");
  if (!std::isfinite(std::exp2(value)))
    throw Error(3, "exposure EV gain is not finite");
  return value;
}
analysis::Roi parse_roi(const std::string &s) {
  std::stringstream in(s);
  analysis::Roi roi;
  char comma;
  double *field[4] = {&roi.x, &roi.y, &roi.w, &roi.h};
  for (int i = 0; i < 4; ++i) {
    if (!(in >> *field[i]))
      throw Error(3, "analysis ROI requires X,Y,W,H");
    if (i < 3 && (!(in >> comma) || comma != ','))
      throw Error(3, "analysis ROI requires X,Y,W,H");
  }
  in >> std::ws;
  if (!in.eof())
    throw Error(3, "unexpected analysis ROI suffix");
  analysis::validate_roi(roi);
  return roi;
}
} // namespace
int main(int argc, char **argv) {
  using namespace frame;
  try {
    if (argc < 4)
      throw Error(2, "usage: dng-linear-frame <input.dng> <output.png> "
                     "<output.json> [--white-balance R,G,B] [--converter PATH] "
                     "[--converter-timeout SECONDS] [--exposure-ev EV] "
                     "[--emit-analysis PATH] [--analysis-roi X,Y,W,H] "
                     "[--no-png]");
    fs::path input = fs::canonical(argv[1]),
             png = fs::weakly_canonical(argv[2]),
             json = fs::weakly_canonical(argv[3]);
    if (input == png || input == json || png == json ||
        (fs::exists(png) && fs::equivalent(input, png)) ||
        (fs::exists(json) && fs::equivalent(input, json)))
      throw Error(2, "input and output paths must be distinct");
    std::optional<Vec> wb;
    std::string converter = "/Applications/Adobe DNG "
                            "Converter.app/Contents/MacOS/Adobe DNG Converter";
    int timeout = 120;
    double exposure_ev = 0;
    fs::path analysis_path;
    analysis::Roi roi;
    bool no_png = false;
    bool roi_set = false;
    for (int i = 4; i < argc; ++i) {
      std::string a = argv[i];
      if (a == "--no-png") {
        no_png = true;
        continue;
      }
      if (i + 1 >= argc)
        throw Error(2, "option value missing");
      std::string value = argv[++i];
      if (a == "--white-balance")
        wb = parse_wb(value);
      else if (a == "--converter")
        converter = value;
      else if (a == "--exposure-ev")
        exposure_ev = parse_ev(value);
      else if (a == "--emit-analysis")
        analysis_path = fs::weakly_canonical(value);
      else if (a == "--analysis-roi") {
        roi = parse_roi(value);
        roi_set = true;
      } else if (a == "--converter-timeout") {
        size_t end = 0;
        try {
          timeout = std::stoi(value, &end);
        } catch (...) {
          throw Error(2, "invalid timeout");
        }
        if (end != value.size() || timeout < 1 || timeout > 3600)
          throw Error(2, "timeout must be 1..3600 seconds");
      } else
        throw Error(2, "unknown option: " + a);
    }
    if (roi_set && analysis_path.empty())
      throw Error(2, "--analysis-roi requires --emit-analysis");
    if (!analysis_path.empty() &&
        (analysis_path == input || analysis_path == png ||
         analysis_path == json ||
         (fs::exists(analysis_path) && fs::equivalent(input, analysis_path))))
      throw Error(2, "analysis sidecar must be distinct from input and "
                     "outputs");
    auto routing = inspect(input);
    // Recognition is DNG-structural: only a recognized DNG is routed by its
    // main RAW photometric; every other file is a vendor/other RAW candidate
    // for the converter alone.
    auto kind = routing ? route(routing->photometric) : Route::vendor_raw;
    std::string source_hash = sha256(input);
    Temp temp;
    fs::path decoded_path = input;
    Json conversion{{"used", false}};
    auto convert = [&] {
      fs::path dir = temp.path / "linear";
      fs::create_directory(dir);
      std::vector<std::string> command{converter, "-c",         "-l",
                                       "-d",      dir.string(), input.string()};
      auto log = temp.path / "converter.log";
      auto result = run_process(command, log, timeout);
      std::string transcript = read_text(log);
      if (result.timed_out || result.exit_code != 0) {
        std::cerr << transcript;
        throw Error(result.timed_out ? 25 : 24,
                    "Converter failed; exit=" +
                        std::to_string(result.exit_code));
      }
      std::vector<fs::path> files;
      for (auto &e : fs::directory_iterator(dir))
        if (e.is_regular_file())
          files.push_back(e.path());
      if (files.size() != 1)
        throw Error(23, "Converter must produce exactly one DNG");
      decoded_path = files.front();
      auto check = inspect(decoded_path);
      if (!check || check->photometric != 34892)
        throw Error(23,
                    "Converter output main RAW IFD is not LinearRaw (34892)");
      std::vector<Json> args;
      for (auto &a : command)
        args.emplace_back(a);
      conversion = Json{
          {"used", true},
          {"executable", converter},
          {"version", converter_version(converter)},
          {"command", args},
          {"exit_code", result.exit_code},
          {"stdout_stderr", transcript},
          {"output_name", decoded_path.filename().string()},
          {"output_sha256", sha256(decoded_path)},
          {"verified_main_ifd_photometric", int(check->photometric)},
          {"temporary_directory", temp.path.string()},
          {"cleanup", "removed before publishing sidecar"},
          {"upstream_semantics", "Converter performs CFA demosaic; detailed "
                                 "clipping/metadata baking UNKNOWN"}};
    };
    // "as-shot" metadata comes from the recognized DNG itself. A vendor/other
    // input is hashed as the original file, but its neutral and capture time
    // exist only in the converter's DNG output (probed: ADC preserves both).
    Vec own;
    if (kind == Route::vendor_raw) {
      convert();
      own = read_neutral(decoded_path);
    } else {
      own = read_neutral(input);
      if (kind == Route::converter)
        convert();
    }
    Decoded d = decode(decoded_path, wb ? wb : std::optional<Vec>(own));
    auto capture_time = read_capture_time(
        kind == Route::vendor_raw ? decoded_path : input);
    const double gain = std::exp2(exposure_ev);
    const bool want_analysis = !analysis_path.empty();
    std::vector<uint16_t> pixels;
    if (!no_png)
      pixels.resize(d.camera.size());
    std::vector<float> gained_scene;
    if (want_analysis)
      gained_scene.resize(d.camera.size());
    double low = INFINITY, high = -INFINITY;
    size_t negatives = 0, upper = 0;
    for (size_t i = 0; i < d.camera.size(); i += 3) {
      Vec camera{d.camera[i], d.camera[i + 1], d.camera[i + 2]};
      Vec scene = multiply(d.camera_to_ap1, camera);
      for (int c = 0; c < 3; ++c) {
        float x = static_cast<float>(scene[c] * gain);
        if (!std::isfinite(x))
          throw Error(40, "nonfinite color result");
        low = std::min(low, double(x));
        high = std::max(high, double(x));
        negatives += x < 0;
        upper += x > 65535.0f / 16384.0f;
        if (want_analysis)
          gained_scene[i + c] = x;
        if (!no_png)
          pixels[i + c] = quantize(x);
      }
    }
    std::optional<analysis::Analysis> measured;
    if (want_analysis)
      measured = analysis::measure(d, gained_scene, roi, exposure_ev);
    Json metadata{
        {"schema_version", 1},
        {"source", input.string()},
        {"source_sha256", source_hash},
        {"source_main_ifd_offset",
         routing ? Json(int(routing->ifd)) : Json()},
        {"source_photometric",
         routing ? Json(int(routing->photometric)) : Json()},
        {"route", kind == Route::direct
                      ? "linearraw-sdk"
                      : kind == Route::converter
                            ? "cfa-converter-linearraw-sdk"
                            : "vendor-raw-converter-linearraw-sdk"},
        {"converter", conversion},
        {"sdk_version", kDNGSDK_GetInfoVersion},
        {"sdk_build",
         "C++20; qDNGUseXMP=1; qDNGXMPFiles=0; FP32 Linearize destination"},
        {"width", int(d.width)},
        {"height", int(d.height)},
        {"channels", 3},
        {"working_space", "ACEScg AP1/D60 scene-linear FP32"},
        {"exposure_ev", exposure_ev},
        {"gain", gain},
        {"capture_time", capture_time ? Json(*capture_time) : Json()},
        {"analysis_sidecar",
         want_analysis ? Json(analysis_path.string()) : Json()},
        {"as_shot_neutral", vec(own)},
        {"decoded_as_shot_neutral", vec(d.as_shot)},
        {"applied_white_balance", vec(d.applied)},
        {"white_balance_source", wb ? "override" : "as-shot"},
        {"color", d.color},
        {"decode_semantics", d.semantics},
        {"observed_scene_min", low},
        {"observed_scene_max", high},
        {"quantization",
         Json{{"scale", 16384},
              {"rounding", "nearest; ties upward"},
              {"negative_policy", "clip to 0"},
              {"upper_policy", "above 65535/16384 clips to 65535"},
              {"negative_samples", double(negatives)},
              {"above_upper_samples", double(upper)},
              {"transfer", "linear; no OETF"}}}};
    // Build artifacts in the run directory; cleanup is complete before
    // publishing.
    auto staged_png = temp.path / "frame.png";
    if (!no_png)
      write_png(staged_png, d.width, d.height, pixels);
    // Output can reside on another volume, so copy to a sibling before rename.
    auto out_png = png;
    out_png += ".tmp-" + std::to_string(getpid());
    auto out_json = json;
    out_json += ".tmp-" + std::to_string(getpid());
    auto out_analysis = analysis_path;
    if (want_analysis)
      out_analysis += ".tmp-" + std::to_string(getpid());
    try {
      if (!no_png)
        fs::copy_file(staged_png, out_png,
                      fs::copy_options::overwrite_existing);
      save(out_json, metadata.text);
      if (measured)
        save(out_analysis,
             analysis::sidecar_json(*measured, source_hash).text);
      temp.clear();
      if (!no_png)
        fs::rename(out_png, png);
      fs::rename(out_json, json);
      if (measured)
        fs::rename(out_analysis, analysis_path);
    } catch (...) {
      std::error_code e;
      fs::remove(out_png, e);
      fs::remove(out_json, e);
      if (measured)
        fs::remove(out_analysis, e);
      throw;
    }
    std::cout << "decoded " << d.width << 'x' << d.height;
    if (no_png)
      std::cout << " (PNG suppressed)";
    else
      std::cout << " -> " << png;
    if (measured)
      std::cout << " analysis=" << analysis_path;
    std::cout << '\n';
    return 0;
  } catch (const Error &e) {
    std::cerr << e.what() << '\n';
    return e.code;
  } catch (const std::exception &e) {
    std::cerr << e.what() << '\n';
    return 50;
  }
}
