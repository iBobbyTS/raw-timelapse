#include "frame.h"
#include <fstream>
#include <map>
#include <set>
namespace frame {
// Read only TIFF directory metadata before giving any file to the SDK.
// Classic TIFF only; BigTIFF is rejected explicitly rather than guessed.
Routing inspect(const fs::path &path) {
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
  auto order = read(0, 2);
  if (order == 0x4949)
    little = true;
  else if (order == 0x4d4d)
    little = false;
  else
    throw Error(20, "not TIFF");
  if (read(2, 2) != 42)
    throw Error(20, "only classic TIFF DNG is supported");
  std::vector<uint32_t> pending{read(4, 4)};
  std::set<uint32_t> seen;
  bool dng = false;
  struct Candidate {
    uint32_t offset;
    std::optional<uint32_t> pi;
    uint64_t area;
  };
  std::vector<Candidate> candidates;
  while (!pending.empty()) {
    uint32_t at = pending.back();
    pending.pop_back();
    if (!at)
      continue;
    if (!seen.insert(at).second)
      throw Error(20, "cyclic/aliased TIFF IFD");
    if (seen.size() > 256)
      throw Error(20, "too many TIFF IFDs");
    auto n = read(at, 2);
    std::map<uint32_t, uint32_t> values;
    for (uint32_t i = 0; i < n; ++i) {
      uint64_t p = uint64_t(at) + 2 + 12 * i;
      auto tag = read(p, 2), type = read(p + 2, 2), count = read(p + 4, 4);
      if (tag == 50706 && count == 4 && type == 1)
        dng = true;
      if (tag != 254 && tag != 256 && tag != 257 && tag != 262 && tag != 330)
        continue;
      if ((type != 3 && type != 4 && type != 13) || count == 0 || count > 256)
        throw Error(20, "invalid TIFF routing tag");
      uint32_t bytes = type == 3 ? 2 : 4;
      uint64_t data = uint64_t(count) * bytes <= 4 ? p + 8 : read(p + 8, 4);
      if (tag == 330) {
        for (uint32_t k = 0; k < count; ++k)
          pending.push_back(read(data + k * bytes, bytes));
      } else {
        if (count != 1 || values.count(tag))
          throw Error(20, "ambiguous TIFF routing tag");
        values[tag] = read(data, bytes);
      }
    }
    pending.push_back(read(uint64_t(at) + 2 + 12 * n, 4));
    if (values[254] == 0 && values[256] > 0 && values[257] > 0)
      candidates.push_back(
          {at, values.count(262) ? std::optional(values[262]) : std::nullopt,
           uint64_t(values[256]) * values[257]});
  }
  if (!dng)
    throw Error(20, "DNGVersion tag missing");
  if (candidates.size() != 1)
    throw Error(20, "expected exactly one full-resolution main RAW IFD");
  auto c = candidates.front();
  if (!c.pi)
    throw Error(21, "main RAW IFD PhotometricInterpretation missing");
  return {c.offset, *c.pi};
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

} // namespace frame
