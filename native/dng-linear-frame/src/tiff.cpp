#include "frame.h"
#include <fstream>
#include <map>
#include <set>
namespace frame {
// Read only TIFF directory metadata before giving any file to the SDK.
// Recognition is structural: classic TIFF (magic 42) declaring DNGVersion.
// Anything else is not a DNG and is left to the converter. BigTIFF (magic 43)
// is rejected explicitly rather than guessed; a recognized DNG then goes
// through the full validation below byte-for-byte unchanged.
std::optional<Routing> inspect(const fs::path &path) {
  std::ifstream f(path, std::ios::binary);
  if (!f)
    throw Error(20, "cannot open DNG: " + path.string());
  auto size = fs::file_size(path);
  bool little = true;
  auto read = [&](uint64_t at, int n) -> uint32_t {
    if (at > size || uint64_t(n) > size - at)
      throw Error(20, "truncated TIFF metadata");
    f.seekg(at);
    unsigned char b[4]{};
    f.read((char *)b, n);
    if (!f)
      throw Error(20, "TIFF read failed");
    uint32_t v = 0;
    for (int j = 0; j < n; ++j)
      v |= uint32_t(b[j]) << (8 * (little ? j : n - 1 - j));
    return v;
  };
  auto order = [&] {
    try {
      return std::optional<uint32_t>(read(0, 2));
    } catch (const Error &) {
      return std::optional<uint32_t>(); // too short to be a TIFF container
    }
  }();
  if (!order)
    return std::nullopt;
  if (*order == 0x4949)
    little = true;
  else if (*order == 0x4d4d)
    little = false;
  else
    return std::nullopt; // not a TIFF container: not a recognized DNG
  auto magic = [&] {
    try {
      return std::optional<uint32_t>(read(2, 2));
    } catch (const Error &) {
      return std::optional<uint32_t>();
    }
  }();
  if (!magic)
    return std::nullopt;
  if (*magic == 43)
    throw Error(20, "only classic TIFF DNG is supported");
  if (*magic != 42)
    return std::nullopt; // not a recognized TIFF structure
  bool dng = false;
  struct Candidate {
    uint32_t offset;
    std::optional<uint32_t> pi;
    uint32_t width;
    uint32_t height;
    uint64_t area;
  };
  std::vector<Candidate> candidates;
  // Complete structural recognition before validating routing values. A bad
  // routing tag must not hide a later DNGVersion declaration, and a non-DNG
  // TIFF's routing semantics are left to the converter.
  std::vector<uint32_t> directories;
  {
    std::vector<uint32_t> pending{read(4, 4)};
    std::set<uint32_t> seen;
    while (!pending.empty()) {
      uint32_t at = pending.back();
      pending.pop_back();
      if (!at)
        continue;
      if (!seen.insert(at).second)
        throw Error(20, "cyclic/aliased TIFF IFD");
      if (seen.size() > 256)
        throw Error(20, "too many TIFF IFDs");
      directories.push_back(at);
      auto n = read(at, 2);
      for (uint32_t i = 0; i < n; ++i) {
        uint64_t p = uint64_t(at) + 2 + 12 * i;
        auto tag = read(p, 2), type = read(p + 2, 2), count = read(p + 4, 4);
        auto field = read(p + 8, 4);
        if (tag == 50706)
          dng = true;
        // Only interpretable SubIFD pointers participate in the graph scan.
        // Invalid types/counts are rejected by the DNG validation pass below.
        if (tag == 330 && (type == 3 || type == 4 || type == 13) &&
            count > 0 && count <= 256) {
          uint32_t bytes = type == 3 ? 2 : 4;
          uint64_t data = uint64_t(count) * bytes <= 4 ? p + 8 : field;
          for (uint32_t k = 0; k < count; ++k)
            pending.push_back(read(data + k * bytes, bytes));
        }
      }
      pending.push_back(read(uint64_t(at) + 2 + 12 * n, 4));
    }
  }
  if (!dng)
    return std::nullopt; // TIFF without DNGVersion: not a recognized DNG
  for (uint32_t at : directories) {
    auto n = read(at, 2);
    std::map<uint32_t, uint32_t> values;
    for (uint32_t i = 0; i < n; ++i) {
      uint64_t p = uint64_t(at) + 2 + 12 * i;
      auto tag = read(p, 2), type = read(p + 2, 2), count = read(p + 4, 4);
      if (tag != 254 && tag != 256 && tag != 257 && tag != 262 && tag != 330)
        continue;
      if ((type != 3 && type != 4 && type != 13) || count == 0 || count > 256)
        throw Error(20, "invalid TIFF routing tag");
      uint32_t bytes = type == 3 ? 2 : 4;
      uint64_t data = uint64_t(count) * bytes <= 4 ? p + 8 : read(p + 8, 4);
      if (tag != 330) {
        if (count != 1 || values.count(tag))
          throw Error(20, "ambiguous TIFF routing tag");
        values[tag] = read(data, bytes);
      }
    }
    if (values[254] == 0 && values[256] > 0 && values[257] > 0)
      candidates.push_back(
          {at, values.count(262) ? std::optional(values[262]) : std::nullopt,
           values[256], values[257],
           uint64_t(values[256]) * values[257]});
  }
  if (candidates.size() != 1)
    throw Error(20, "expected exactly one full-resolution main RAW IFD");
  auto c = candidates.front();
  if (!c.pi)
    throw Error(21, "main RAW IFD PhotometricInterpretation missing");
  return Routing{c.offset, *c.pi, c.width, c.height};
}
Route route(uint32_t p) {
  if (p == 34892)
    return Route::direct;
  if (p == 32803)
    return Route::converter;
  throw Error(22, "unsupported main RAW PhotometricInterpretation: " +
                      std::to_string(p));
}
Vec read_neutral(const fs::path &path) {
  std::ifstream f(path, std::ios::binary);
  auto size = fs::file_size(path);
  unsigned char header[8]{};
  f.read((char *)header, 8);
  bool le = header[0] == 'I';
  auto read = [&](uint64_t at, int n) -> uint32_t {
    if (at > size || uint64_t(n) > size - at)
      throw Error(20, "truncated neutral tag");
    f.seekg(at);
    unsigned char b[4]{};
    f.read((char *)b, n);
    if (!f)
      throw Error(20, "neutral read failed");
    uint32_t v = 0;
    for (int i = 0; i < n; ++i)
      v |= uint32_t(b[i]) << (8 * (le ? i : n - 1 - i));
    return v;
  };
  auto at = read(4, 4), n = read(at, 2);
  for (uint32_t i = 0; i < n; ++i) {
    uint64_t p = uint64_t(at) + 2 + 12 * i;
    if (read(p, 2) != 50728)
      continue;
    if (read(p + 2, 2) != 5 || read(p + 4, 4) != 3)
      throw Error(40, "AsShotNeutral must be three rationals");
    uint64_t data = read(p + 8, 4);
    Vec v{};
    for (int k = 0; k < 3; ++k) {
      auto den = read(data + 8 * k + 4, 4);
      if (!den)
        throw Error(40, "invalid AsShotNeutral denominator");
      v[k] = double(read(data + 8 * k, 4)) / den;
      if (v[k] <= 0)
        throw Error(40, "invalid AsShotNeutral");
    }
    return v;
  }
  throw Error(40, "source AsShotNeutral missing");
}
std::optional<std::string> read_capture_time(const fs::path &path) {
  // Optional metadata: malformed or unreadable Exif structures degrade to
  // "absent" and never fail the frame. Bounded reads still apply.
  std::ifstream f(path, std::ios::binary);
  if (!f)
    return std::nullopt;
  std::error_code ec;
  auto size = fs::file_size(path, ec);
  if (ec || size < 8)
    return std::nullopt;
  unsigned char order_bytes[2]{};
  f.read((char *)order_bytes, 2);
  if (!f)
    return std::nullopt;
  bool little;
  if (order_bytes[0] == 'I' && order_bytes[1] == 'I')
    little = true;
  else if (order_bytes[0] == 'M' && order_bytes[1] == 'M')
    little = false;
  else
    return std::nullopt;
  auto read = [&](uint64_t at, int n) -> std::optional<uint32_t> {
    if (at > size || uint64_t(n) > size - at)
      return std::nullopt;
    f.clear();
    f.seekg(at);
    unsigned char b[4]{};
    f.read((char *)b, n);
    if (!f)
      return std::nullopt;
    uint32_t v = 0;
    for (int i = 0; i < n; ++i)
      v |= uint32_t(b[i]) << (8 * (little ? i : n - 1 - i));
    return v;
  };
  auto magic = read(2, 2);
  if (!magic || *magic != 42)
    return std::nullopt;
  auto ascii = [&](uint64_t entry, uint32_t count) -> std::optional<std::string> {
    if (count < 1 || count > 64)
      return std::nullopt;
    uint64_t data;
    if (uint64_t(count) <= 4)
      data = entry + 8;
    else {
      auto offset = read(entry + 8, 4);
      if (!offset)
        return std::nullopt;
      data = *offset;
    }
    std::string text;
    for (uint32_t k = 0; k < count; ++k) {
      auto value = read(data + k, 1);
      if (!value)
        return std::nullopt;
      unsigned char c = static_cast<unsigned char>(*value);
      if (c == 0)
        break;
      if (c < 32 || c > 126)
        return std::nullopt;
      text.push_back(static_cast<char>(c));
    }
    if (text.empty())
      return std::nullopt;
    return text;
  };
  auto scan = [&](uint32_t ifd, uint32_t *exif) -> std::optional<std::string> {
    if (!ifd)
      return std::nullopt;
    auto n = read(ifd, 2);
    if (!n)
      return std::nullopt;
    // Accept any legal classic-TIFF directory whose count header, entries and
    // next-directory trailer all fit inside the file. A truncated or
    // out-of-range span is unreadable metadata.
    uint64_t span = 2 + uint64_t(*n) * 12 + 4;
    if (uint64_t(ifd) + span > size)
      return std::nullopt;
    std::optional<std::string> found;
    for (uint32_t i = 0; i < *n; ++i) {
      uint64_t p = uint64_t(ifd) + 2 + 12 * i;
      auto tag = read(p, 2);
      auto type = read(p + 2, 2);
      auto count = read(p + 4, 4);
      if (!tag || !type || !count)
        return std::nullopt;
      if (*tag == 36867 && *type == 2 && !found)
        found = ascii(p, *count);
      else if (*tag == 34665 && *type == 4 && *count == 1) {
        auto pointer = read(p + 8, 4);
        if (!pointer)
          return std::nullopt;
        *exif = *pointer;
      }
    }
    return found;
  };
  auto root = read(4, 4);
  if (!root)
    return std::nullopt;
  uint32_t exif = 0;
  if (auto direct = scan(*root, &exif))
    return direct;
  if (!exif)
    return std::nullopt;
  return scan(exif, &exif);
}

} // namespace frame
