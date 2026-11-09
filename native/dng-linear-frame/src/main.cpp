#include "dng_flags.h"
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
} // namespace
int main(int argc, char **argv) {
  using namespace frame;
  try {
    if (argc < 4)
      throw Error(2, "usage: dng-linear-frame <input.dng> <output.png> "
                     "<output.json> [--white-balance R,G,B] [--converter PATH] "
                     "[--converter-timeout SECONDS]");
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
    for (int i = 4; i < argc; ++i) {
      std::string a = argv[i];
      if (i + 1 >= argc)
        throw Error(2, "option value missing");
      std::string value = argv[++i];
      if (a == "--white-balance")
        wb = parse_wb(value);
      else if (a == "--converter")
        converter = value;
      else if (a == "--converter-timeout") {
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
    auto routing = inspect(input);
    auto kind = route(routing.photometric);
    Vec own = read_neutral(input);
    std::string source_hash = sha256(input);
    Temp temp;
    fs::path decoded_path = input;
    Json conversion{{"used", false}};
    if (kind == Route::converter) {
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
      if (check.photometric != 34892)
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
          {"verified_main_ifd_photometric", int(check.photometric)},
          {"temporary_directory", temp.path.string()},
          {"cleanup", "removed before publishing sidecar"},
          {"upstream_semantics", "Converter performs CFA demosaic; detailed "
                                 "clipping/metadata baking UNKNOWN"}};
    }
    Decoded d = decode(decoded_path, wb ? wb : std::optional<Vec>(own));
    std::vector<uint16_t> pixels(d.camera.size());
    double low = INFINITY, high = -INFINITY;
    size_t negatives = 0, upper = 0;
    for (size_t i = 0; i < d.camera.size(); i += 3) {
      Vec camera{d.camera[i], d.camera[i + 1], d.camera[i + 2]};
      Vec scene = multiply(d.camera_to_ap1, camera);
      for (int c = 0; c < 3; ++c) {
        float x = static_cast<float>(scene[c]);
        if (!std::isfinite(x))
          throw Error(40, "nonfinite color result");
        low = std::min(low, double(x));
        high = std::max(high, double(x));
        negatives += x < 0;
        upper += x > 65535.0f / 16384.0f;
        pixels[i + c] = quantize(x);
      }
    }
    Json metadata{
        {"schema_version", 1},
        {"source", input.string()},
        {"source_sha256", source_hash},
        {"source_main_ifd_offset", int(routing.ifd)},
        {"source_photometric", int(routing.photometric)},
        {"route", kind == Route::direct ? "linearraw-sdk"
                                        : "cfa-converter-linearraw-sdk"},
        {"converter", conversion},
        {"sdk_version", kDNGSDK_GetInfoVersion},
        {"sdk_build",
         "C++20; qDNGUseXMP=1; qDNGXMPFiles=0; FP32 Linearize destination"},
        {"width", int(d.width)},
        {"height", int(d.height)},
        {"channels", 3},
        {"working_space", "ACEScg AP1/D60 scene-linear FP32"},
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
    write_png(staged_png, d.width, d.height, pixels);
    // Output can reside on another volume, so copy to a sibling before rename.
    auto out_png = png;
    out_png += ".tmp-" + std::to_string(getpid());
    auto out_json = json;
    out_json += ".tmp-" + std::to_string(getpid());
    try {
      fs::copy_file(staged_png, out_png, fs::copy_options::overwrite_existing);
      save(out_json, metadata.text);
      temp.clear();
      fs::rename(out_png, png);
      fs::rename(out_json, json);
    } catch (...) {
      std::error_code e;
      fs::remove(out_png, e);
      fs::remove(out_json, e);
      throw;
    }
    std::cout << "decoded " << d.width << 'x' << d.height << " -> " << png
              << '\n';
    return 0;
  } catch (const Error &e) {
    std::cerr << e.what() << '\n';
    return e.code;
  } catch (const std::exception &e) {
    std::cerr << e.what() << '\n';
    return 50;
  }
}
