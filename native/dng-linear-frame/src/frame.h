#pragma once
#include "json.h"
#include <array>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <stdexcept>
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
Routing inspect(const fs::path &);
Vec read_neutral(const fs::path &);
// EXIF DateTimeOriginal from the main IFD or its Exif sub-IFD; null when absent.
std::optional<std::string> read_capture_time(const fs::path &);
enum class Route { direct, converter };
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
} // namespace frame
