#include "dng_flags.h"
#include "analysis.h"
#include "frame.h"
#include <CoreFoundation/CoreFoundation.h>
#include <algorithm>
#include <cmath>
#include <csignal>
#include <cstdio>
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

// ---- stdout streaming protocol v1 -----------------------------------------
// Every stdout-mode call starts with the 8-byte ASCII magic and continues with
// records: type:u8 | reserved:u8x3 (zero) | length:u64 LE | payload[length].
constexpr char kStdoutMagic[8] = {'R', 'A', 'W', 'T', 'L', 'S', 'T', '1'};
constexpr unsigned char kRecordMetadata = 0x01;
constexpr unsigned char kRecordAnalysis = 0x02;
constexpr unsigned char kRecordFrame = 0x03;
constexpr unsigned char kRecordPixels = 0x04;
// Write staging: payloads are never materialized as one full byte copy.
constexpr size_t kWriteChunk = size_t(1) << 16;
// 32768 samples per staging block (64 KiB of RGB48LE bytes).
constexpr size_t kPixelChunk = size_t(1) << 15;

void write_all(const void *data, size_t length) {
  const auto *bytes = static_cast<const unsigned char *>(data);
  while (length) {
    size_t chunk = std::min(length, kWriteChunk);
    if (std::fwrite(bytes, 1, chunk, stdout) != chunk ||
        std::ferror(stdout))
      throw Error(50, "stdout write failed");
    bytes += chunk;
    length -= chunk;
  }
}
void write_record_header(unsigned char type, uint64_t length) {
  unsigned char header[12] = {type, 0, 0, 0};
  for (int i = 0; i < 8; ++i)
    header[4 + i] = static_cast<unsigned char>((length >> (8 * i)) & 0xff);
  write_all(header, sizeof header);
}
void write_record_text(unsigned char type, const std::string &payload) {
  write_record_header(type, uint64_t(payload.size()));
  write_all(payload.data(), payload.size());
}
// The 0x04 payload is little-endian uint16 samples, interleaved HWC RGB.
template <typename Sink>
void for_each_rgb48le_chunk(const std::vector<uint16_t> &pixels, Sink &&sink) {
  std::vector<unsigned char> bytes;
  for (size_t offset = 0; offset < pixels.size(); offset += kPixelChunk) {
    size_t count = std::min(kPixelChunk, pixels.size() - offset);
    bytes.resize(count * 2);
    for (size_t i = 0; i < count; ++i) {
      const uint16_t value = pixels[offset + i];
      bytes[2 * i] = static_cast<unsigned char>(value & 0xff);
      bytes[2 * i + 1] = static_cast<unsigned char>(value >> 8);
    }
    sink(bytes.data(), bytes.size());
  }
}

struct Prepared {
  Decoded decoded;
  fs::path source;
  std::string source_hash;
  Json conversion;
  std::optional<Routing> routing;
  Route kind = Route::vendor_raw;
  std::optional<std::string> capture_time;
  Vec own{};
  bool wb_override = false;
};

// Routing, optional Converter step, decode and capture-time read. The run
// temporary directory stays owned by the caller so positional publication and
// stdout streaming keep their existing cleanup ordering.
Prepared prepare(Temp &temp, const fs::path &input,
                 const std::optional<Vec> &wb, const std::string &converter,
                 int timeout) {
  Prepared out;
  out.source = input;
  out.wb_override = wb.has_value();
  out.routing = inspect(input);
  // Recognition is DNG-structural: only a recognized DNG is routed by its
  // main RAW photometric; every other file is a vendor/other RAW candidate for
  // the converter alone.
  out.kind = out.routing ? route(out.routing->photometric) : Route::vendor_raw;
  out.source_hash = sha256(input);
  fs::path decoded_path = input;
  out.conversion = Json{{"used", false}};
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
                  "Converter failed; exit=" + std::to_string(result.exit_code));
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
    out.conversion = Json{
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
  if (out.kind == Route::vendor_raw) {
    convert();
    out.own = read_neutral(decoded_path);
  } else {
    out.own = read_neutral(input);
    if (out.kind == Route::converter)
      convert();
  }
  out.decoded = decode(decoded_path, wb ? wb : std::optional<Vec>(out.own));
  out.capture_time = read_capture_time(out.kind == Route::vendor_raw
                                           ? decoded_path
                                           : input);
  return out;
}

struct FrameOutput {
  uint32_t width = 0, height = 0;
  std::string metadata;                // without the trailing newline
  std::optional<std::string> analysis; // without the trailing newline
  std::vector<uint16_t> pixels;        // empty unless requested
};

// Gain, quantization, photometry and the metadata/analysis JSON documents.
// The analysis_sidecar field is the requested positional path or null.
FrameOutput render(const Prepared &prepared,
                   const std::optional<fs::path> &analysis_sidecar,
                   const analysis::Roi &roi, double exposure_ev,
                   bool want_analysis, bool need_pixels) {
  const Decoded &d = prepared.decoded;
  const double gain = std::exp2(exposure_ev);
  std::vector<uint16_t> pixels;
  if (need_pixels)
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
      if (need_pixels)
        pixels[i + c] = quantize(x);
    }
  }
  std::optional<analysis::Analysis> measured;
  if (want_analysis)
    measured = analysis::measure(d, gained_scene, roi, exposure_ev);
  Json metadata{
      {"schema_version", 1},
      {"source", prepared.source.string()},
      {"source_sha256", prepared.source_hash},
      {"source_main_ifd_offset",
       prepared.routing ? Json(int(prepared.routing->ifd)) : Json()},
      {"source_photometric",
       prepared.routing ? Json(int(prepared.routing->photometric)) : Json()},
      {"route", prepared.kind == Route::direct
                    ? "linearraw-sdk"
                    : prepared.kind == Route::converter
                          ? "cfa-converter-linearraw-sdk"
                          : "vendor-raw-converter-linearraw-sdk"},
      {"converter", prepared.conversion},
      {"sdk_version", kDNGSDK_GetInfoVersion},
      {"sdk_build",
       "C++20; qDNGUseXMP=1; qDNGXMPFiles=0; FP32 Linearize destination"},
      {"width", int(d.width)},
      {"height", int(d.height)},
      {"channels", 3},
      {"working_space", "ACEScg AP1/D60 scene-linear FP32"},
      {"exposure_ev", exposure_ev},
      {"gain", gain},
      {"capture_time",
       prepared.capture_time ? Json(*prepared.capture_time) : Json()},
      {"analysis_sidecar",
       analysis_sidecar ? Json(analysis_sidecar->string()) : Json()},
      {"as_shot_neutral", vec(prepared.own)},
      {"decoded_as_shot_neutral", vec(d.as_shot)},
      {"applied_white_balance", vec(d.applied)},
      {"white_balance_source",
       prepared.wb_override ? Json("override") : Json("as-shot")},
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
  FrameOutput out;
  out.width = d.width;
  out.height = d.height;
  out.metadata = metadata.text;
  if (measured)
    out.analysis = analysis::sidecar_json(*measured, prepared.source_hash).text;
  out.pixels = std::move(pixels);
  return out;
}

} // namespace
int main(int argc, char **argv) {
  using namespace frame;
  try {
    // The stdout grammar is flag-first: only argv[1] selects a streaming mode,
    // so the positional path slots stay paths even when a path is spelled like
    // a mode flag.
    const bool stdout_mode =
        argc >= 2 && (std::string(argv[1]) == "--stdout-analysis" ||
                      std::string(argv[1]) == "--stdout-frame");
    if (!stdout_mode) {
      if (argc < 4)
        throw Error(2, "usage: dng-linear-frame <input.dng> <output.png> "
                       "<output.json> [--white-balance R,G,B] [--converter "
                       "PATH] [--converter-timeout SECONDS] [--exposure-ev EV] "
                       "[--emit-analysis PATH] [--analysis-roi X,Y,W,H] "
                       "[--no-png]; or dng-linear-frame --stdout-analysis "
                       "<input.dng> | --stdout-frame <input.dng> with the "
                       "applicable options");
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
           (fs::exists(analysis_path) &&
            fs::equivalent(input, analysis_path))))
        throw Error(2, "analysis sidecar must be distinct from input and "
                       "outputs");
      Temp temp;
      Prepared prepared = prepare(temp, input, wb, converter, timeout);
      FrameOutput frame =
          render(prepared,
                 analysis_path.empty()
                     ? std::optional<fs::path>()
                     : std::optional<fs::path>(analysis_path),
                 roi, exposure_ev, !analysis_path.empty(), !no_png);
      // Build artifacts in the run directory; cleanup is complete before
      // publishing.
      auto staged_png = temp.path / "frame.png";
      if (!no_png)
        write_png(staged_png, frame.width, frame.height, frame.pixels);
      // Output can reside on another volume, so copy to a sibling before
      // rename.
      auto out_png = png;
      out_png += ".tmp-" + std::to_string(getpid());
      auto out_json = json;
      out_json += ".tmp-" + std::to_string(getpid());
      auto out_analysis = analysis_path;
      if (frame.analysis)
        out_analysis += ".tmp-" + std::to_string(getpid());
      try {
        if (!no_png)
          fs::copy_file(staged_png, out_png,
                        fs::copy_options::overwrite_existing);
        save(out_json, frame.metadata);
        if (frame.analysis)
          save(out_analysis, *frame.analysis);
        temp.clear();
        if (!no_png)
          fs::rename(out_png, png);
        fs::rename(out_json, json);
        if (frame.analysis)
          fs::rename(out_analysis, analysis_path);
      } catch (...) {
        std::error_code e;
        fs::remove(out_png, e);
        fs::remove(out_json, e);
        if (frame.analysis)
          fs::remove(out_analysis, e);
        throw;
      }
      std::cout << "decoded " << frame.width << 'x' << frame.height;
      if (no_png)
        std::cout << " (PNG suppressed)";
      else
        std::cout << " -> " << png;
      if (frame.analysis)
        std::cout << " analysis=" << analysis_path;
      std::cout << '\n';
      return 0;
    }
    // ---- stdout streaming modes -------------------------------------------
    // argv[1] is the mode flag; argv[2] is the input path and options follow.
    const bool want_analysis = std::string(argv[1]) == "--stdout-analysis";
    if (argc < 3)
      throw Error(2, "option value missing");
    const fs::path input = fs::canonical(argv[2]);
    std::optional<Vec> wb;
    std::string converter = "/Applications/Adobe DNG "
                            "Converter.app/Contents/MacOS/Adobe DNG Converter";
    int timeout = 120;
    double exposure_ev = 0;
    analysis::Roi roi;
    for (int i = 3; i < argc; ++i) {
      std::string a = argv[i];
      if (a == "--stdout-analysis" || a == "--stdout-frame")
        throw Error(2, "specify exactly one of --stdout-analysis or "
                       "--stdout-frame");
      if (a == "--no-png" || a == "--emit-analysis")
        throw Error(2, a + " is not applicable to stdout modes");
      if (i + 1 >= argc)
        throw Error(2, "option value missing");
      std::string value = argv[++i];
      if (a == "--white-balance")
        wb = parse_wb(value);
      else if (a == "--converter")
        converter = value;
      else if (a == "--exposure-ev")
        exposure_ev = parse_ev(value);
      else if (a == "--analysis-roi") {
        if (!want_analysis)
          throw Error(2, "--analysis-roi requires --stdout-analysis");
        roi = parse_roi(value);
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
    // A closed consumer pipe must surface as a write error, not a SIGPIPE
    // death: no record, artifact or success claim is published.
    std::signal(SIGPIPE, SIG_IGN);
    Temp temp;
    Prepared prepared = prepare(temp, input, wb, converter, timeout);
    FrameOutput frame =
        render(prepared, std::optional<fs::path>(), roi, exposure_ev,
               want_analysis, !want_analysis);
    temp.clear();
    write_all(kStdoutMagic, sizeof kStdoutMagic);
    write_record_text(kRecordMetadata, frame.metadata + "\n");
    if (want_analysis) {
      if (!frame.analysis)
        throw Error(50, "analysis document missing");
      write_record_text(kRecordAnalysis, *frame.analysis + "\n");
    } else {
      if (frame.pixels.size() !=
          size_t(frame.width) * size_t(frame.height) * 3)
        throw Error(50, "pixel geometry mismatch");
      Sha256 hasher;
      for_each_rgb48le_chunk(frame.pixels, [&](const unsigned char *data,
                                               size_t length) {
        hasher.update(data, length);
      });
      Json descriptor{{"width", int(frame.width)},
                      {"height", int(frame.height)},
                      {"pixel_format", "rgb48le"},
                      {"pixels_sha256", hasher.hex()}};
      write_record_text(kRecordFrame, descriptor.text);
      write_record_header(kRecordPixels, uint64_t(frame.pixels.size()) * 2);
      for_each_rgb48le_chunk(frame.pixels, [](const unsigned char *data,
                                              size_t length) {
        write_all(data, length);
      });
    }
    if (std::fflush(stdout) != 0 || std::ferror(stdout))
      throw Error(50, "stdout write failed");
    std::cerr << "decoded " << frame.width << 'x' << frame.height
              << " (stdout-" << (want_analysis ? "analysis" : "frame")
              << ")\n";
    return 0;
  } catch (const Error &e) {
    std::cerr << e.what() << '\n';
    return e.code;
  } catch (const std::exception &e) {
    std::cerr << e.what() << '\n';
    return 50;
  }
}
