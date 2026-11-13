#include "dng_flags.h"
#include "analysis.h"
#include "frame.h"
#include <bit>
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

struct FrameOutput {
  uint32_t width = 0, height = 0;
  std::string metadata;                // without the trailing newline
  std::optional<std::string> analysis; // without the trailing newline
  std::vector<uint16_t> pixels;        // empty unless requested
  std::vector<float> floats;
};

// Gain, quantization, photometry and the metadata/analysis JSON documents.
// The analysis_sidecar field is the requested positional path or null.
FrameOutput render(const PreparedFrame &prepared, const Decoded &d,
                   const std::optional<fs::path> &analysis_sidecar,
                   const analysis::Roi &roi, double exposure_ev,
                   bool want_analysis, bool need_pixels, bool f32 = false,
                   bool consumed_context = false, bool wb_override = false) {
  const double gain = std::exp2(exposure_ev);
  std::vector<uint16_t> pixels;
  if (need_pixels && !f32)
    pixels.resize(d.camera.size());
  std::vector<float> gained_scene;
  if (want_analysis || f32)
    gained_scene.resize(d.camera.size());
  double low = INFINITY, high = -INFINITY;
  size_t negatives = 0, upper = 0;
  for (size_t i = 0; i < d.camera.size(); i += 3) {
    Vec camera{d.camera[i], d.camera[i + 1], d.camera[i + 2]};
    Vec scene = d.calibrated_scene.empty()
                    ? multiply(d.camera_to_ap1, camera)
                    : Vec{d.calibrated_scene[i], d.calibrated_scene[i + 1],
                          d.calibrated_scene[i + 2]};
    for (int c = 0; c < 3; ++c) {
      float x = static_cast<float>(scene[c] * d.reference_gain * gain);
      if (!std::isfinite(x))
        throw Error(40, "nonfinite color result");
      low = std::min(low, double(x));
      high = std::max(high, double(x));
      negatives += x < 0;
      upper += x > 65535.0f / 16384.0f;
      if (want_analysis || f32)
        gained_scene[i + c] = x;
      if (need_pixels && !f32)
        pixels[i + c] = quantize(x);
    }
  }
  std::optional<analysis::Analysis> measured;
  if (want_analysis)
    measured = analysis::measure(d, gained_scene, roi, exposure_ev);
  Json metadata{
      {"schema_version", 1},
      {"color_contract_version", 1},
      {"linear_pixel_format", f32 ? "rgbf32le" : "rgb48le"},
      {"source", prepared.source.string()},
      {"source_sha256", prepared.source_hash},
      {"source_main_ifd_offset",
       prepared.routing ? Json(int(prepared.routing->ifd)) : Json()},
      {"source_photometric",
       prepared.routing ? Json(int(prepared.routing->photometric)) : Json()},
      {"route", route_name(prepared.kind)},
      {"converter", prepared.conversion.record()},
      {"prepared", Json{{"schema_version", 1},
                        {"dng", prepared.prepared_dng.string()},
                        {"sha256", prepared.prepared_hash},
                        {"generated", prepared.generated},
                        {"consumed_context", consumed_context}}},
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
       wb_override ? Json("override") : Json("as-shot")},
      {"color", [&] { Json v = d.color; v.text.pop_back();
        v.text += ",\"exposure_calibration\":" + d.exposure_calibration.text + "}"; return v; }()},
      {"decode_semantics", d.semantics},
      {"observed_scene_min", low},
      {"observed_scene_max", high},
      {"quantization",
       f32 ? Json{{"dtype", "f32"}, {"pixel_format", "rgbf32le"},
                  {"negative_policy", "preserve"}, {"upper_policy", "preserve"},
                  {"negative_samples", double(negatives)},
                  {"above_legacy_upper_samples", double(upper)},
                  {"transfer", "linear; no OETF"}} : Json{{"scale", 16384},
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
  if (f32) out.floats = std::move(gained_scene);
  return out;
}

} // namespace
int main(int argc, char **argv) {
  using namespace frame;
  install_stop_handlers();
  try {
    // The stdout grammar is flag-first: only argv[1] selects a streaming or
    // prepare mode, so the positional path slots stay paths even when a path
    // is spelled like a mode flag.
    const std::string mode = argc >= 2 ? std::string(argv[1]) : std::string();
    const bool prepare_mode = mode == "--prepare";
    const bool stdout_mode = mode == "--stdout-analysis" ||
                             mode == "--stdout-frame" ||
                             mode == "--stdout-frame-f32";
    const std::string default_converter =
        "/Applications/Adobe DNG "
        "Converter.app/Contents/MacOS/Adobe DNG Converter";

    if (prepare_mode) {
      if (argc < 3)
        throw Error(2, "usage: dng-linear-frame --prepare <source> --scratch "
                       "DIR --emit-prepared PATH [--converter PATH] "
                       "[--converter-timeout SECONDS]");
      fs::path input = argv[2];
      fs::path scratch, context;
      bool scratch_set = false, context_set = false;
      std::string converter = default_converter;
      int timeout = 120;
      for (int i = 3; i < argc; ++i) {
        std::string a = argv[i];
        if (i + 1 >= argc)
          throw Error(2, "option value missing");
        std::string value = argv[++i];
        if (a == "--scratch") {
          scratch = value;
          scratch_set = true;
        } else if (a == "--emit-prepared") {
          context = value;
          context_set = true;
        } else if (a == "--converter")
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
      if (!scratch_set)
        throw Error(2, "--prepare requires --scratch");
      if (!context_set)
        throw Error(2, "--prepare requires --emit-prepared");
      PreparedFrame prepared =
          prepare_frame(input, scratch, converter, timeout, false);
      check_cancelled();
      write_prepared(prepared, context);
      std::cerr << "prepared " << prepared.source << " -> "
                << prepared.prepared_dng << " context=" << context << '\n';
      return 0;
    }

    if (!stdout_mode) {
      if (argc < 4)
        throw Error(2, "usage: dng-linear-frame <input.dng> <output.png> "
                       "<output.json> [--white-balance R,G,B] [--converter "
                       "PATH] [--converter-timeout SECONDS] [--exposure-ev EV] "
                       "[--emit-analysis PATH] [--analysis-roi X,Y,W,H] "
                       "[--no-png]; or dng-linear-frame --stdout-analysis "
                       "<input.dng> | --stdout-frame <input.dng> | "
                       "--stdout-frame-f32 <input.dng> with the applicable "
                       "options [--camera-color-method "
                       "legacy-inverse-cm|dng-camera-to-pcs]; or "
                       "dng-linear-frame --prepare <source> --scratch DIR "
                       "--emit-prepared PATH");
      fs::path input = fs::canonical(argv[1]),
               png = fs::weakly_canonical(argv[2]),
               json = fs::weakly_canonical(argv[3]);
      if (input == png || input == json || png == json ||
          (fs::exists(png) && fs::equivalent(input, png)) ||
          (fs::exists(json) && fs::equivalent(input, json)))
        throw Error(2, "input and output paths must be distinct");
      std::optional<Vec> wb;
      std::string converter = default_converter;
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
      PreparedFrame prepared =
          prepare_frame(input, temp.path, converter, timeout, true);
      Decoded decoded = decode_prepared(
          prepared, wb, CameraColorMethod::legacy_inverse_cm);
      FrameOutput frame =
          render(prepared, decoded,
                 analysis_path.empty()
                     ? std::optional<fs::path>()
                     : std::optional<fs::path>(analysis_path),
                 roi, exposure_ev, !analysis_path.empty(), !no_png, false,
                 false, wb.has_value());
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

    // ---- stdout streaming consumers ---------------------------------------
    // argv[1] is the mode flag. argv[2] is either the source path or the
    // literal --prepared followed by a prepared-context path; options follow.
    const bool want_analysis = mode == "--stdout-analysis";
    const bool f32 = mode == "--stdout-frame-f32";
    CameraColorMethod method = CameraColorMethod::legacy_inverse_cm;
    if (argc < 3)
      throw Error(2, "option value missing");
    const bool prepared_mode = std::string(argv[2]) == "--prepared";
    fs::path input, context;
    int option_start = 3;
    if (prepared_mode) {
      if (argc < 4)
        throw Error(2, "--prepared requires a context path");
      context = argv[3];
      option_start = 4;
    } else {
      input = fs::canonical(argv[2]);
    }
    std::optional<Vec> wb;
    std::string converter = default_converter;
    int timeout = 120;
    double exposure_ev = 0;
    analysis::Roi roi;
    for (int i = option_start; i < argc; ++i) {
      std::string a = argv[i];
      if (a == "--stdout-analysis" || a == "--stdout-frame" ||
          a == "--stdout-frame-f32")
        throw Error(2, "specify exactly one of --stdout-analysis or "
                       "--stdout-frame");
      if (a == "--no-png" || a == "--emit-analysis")
        throw Error(2, a + " is not applicable to stdout modes");
      if (a == "--prepared")
        throw Error(2, "--prepared may be given once and replaces the source "
                       "path");
      if (i + 1 >= argc)
        throw Error(2, "option value missing");
      std::string value = argv[++i];
      if (a == "--white-balance")
        wb = parse_wb(value);
      else if (a == "--exposure-ev")
        exposure_ev = parse_ev(value);
      else if (a == "--camera-color-method") {
        if (value == "dng-camera-to-pcs")
          method = CameraColorMethod::dng_camera_to_pcs;
        else if (value == "legacy-inverse-cm")
          method = CameraColorMethod::legacy_inverse_cm;
        else
          throw Error(2, "unknown camera color method");
      } else if (a == "--analysis-roi") {
        if (!want_analysis)
          throw Error(2, "--analysis-roi requires --stdout-analysis");
        roi = parse_roi(value);
      } else if (a == "--converter" || a == "--converter-timeout") {
        if (prepared_mode)
          throw Error(2, a + " is not applicable to a prepared context; the "
                             "Converter ran during --prepare");
        if (a == "--converter")
          converter = value;
        else {
          size_t end = 0;
          try {
            timeout = std::stoi(value, &end);
          } catch (...) {
            throw Error(2, "invalid timeout");
          }
          if (end != value.size() || timeout < 1 || timeout > 3600)
            throw Error(2, "timeout must be 1..3600 seconds");
        }
      } else
        throw Error(2, "unknown option: " + a);
    }
    // A closed consumer pipe must surface as a write error, not a SIGPIPE
    // death: no record, artifact or success claim is published.
    std::signal(SIGPIPE, SIG_IGN);
    std::optional<Temp> temp;
    PreparedFrame prepared;
    if (prepared_mode) {
      prepared = read_prepared(context);
    } else {
      temp.emplace();
      prepared = prepare_frame(input, temp->path, converter, timeout, true);
    }
    Decoded decoded = decode_prepared(prepared, wb, method);
    FrameOutput frame =
        render(prepared, decoded, std::optional<fs::path>(), roi, exposure_ev,
               want_analysis, !want_analysis, f32, prepared_mode,
               wb.has_value());
    if (temp)
      temp->clear();
    check_cancelled();
    write_all(f32 ? "RAWTLST2" : kStdoutMagic, sizeof kStdoutMagic);
    write_record_text(kRecordMetadata, frame.metadata + "\n");
    if (want_analysis) {
      if (!frame.analysis)
        throw Error(50, "analysis document missing");
      write_record_text(kRecordAnalysis, *frame.analysis + "\n");
    } else if (f32) {
      if (frame.floats.size() != size_t(frame.width) * frame.height * 3)
        throw Error(50, "FP32 geometry mismatch");
      auto chunks = [&](auto sink) {
        std::vector<unsigned char> bytes;
        for (size_t start = 0; start < frame.floats.size(); start += kPixelChunk) {
          size_t count = std::min(kPixelChunk, frame.floats.size() - start);
          bytes.resize(count * 4);
          for (size_t i = 0; i < count; ++i) {
            uint32_t bits = std::bit_cast<uint32_t>(frame.floats[start+i]);
            for (int b = 0; b < 4; ++b) bytes[i*4+b] = (bits >> (b*8)) & 255;
          }
          sink(bytes.data(), bytes.size());
        }
      };
      Sha256 hash;
      chunks([&](const auto *data, size_t n) { hash.update(data, n); });
      Json descriptor{{"width", int(frame.width)}, {"height", int(frame.height)},
        {"pixel_format", "rgbf32le"}, {"byte_length", double(frame.floats.size()*4)},
        {"color_contract_version", 1}, {"space", "AP1/D60"}, {"transfer", "linear"},
        {"pixels_sha256", hash.hex()}};
      write_record_text(kRecordFrame, descriptor.text);
      write_record_header(kRecordPixels, uint64_t(frame.floats.size())*4);
      chunks([](const auto *data, size_t n) { write_all(data, n); });
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
