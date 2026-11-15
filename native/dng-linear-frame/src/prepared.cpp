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
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <set>
#include <sstream>
#include <unistd.h>
namespace frame {
namespace {
constexpr const char *kContextMagic = "rawtl-prepared-frame/2";
constexpr size_t kContextLimit = 65536;
constexpr size_t kDocumentLimit = 1024 * 1024;

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
      if (((hi << 4) | lo) == 0) throw Error(26, "prepared document contains NUL");
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

Context parse_context(const std::string &text, const char *magic = kContextMagic, size_t limit = kContextLimit) {
  if (text.empty() || text.back() != '\n')
    throw Error(26, "prepared document is truncated (missing final LF)");
  if (text.size() > limit)
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
  if (line() != magic)
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

std::string read_bounded(const fs::path &path, size_t limit) {
  check_cancelled();
  std::ifstream file(path, std::ios::binary);
  if (!file) throw Error(26, "cannot read prepared document: " + path.string());
  std::string text(limit + 1, '\0');
  file.read(text.data(), text.size());
  text.resize(size_t(file.gcount()));
  if (file.bad() || text.size() > limit)
    throw Error(26, "prepared document exceeds limit or cannot be read: " + path.string());
  return text;
}
void atomic_document(const fs::path &path, const std::string &text, size_t limit) {
  check_cancelled();
  if (text.size() > limit) throw Error(26, "prepared document exceeds size limit");
  auto staged = path;
  staged += ".tmp-" + std::to_string(getpid());
  std::ofstream file(staged, std::ios::binary | std::ios::trunc);
  if (!file) throw Error(26, "cannot write prepared document: " + staged.string());
  file << text;
  file.close();
  if (!file) throw Error(26, "cannot write prepared document: " + staged.string());
  check_cancelled();
  std::error_code error;
  fs::rename(staged, path, error);
  if (error) throw Error(26, "cannot publish prepared document: " + path.string() + ": " + error.message());
}
void field(std::ostream &out, const std::string &key, const std::string &value) {
  out << key << '=' << escape(value) << '\n';
}
std::string item(size_t i, const std::string &key) {
  return "item" + std::to_string(i) + "_" + key;
}
// No reader checks other frames' artifact existence: Delete owns individual
// DNGs; this immutable record lives for the entire scratch lifetime.
void write_conversion_record(std::vector<PreparedFrame> &frames,
                             const std::vector<size_t> &group,
                             const fs::path &record) {
  auto &info = frames[group.front()].conversion;
  std::ostringstream out;
  out << "rawtl-conversion/1\n";
  field(out, "executable", info.executable);
  field(out, "version", info.version);
  field(out, "argc", std::to_string(info.command.size()));
  for (size_t i = 0; i < info.command.size(); ++i)
    field(out, "arg" + std::to_string(i), info.command[i]);
  field(out, "exit_code", "0");
  field(out, "transcript", info.transcript);
  field(out, "directory", info.directory);
  field(out, "count", std::to_string(group.size()));
  for (size_t i = 0; i < group.size(); ++i) {
    check_cancelled();
    auto &f = frames[group[i]];
    field(out, item(i, "source"), f.source.string());
    field(out, item(i, "source_sha256"), f.source_hash);
    field(out, item(i, "output_name"), f.conversion.output_name);
    field(out, item(i, "output_sha256"), f.prepared_hash);
  }
  atomic_document(record, out.str(), kDocumentLimit);
  auto hash = sha256(record);
  for (size_t i : group) {
    frames[i].conversion.record_path = record;
    frames[i].conversion.record_hash = hash;
  }
}
void read_conversion_record(PreparedFrame &frame, const Context &context) {
  auto &info = frame.conversion;
  info.record_path = context.at("converter_record");
  info.record_hash = hex64(context.at("converter_record_sha256"), "converter_record_sha256");
  if (!info.record_path.is_absolute()) throw Error(26, "conversion record path must be absolute");
  std::string text = read_bounded(info.record_path, kDocumentLimit);
  Sha256 hash; hash.update(text.data(), text.size());
  if (hash.hex() != info.record_hash) throw Error(26, "conversion record hash mismatch");
  auto record = parse_context(text, "rawtl-conversion/1", kDocumentLimit);
  auto count = parse_u32(record.at("count"), "count");
  auto argc = parse_u32(record.at("argc"), "argc");
  if (!count || count > 64 || argc != count + 6) throw Error(26, "conversion record count/argc out of range");
  std::set<std::string> allowed{"executable", "version", "argc", "exit_code", "transcript", "directory", "count"};
  size_t bytes = 0;
  for (size_t i = 0; i < argc; ++i) {
    auto key = "arg" + std::to_string(i); allowed.insert(key);
    info.command.push_back(record.at(key));
    bytes += record.at(key).size() + 1 + sizeof(char *);
  }
  if (bytes > 65536) throw Error(26, "conversion record argv exceeds byte limit");
  info.executable = record.at("executable");
  info.version = record.at("version");
  info.exit_code = int(parse_u32(record.at("exit_code"), "exit_code"));
  info.transcript = record.at("transcript");
  info.directory = record.at("directory");
  if (info.exit_code || info.transcript.size() > 65792 ||
      info.command[0] != info.executable || info.command[1] != "-c" ||
      info.command[2] != "-l" || info.command[3] != "-d" ||
      info.command[4] != info.directory || info.command[5] != "-mp" ||
      !fs::path(info.directory).is_absolute())
    throw Error(26, "conversion record invocation mismatch");
  bool matched = false;
  std::set<std::string> sources, names;
  for (size_t i = 0; i < count; ++i) {
    check_cancelled();
    for (const char *key : {"source", "source_sha256", "output_name", "output_sha256"}) allowed.insert(item(i,key));
    auto source = record.at(item(i,"source"));
    auto source_hash = hex64(record.at(item(i,"source_sha256")), "source_sha256");
    auto name = record.at(item(i,"output_name"));
    auto output_hash = hex64(record.at(item(i,"output_sha256")), "output_sha256");
    if (!fs::path(source).is_absolute() || info.command[i+6] != source ||
        name != fs::path(source).stem().string() + ".dng" ||
        !sources.insert(source).second || !names.insert(output_collision_key(source)).second)
      throw Error(26, "conversion record source/output identity mismatch");
    if (source == context.at("source")) {
      if (source_hash != frame.source_hash || name != info.output_name ||
          output_hash != info.output_sha256 ||
          fs::path(context.at("prepared_dng")) != fs::path(info.directory) / name)
        throw Error(26, "conversion record does not describe prepared source/output");
      matched = true;
    }
  }
  record.require_only(allowed);
  if (!matched) throw Error(26, "conversion record does not contain prepared source");
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

std::vector<PreparedFrame> prepare_frames(const std::vector<fs::path> &sources,
                            const fs::path &scratch_in, const std::string &converter,
                            int timeout, bool temporary) {
  auto started = std::chrono::steady_clock::now();
  if (sources.empty() || sources.size() > 64) throw Error(26, "batch count must be 1..64");
  auto scratch = canonical_existing(scratch_in, "scratch directory");
  if (!fs::is_directory(scratch)) throw Error(26, "prepare scratch is not a directory");
  std::vector<PreparedFrame> frames;
  // Identify every input before starting the first actual conversion.
  for (const auto &source : sources) {
    check_cancelled();
    PreparedFrame out;
    out.source = canonical_existing(source, "original source");
    out.source_hash = sha256(out.source);
    out.routing = inspect(out.source);
    out.kind = out.routing ? route(out.routing->photometric) : Route::vendor_raw;
    if (out.kind != Route::vendor_raw) out.own = read_neutral(out.source);
    out.prepared_dng = out.source;
    frames.push_back(out);
  }
  std::vector<std::vector<size_t>> groups;
  const size_t budget = converter_argv_budget();
  // Greedy ordered partition. Any collision or exhausted byte budget starts a
  // new serial call, with a different output directory and independent timeout.
  std::set<std::string> names;
  size_t bytes = 0;
  auto base_bytes = [&](size_t index) {
    auto dir = scratch / (index == 0 ? "linear" : "linear-" + std::to_string(index));
    size_t n = 0;
    for (const auto &arg : std::vector<std::string>{converter,"-c","-l","-d",dir.string(),"-mp"}) n += arg.size()+1+sizeof(char *);
    return n;
  };
  for (size_t i = 0; i < frames.size(); ++i) {
    if (frames[i].kind == Route::direct) continue;
    auto key = output_collision_key(frames[i].source);
    size_t n = frames[i].source.string().size()+1+sizeof(char *);
    if (groups.empty() || names.count(key) || bytes+n > budget) {
      groups.emplace_back(); names.clear(); bytes = base_bytes(groups.size()-1);
    }
    if (bytes+n > budget) throw Error(26, "single source exceeds Converter argv byte budget: " + frames[i].source.string());
    groups.back().push_back(i); names.insert(key); bytes += n;
  }
  auto conversion_start = std::chrono::steady_clock::now();
  for (size_t g = 0; g < groups.size(); ++g) {
    check_cancelled();
    auto dir = scratch / (g == 0 ? "linear" : "linear-" + std::to_string(g));
    if (!fs::create_directory(dir)) throw Error(26, "prepare scratch already contains a linear directory: " + dir.string());
    std::vector<fs::path> inputs;
    for (size_t i : groups[g]) inputs.push_back(frames[i].source);
    auto converted = convert_inputs(inputs, dir, converter, timeout, temporary);
    for (size_t j = 0; j < groups[g].size(); ++j) {
      auto &out = frames[groups[g][j]];
      out.prepared_dng = canonical_existing(converted[j].output, "prepared DNG");
      out.conversion = converted[j].info;
      out.generated = true;
      out.prepared_hash = out.conversion.output_sha256;
    }
    write_conversion_record(frames, groups[g], scratch / ("conversion-" + std::to_string(g) + ".record"));
  }
  auto verification_start = std::chrono::steady_clock::now();
  for (auto &out : frames) {
    check_cancelled();
    if (out.kind == Route::vendor_raw) out.own = read_neutral(out.prepared_dng);
    auto check = inspect(out.prepared_dng);
    if (!check || check->photometric != 34892 || !check->width || !check->height)
      throw Error(23, "batch prepared DNG is not nonzero LinearRaw (34892): " + out.source.string());
    if (!out.generated) out.prepared_hash = out.source_hash;
    out.main_ifd_width = check->width;
    out.main_ifd_height = check->height;
    out.capture_time = read_capture_time(out.kind == Route::vendor_raw ? out.prepared_dng : out.source);
  }
  auto end = std::chrono::steady_clock::now();
  auto seconds = [](auto a, auto b) { return std::chrono::duration<double>(b-a).count(); };
  std::cerr << "prepare batch count=" << frames.size() << " adc_calls=" << groups.size()
            << " conversion_and_output_verification_s=" << seconds(conversion_start,verification_start)
            << " metadata_verification_s=" << seconds(verification_start,end)
            << " total_s=" << seconds(started,end) << '\n';
  return frames;
}
PreparedFrame prepare_frame(const fs::path &source, const fs::path &scratch,
                            const std::string &converter, int timeout, bool temporary) {
  return prepare_frames({source}, scratch, converter, timeout, temporary).front();
}

void prepare_batch(const fs::path &request, const fs::path &scratch_in,
                   const fs::path &result_in, const std::string &converter, int timeout) {
  auto started = std::chrono::steady_clock::now();
  auto scratch = canonical_existing(scratch_in, "batch scratch");
  auto doc = parse_context(read_bounded(request,kDocumentLimit), "rawtl-prepare-request/1", kDocumentLimit);
  size_t count = parse_u32(doc.at("count"), "count");
  if (!count || count > 64) throw Error(26, "batch count must be 1..64");
  std::set<std::string> allowed{"count"}, targets;
  std::set<uint32_t> ids;
  std::vector<fs::path> sources, contexts;
  auto output_path = [&](const fs::path &p) {
    if (!p.is_absolute()) throw Error(26, "batch output paths must be absolute");
    auto resolved = fs::weakly_canonical(p);
    auto relative = resolved.lexically_relative(scratch);
    if (relative.empty() || *relative.begin() == ".." || resolved == scratch)
      throw Error(26, "batch outputs must be inside scratch");
    auto first = relative.begin()->string();
    if (first == "linear" || first.starts_with("linear-") || first.starts_with("conversion-"))
      throw Error(26, "batch output overlaps reserved conversion scratch");
    if (!targets.insert(resolved.string()).second || fs::exists(resolved))
      throw Error(26, "batch output target already exists or is duplicated: " + resolved.string());
    return resolved;
  };
  auto result = output_path(result_in);
  for (size_t i = 0; i < count; ++i) {
    for (auto key : {"id","source","context"}) allowed.insert(item(i,key));
    auto id = parse_u32(doc.at(item(i,"id")), "id");
    if (!ids.insert(id).second) throw Error(26, "batch request repeats frame id");
    fs::path source = doc.at(item(i,"source"));
    if (!source.is_absolute()) throw Error(26, "batch source must be absolute");
    sources.push_back(canonical_existing(source,"original source"));
    contexts.push_back(output_path(doc.at(item(i,"context"))));
  }
  doc.require_only(allowed);
  for (const auto &source : sources)
    if (targets.count(source.string()) || fs::weakly_canonical(request) == source)
      throw Error(26, "batch output/request must be distinct from source");
  for (const auto &path : contexts)
    if (fs::weakly_canonical(request) == path) throw Error(26, "batch context must differ from request");
  auto frames = prepare_frames(sources,scratch,converter,timeout,false);
  std::ostringstream out;
  out << "rawtl-prepare-result/1\ncount=" << count << '\n';
  for (size_t i = 0; i < count; ++i) {
    check_cancelled();
    write_prepared(frames[i], contexts[i]);
    field(out,item(i,"id"),doc.at(item(i,"id")));
    field(out,item(i,"source"),frames[i].source.string());
    field(out,item(i,"source_sha256"),frames[i].source_hash);
    field(out,item(i,"context"),contexts[i].string());
  }
  // This rename is the only batch success marker; contexts alone are not results.
  atomic_document(result,out.str(),kDocumentLimit);
  check_cancelled();
  std::cerr << "prepared batch result=" << result << " count=" << count
            << " total_including_publication_s=" << std::chrono::duration<double>(std::chrono::steady_clock::now()-started).count() << '\n';
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
  if (same(prepared.source) || same(prepared.prepared_dng) ||
      (prepared.conversion.used && same(prepared.conversion.record_path)))
    throw Error(2, "prepared context path must be distinct from the source "
                   "and prepared DNG");
  std::ostringstream out;
  out << kContextMagic << '\n';
  field(out, "source", prepared.source.string());
  field(out, "source_sha256", prepared.source_hash);
  field(out, "route", route_name(prepared.kind));
  field(out, "prepared_dng", prepared.prepared_dng.string());
  field(out, "prepared_sha256", prepared.prepared_hash);
  field(out, "generated", prepared.generated ? "1" : "0");
  field(out, "main_ifd_width", std::to_string(prepared.main_ifd_width));
  field(out, "main_ifd_height", std::to_string(prepared.main_ifd_height));
  field(out, "as_shot_neutral", number(prepared.own[0]) + "," +
                               number(prepared.own[1]) + "," +
                               number(prepared.own[2]));
  field(out, "has_capture_time", prepared.capture_time ? "1" : "0");
  if (prepared.capture_time)
    field(out, "capture_time", *prepared.capture_time);
  const ConversionInfo &conversion = prepared.conversion;
  field(out, "converter_used", conversion.used ? "1" : "0");
  if (conversion.used) {
    field(out, "converter_record", conversion.record_path.string());
    field(out, "converter_record_sha256", conversion.record_hash);
    field(out, "converter_output_name", conversion.output_name);
    field(out, "converter_output_sha256", conversion.output_sha256);
    field(out, "converter_photometric", std::to_string(conversion.photometric));
    field(out, "converter_cleanup", conversion.cleanup);
  }
  atomic_document(context_path, out.str(), kContextLimit);
}

PreparedFrame read_prepared(const fs::path &context_path) {
  std::error_code error;
  if (!fs::is_regular_file(context_path, error) || error)
    throw Error(26, "prepared context is missing: " + context_path.string());
  Context context = parse_context(read_bounded(context_path, kContextLimit));

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
    for (const char *key : {"converter_record", "converter_record_sha256",
                            "converter_output_name", "converter_output_sha256",
                            "converter_photometric", "converter_cleanup"}) allowed.insert(key);
    conversion.output_name = context.at("converter_output_name");
    conversion.output_sha256 = hex64(context.at("converter_output_sha256"), "converter_output_sha256");
    conversion.photometric = parse_u32(context.at("converter_photometric"), "converter_photometric");
    conversion.cleanup = context.at("converter_cleanup");
    if (conversion.photometric != 34892) throw Error(26, "prepared context converter output is not LinearRaw");
    // Association is checked after the prepared artifact's own structural checks.
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
  if (out.conversion.used != out.generated) throw Error(26, "prepared context converter_used disagrees with generated");
  if (out.generated) read_conversion_record(out, context);
  return out;
}

Decoded decode_prepared(const PreparedFrame &prepared,
                        const std::optional<Vec> &wb,
                        CameraRecipe recipe) {
  Decoded decoded = decode(prepared.prepared_dng,
                           wb ? wb : std::optional<Vec>(prepared.own), recipe.method, recipe.correction);
  if (recipe.calibration == CameraCalibration::canon_normal_white_fixed_v1) {
    if (prepared.kind != Route::vendor_raw) throw Error(40, "Canon calibration requires original Canon CR3");
    calibrate_canon_cr3(prepared.source, decoded);
  }
  const char *correction = recipe.correction == ProfileCorrection::none ? "none" : "hue-sat-map";
  const char *calibration = recipe.calibration == CameraCalibration::none ? "none" : "canon-normal-white-fixed-v1";
  decoded.color.text.pop_back();
  decoded.color.text += ",\"recipe_requested\":" + Json(recipe.name).text +
    ",\"recipe_effective\":" + Json(recipe.name).text +
    ",\"profile_correction_requested\":" + Json(correction).text +
    ",\"profile_correction_effective\":" + Json(correction).text +
    ",\"camera_calibration_requested\":" + Json(calibration).text +
    ",\"camera_calibration_effective\":" + Json(calibration).text + "}";
  return decoded;
}
} // namespace frame
