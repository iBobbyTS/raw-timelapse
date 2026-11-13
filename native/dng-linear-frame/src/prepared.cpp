// Prepared-frame boundary: one source is resolved to the LinearRaw DNG the SDK
// may decode, and the original identity is carried forward verbatim. The
// context document is a strict, versioned line format rather than JSON because
// this runtime deliberately has no JSON parser; every field is scalar and
// percent-escaped, so a tampered or truncated context fails closed.
#include "frame.h"
#include <algorithm>
#include <cctype>
#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iomanip>
#include <map>
#include <set>
#include <sstream>
#include <unistd.h>
namespace frame {
namespace {
constexpr const char *kContextMagic = "rawtl-prepared-frame/1";
constexpr size_t kContextLimit = size_t(4) << 20;

std::string escape(const std::string &text) {
  static const char digits[] = "0123456789abcdef";
  std::string out;
  out.reserve(text.size());
  for (unsigned char c : text) {
    if (c == '%' || c < 32 || c == 127) {
      out += '%';
      out += digits[c >> 4];
      out += digits[c & 15];
    } else {
      out += static_cast<char>(c);
    }
  }
  return out;
}
int hex_digit(unsigned char c) {
  if (c >= '0' && c <= '9')
    return c - '0';
  if (c >= 'a' && c <= 'f')
    return c - 'a' + 10;
  if (c >= 'A' && c <= 'F')
    return c - 'A' + 10;
  return -1;
}
std::string unescape(const std::string &text) {
  std::string out;
  out.reserve(text.size());
  for (size_t i = 0; i < text.size(); ++i) {
    unsigned char c = static_cast<unsigned char>(text[i]);
    if (c == '%') {
      if (i + 2 >= text.size())
        throw Error(26, "prepared context has a truncated escape");
      int hi = hex_digit(static_cast<unsigned char>(text[i + 1]));
      int lo = hex_digit(static_cast<unsigned char>(text[i + 2]));
      if (hi < 0 || lo < 0)
        throw Error(26, "prepared context has an invalid escape");
      out += static_cast<char>((hi << 4) | lo);
      i += 2;
    } else if (c < 32 || c == 127) {
      throw Error(26, "prepared context value contains a raw control byte");
    } else {
      out += static_cast<char>(c);
    }
  }
  return out;
}

struct Context {
  std::map<std::string, std::string> fields;
  bool has(const std::string &key) const { return fields.count(key) != 0; }
  const std::string &at(const std::string &key) const {
    auto it = fields.find(key);
    if (it == fields.end())
      throw Error(26, "prepared context is missing field " + key);
    return it->second;
  }
  void require_only(const std::set<std::string> &allowed) const {
    for (const auto &[key, value] : fields)
      if (!allowed.count(key))
        throw Error(26, "prepared context has an unknown field " + key);
  }
};

Context parse_context(const std::string &text) {
  if (text.size() > kContextLimit)
    throw Error(26, "prepared context exceeds the size limit");
  size_t at = 0;
  auto line = [&]() -> std::string {
    auto end = text.find('\n', at);
    if (end == std::string::npos)
      end = text.size();
    std::string value = text.substr(at, end - at);
    at = end == text.size() ? text.size() : end + 1;
    if (!value.empty() && value.back() == '\r')
      throw Error(26, "prepared context must use LF line endings");
    return value;
  };
  if (line() != kContextMagic)
    throw Error(26, "prepared context has an unsupported header");
  Context out;
  while (at < text.size()) {
    std::string row = line();
    if (row.empty())
      throw Error(26, "prepared context has an empty field row");
    auto equals = row.find('=');
    if (equals == std::string::npos || equals == 0)
      throw Error(26, "prepared context field is malformed");
    std::string key = row.substr(0, equals);
    if (!std::all_of(key.begin(), key.end(), [](unsigned char c) {
          return (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_';
        }))
      throw Error(26, "prepared context field name is malformed");
    if (out.fields.count(key))
      throw Error(26, "prepared context repeats field " + key);
    out.fields.emplace(key, unescape(row.substr(equals + 1)));
  }
  return out;
}
uint32_t parse_u32(const std::string &text, const char *field) {
  if (text.empty() || text.size() > 10 ||
      !std::all_of(text.begin(), text.end(), [](unsigned char c) {
        return std::isdigit(c) != 0;
      }))
    throw Error(26, std::string("prepared context field ") + field +
                        " must be a non-negative integer");
  errno = 0;
  char *end = nullptr;
  unsigned long value = std::strtoul(text.c_str(), &end, 10);
  if (errno || !end || *end || value > 0xfffffffful)
    throw Error(26, std::string("prepared context field ") + field +
                        " must be a non-negative integer");
  return static_cast<uint32_t>(value);
}
double parse_double(const std::string &text, const char *field) {
  if (text.empty())
    throw Error(26, std::string("prepared context field ") + field +
                        " must be a finite number");
  errno = 0;
  char *end = nullptr;
  double value = std::strtod(text.c_str(), &end);
  if (errno || !end || *end || !std::isfinite(value))
    throw Error(26, std::string("prepared context field ") + field +
                        " must be a finite number");
  return value;
}
bool parse_bool(const std::string &text, const char *field) {
  if (text == "1")
    return true;
  if (text == "0")
    return false;
  throw Error(26, std::string("prepared context field ") + field +
                      " must be 0 or 1");
}
std::string hex64(const std::string &text, const char *field) {
  if (text.size() != 64 ||
      !std::all_of(text.begin(), text.end(), [](unsigned char c) {
        return std::isxdigit(c) != 0;
      }))
    throw Error(26, std::string("prepared context field ") + field +
                        " must be a 64-digit SHA-256");
  std::string out = text;
  std::transform(out.begin(), out.end(), out.begin(),
                 [](unsigned char c) { return std::tolower(c); });
  return out;
}
std::string number(double value) {
  std::ostringstream out;
  out << std::setprecision(17) << value;
  return out.str();
}
fs::path canonical_existing(const fs::path &path, const char *what) {
  std::error_code error;
  auto resolved = fs::canonical(path, error);
  if (error)
    throw Error(26, std::string("prepared context ") + what +
                        " cannot be resolved: " + path.string() + ": " +
                        error.message());
  return resolved;
}
Route route_from_name(const std::string &name) {
  if (name == "linearraw-sdk")
    return Route::direct;
  if (name == "cfa-converter-linearraw-sdk")
    return Route::converter;
  if (name == "vendor-raw-converter-linearraw-sdk")
    return Route::vendor_raw;
  throw Error(26, "prepared context has an unknown route " + name);
}
} // namespace

const char *route_name(Route route) {
  switch (route) {
  case Route::direct:
    return "linearraw-sdk";
  case Route::converter:
    return "cfa-converter-linearraw-sdk";
  case Route::vendor_raw:
    return "vendor-raw-converter-linearraw-sdk";
  }
  return "unknown";
}

Json ConversionInfo::record() const {
  if (!used)
    return Json{{"used", false}};
  std::vector<Json> args;
  for (const auto &argument : command)
    args.emplace_back(argument);
  return Json{{"used", true},
              {"executable", executable},
              {"version", version},
              {"command", args},
              {"exit_code", exit_code},
              {"stdout_stderr", transcript},
              {"output_name", output_name},
              {"output_sha256", output_sha256},
              {"verified_main_ifd_photometric", int(photometric)},
              {"temporary_directory", directory},
              {"cleanup", cleanup},
              {"upstream_semantics", "Converter performs CFA demosaic; detailed "
                                     "clipping/metadata baking UNKNOWN"}};
}

PreparedFrame prepare_frame(const fs::path &source_in, const fs::path &scratch,
                            const std::string &converter, int timeout,
                            bool temporary) {
  std::error_code error;
  if (!fs::is_directory(scratch, error) || error)
    throw Error(26,
                "prepare scratch directory does not exist: " + scratch.string());
  PreparedFrame out;
  out.source = canonical_existing(source_in, "original source");
  out.source_hash = sha256(out.source);
  out.routing = inspect(out.source);
  // Recognition is DNG-structural: only a recognized DNG is routed by its
  // main RAW photometric; every other file is a vendor/other RAW candidate for
  // the converter alone.
  out.kind = out.routing ? route(out.routing->photometric) : Route::vendor_raw;
  fs::path prepared = out.source;
  auto convert = [&] {
    fs::path linear = scratch / "linear";
    if (fs::exists(linear))
      throw Error(26, "prepare scratch already contains a linear directory: " +
                          linear.string());
    std::error_code make;
    if (!fs::create_directory(linear, make) || make)
      throw Error(26,
                  "cannot create prepare scratch directory: " + linear.string());
    Conversion conversion =
        convert_input(out.source, linear, converter, timeout, temporary);
    prepared = conversion.output;
    out.conversion = conversion.info;
    out.generated = true;
  };
  // "as-shot" metadata comes from the recognized DNG itself. A vendor/other
  // input is hashed as the original file, but its neutral and capture time
  // exist only in the converter's DNG output (probed: ADC preserves both).
  if (out.kind == Route::vendor_raw) {
    convert();
    out.own = read_neutral(prepared);
  } else {
    out.own = read_neutral(out.source);
    if (out.kind == Route::converter)
      convert();
  }
  out.prepared_dng = canonical_existing(prepared, "prepared DNG");
  auto check = inspect(out.prepared_dng);
  if (!check || check->photometric != 34892)
    throw Error(23, "prepared DNG main RAW IFD is not LinearRaw (34892)");
  if (out.generated == (out.prepared_dng == out.source))
    throw Error(26, "prepared DNG identity does not match its route");
  // The generated branch reuses the SHA-256 convert_input already computed
  // over the same output bytes; only a passthrough needs the original hash.
  out.prepared_hash = out.generated ? out.conversion.output_sha256
                                    : out.source_hash;
  out.main_ifd_width = check->width;
  out.main_ifd_height = check->height;
  out.capture_time = read_capture_time(
      out.kind == Route::vendor_raw ? out.prepared_dng : out.source);
  return out;
}

void write_prepared(const PreparedFrame &prepared,
                    const fs::path &context_path) {
  auto same = [&](const fs::path &other) {
    std::error_code error;
    auto left = fs::weakly_canonical(context_path, error);
    if (error)
      return false;
    auto right = fs::weakly_canonical(other, error);
    return !error && left == right;
  };
  if (same(prepared.source) || same(prepared.prepared_dng))
    throw Error(2, "prepared context path must be distinct from the source "
                   "and prepared DNG");
  std::ostringstream out;
  out << kContextMagic << '\n';
  auto field = [&](const char *key, const std::string &value) {
    out << key << '=' << escape(value) << '\n';
  };
  auto integer = [&](const char *key, uint32_t value) {
    out << key << '=' << value << '\n';
  };
  field("source", prepared.source.string());
  field("source_sha256", prepared.source_hash);
  field("route", route_name(prepared.kind));
  field("prepared_dng", prepared.prepared_dng.string());
  field("prepared_sha256", prepared.prepared_hash);
  out << "generated=" << (prepared.generated ? 1 : 0) << '\n';
  integer("main_ifd_width", prepared.main_ifd_width);
  integer("main_ifd_height", prepared.main_ifd_height);
  field("as_shot_neutral", number(prepared.own[0]) + "," +
                               number(prepared.own[1]) + "," +
                               number(prepared.own[2]));
  out << "has_capture_time=" << (prepared.capture_time ? 1 : 0) << '\n';
  if (prepared.capture_time)
    field("capture_time", *prepared.capture_time);
  const ConversionInfo &conversion = prepared.conversion;
  out << "converter_used=" << (conversion.used ? 1 : 0) << '\n';
  if (conversion.used) {
    field("converter_executable", conversion.executable);
    field("converter_version", conversion.version);
    integer("converter_argc", static_cast<uint32_t>(conversion.command.size()));
    for (size_t i = 0; i < conversion.command.size(); ++i)
      field(("converter_arg" + std::to_string(i)).c_str(),
            conversion.command[i]);
    integer("converter_exit_code", static_cast<uint32_t>(conversion.exit_code));
    field("converter_transcript", conversion.transcript);
    field("converter_output_name", conversion.output_name);
    field("converter_output_sha256", conversion.output_sha256);
    integer("converter_photometric", conversion.photometric);
    field("converter_directory", conversion.directory);
    field("converter_cleanup", conversion.cleanup);
  }
  auto staged = context_path;
  staged += ".tmp-" + std::to_string(getpid());
  {
    std::ofstream file(staged, std::ios::binary | std::ios::trunc);
    if (!file)
      throw Error(26, "cannot write prepared context: " + staged.string());
    file << out.str();
    file.close();
    if (!file)
      throw Error(26, "cannot write prepared context: " + staged.string());
  }
  std::error_code error;
  fs::rename(staged, context_path, error);
  if (error) {
    std::error_code ignored;
    fs::remove(staged, ignored);
    throw Error(26, "cannot publish prepared context: " +
                        context_path.string() + ": " + error.message());
  }
}

PreparedFrame read_prepared(const fs::path &context_path) {
  std::error_code error;
  if (!fs::is_regular_file(context_path, error) || error)
    throw Error(26, "prepared context is missing: " + context_path.string());
  std::ifstream file(context_path, std::ios::binary);
  if (!file)
    throw Error(26, "cannot read prepared context: " + context_path.string());
  std::string text{std::istreambuf_iterator<char>(file), {}};
  if (file.bad())
    throw Error(26, "cannot read prepared context: " + context_path.string());
  Context context = parse_context(text);

  const std::set<std::string> base{"source",
                                   "source_sha256",
                                   "route",
                                   "prepared_dng",
                                   "prepared_sha256",
                                   "generated",
                                   "main_ifd_width",
                                   "main_ifd_height",
                                   "as_shot_neutral",
                                   "has_capture_time",
                                   "converter_used"};
  std::set<std::string> allowed = base;
  PreparedFrame out;
  out.source_hash = hex64(context.at("source_sha256"), "source_sha256");
  out.prepared_hash = hex64(context.at("prepared_sha256"), "prepared_sha256");
  out.kind = route_from_name(context.at("route"));
  out.generated = parse_bool(context.at("generated"), "generated");
  out.main_ifd_width = parse_u32(context.at("main_ifd_width"), "main_ifd_width");
  out.main_ifd_height =
      parse_u32(context.at("main_ifd_height"), "main_ifd_height");
  auto neutral_text = context.at("as_shot_neutral");
  std::vector<std::string> parts;
  size_t start = 0;
  for (;;) {
    auto comma = neutral_text.find(',', start);
    parts.push_back(neutral_text.substr(start, comma - start));
    if (comma == std::string::npos)
      break;
    start = comma + 1;
  }
  if (parts.size() != 3)
    throw Error(26, "prepared context as_shot_neutral requires three values");
  for (int i = 0; i < 3; ++i) {
    out.own[i] = parse_double(parts[i], "as_shot_neutral");
    if (out.own[i] <= 0)
      throw Error(26, "prepared context as_shot_neutral must be positive");
  }
  bool has_capture =
      parse_bool(context.at("has_capture_time"), "has_capture_time");
  if (has_capture) {
    allowed.insert("capture_time");
    out.capture_time = context.at("capture_time");
    if (out.capture_time->empty())
      throw Error(26, "prepared context capture_time must be non-empty");
  } else if (context.has("capture_time")) {
    throw Error(
        26, "prepared context carries capture_time with has_capture_time=0");
  }
  bool converter_used =
      parse_bool(context.at("converter_used"), "converter_used");
  ConversionInfo &conversion = out.conversion;
  conversion.used = converter_used;
  if (converter_used) {
    for (const char *key : {"converter_executable", "converter_version",
                            "converter_argc", "converter_exit_code",
                            "converter_transcript", "converter_output_name",
                            "converter_output_sha256", "converter_photometric",
                            "converter_directory", "converter_cleanup"})
      allowed.insert(key);
    uint32_t argc = parse_u32(context.at("converter_argc"), "converter_argc");
    if (argc == 0 || argc > 16)
      throw Error(26, "prepared context converter_argc is out of range");
    for (uint32_t i = 0; i < argc; ++i) {
      std::string key = "converter_arg" + std::to_string(i);
      allowed.insert(key);
      conversion.command.push_back(context.at(key));
    }
    conversion.executable = context.at("converter_executable");
    conversion.version = context.at("converter_version");
    conversion.exit_code =
        int(parse_u32(context.at("converter_exit_code"), "converter_exit_code"));
    conversion.transcript = context.at("converter_transcript");
    conversion.output_name = context.at("converter_output_name");
    conversion.output_sha256 =
        hex64(context.at("converter_output_sha256"), "converter_output_sha256");
    conversion.photometric = parse_u32(context.at("converter_photometric"),
                                       "converter_photometric");
    conversion.directory = context.at("converter_directory");
    conversion.cleanup = context.at("converter_cleanup");
    if (conversion.photometric != 34892)
      throw Error(26, "prepared context converter output is not LinearRaw");
    if (conversion.exit_code != 0)
      throw Error(26, "prepared context records a failed Converter call");
  }
  context.require_only(allowed);

  if (!fs::path(context.at("source")).is_absolute() ||
      !fs::path(context.at("prepared_dng")).is_absolute())
    throw Error(26, "prepared context paths must be absolute");
  if (out.main_ifd_width == 0 || out.main_ifd_height == 0)
    throw Error(26, "prepared context geometry must be non-zero");
  // Original identity: never replaced by the cache path.
  out.source = canonical_existing(context.at("source"), "original source");
  out.prepared_dng =
      canonical_existing(context.at("prepared_dng"), "prepared DNG");
  if (sha256(out.source) != out.source_hash)
    throw Error(26, "prepared context original source hash does not match: " +
                        out.source.string());
  auto routing = inspect(out.source);
  Route expected = routing ? route(routing->photometric) : Route::vendor_raw;
  if (expected != out.kind)
    throw Error(26,
                "prepared context route does not match the original source");
  out.routing = routing;
  if (out.generated != (out.kind != Route::direct))
    throw Error(26, "prepared context generated flag does not match its route");
  if (out.kind == Route::direct) {
    if (out.prepared_dng != out.source)
      throw Error(26, "prepared context for a LinearRaw source must reference "
                      "the original DNG");
    if (out.prepared_hash != out.source_hash)
      throw Error(26, "prepared context prepared hash differs from the "
                      "original source hash");
  } else {
    if (out.prepared_dng == out.source)
      throw Error(26, "prepared context cannot use the original source as its "
                      "converted DNG");
    if (sha256(out.prepared_dng) != out.prepared_hash)
      throw Error(26, "prepared DNG hash does not match the context: " +
                          out.prepared_dng.string());
    if (out.conversion.output_sha256 != out.prepared_hash ||
        out.conversion.output_name != out.prepared_dng.filename().string())
      throw Error(26, "prepared context Converter provenance does not "
                      "describe the prepared DNG");
  }
  auto check = inspect(out.prepared_dng);
  if (!check || check->photometric != 34892)
    throw Error(26, "prepared DNG main RAW IFD is not LinearRaw (34892): " +
                        out.prepared_dng.string());
  if (check->width != out.main_ifd_width ||
      check->height != out.main_ifd_height)
    throw Error(26, "prepared context geometry does not match the prepared DNG");
  return out;
}

Decoded decode_prepared(const PreparedFrame &prepared,
                        const std::optional<Vec> &wb,
                        CameraColorMethod method) {
  Decoded decoded = decode(prepared.prepared_dng,
                           wb ? wb : std::optional<Vec>(prepared.own), method);
  if (method == CameraColorMethod::dng_camera_to_pcs &&
      prepared.kind == Route::vendor_raw)
    calibrate_canon_cr3(prepared.source, decoded);
  return decoded;
}
} // namespace frame
