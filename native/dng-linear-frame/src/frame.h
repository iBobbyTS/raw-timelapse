#pragma once
#include "json.h"
#include <array>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>
namespace frame {
namespace fs = std::filesystem;
struct Error : std::runtime_error {
  int code;
  Error(int c, const std::string &m) : std::runtime_error(m), code(c) {}
};
using Vec = std::array<double, 3>;
using Mat = std::array<Vec, 3>;
struct Routing {
  uint32_t ifd;
  uint32_t photometric;
};
// Recognized DNG (classic TIFF structure declaring DNGVersion) metadata.
// nullopt means the file is not a recognized DNG; it is resolved by the
// configured converter instead. Malformed recognized DNGs still throw.
std::optional<Routing> inspect(const fs::path &);
Vec read_neutral(const fs::path &);
// EXIF DateTimeOriginal from the main IFD or its Exif sub-IFD; null when absent.
std::optional<std::string> read_capture_time(const fs::path &);
// direct: LinearRaw main IFD. converter: CFA main IFD. vendor_raw: not a
// recognized DNG, so the converter is the sole arbiter of the input.
enum class Route { direct, converter, vendor_raw };
Route route(uint32_t photometric);
Mat inverse(const Mat &);
Mat multiply(const Mat &, const Mat &);
Vec multiply(const Mat &, const Vec &);
Mat adaptation(Vec from, Vec to);
Mat d50_to_ap1();
uint16_t quantize(float);
struct Decoded {
  uint32_t width = 0, height = 0;
  std::vector<float> camera;
  Vec as_shot{}, applied{};
  Mat camera_to_d50{}, camera_to_ap1{};
  Json color, semantics;
};
Decoded decode(const fs::path &, const std::optional<Vec> &);
struct ProcessResult {
  int exit_code;
  bool timed_out;
};
ProcessResult run_process(const std::vector<std::string> &, const fs::path &log,
                          int timeout_seconds);
std::string sha256(const fs::path &);
void write_png(const fs::path &, uint32_t, uint32_t,
               const std::vector<uint16_t> &);
// Incremental SHA-256 over caller-supplied bytes (same authority as sha256()).
// hex() finalizes the digest; call it once, after every update().
class Sha256 {
public:
  Sha256();
  ~Sha256();
  Sha256(const Sha256 &) = delete;
  Sha256 &operator=(const Sha256 &) = delete;
  void update(const void *data, size_t length);
  std::string hex();

private:
  struct State;
  std::unique_ptr<State> state_;
};
} // namespace frame
