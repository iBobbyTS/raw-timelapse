#pragma once
#include "lens.h"
#include <filesystem>
#include <map>
#include <string>
#include <vector>
namespace frame::lens {
// Prepare-only profile adapter. Numeric kernel never discovers files.
struct ProfileSample {
  double focal = 0, aperture = 0, focus = 0;
  std::array<Model, 3> model;
};
struct Profile {
  std::string make, camera, lens, path, fingerprint;
  double crop = 1, aspect = 1.5;
  bool raw = false, unsupported = false;
  std::vector<ProfileSample> samples;
};
Profile read_profile(const std::filesystem::path &);
std::array<Model, 3> interpolate_profile(const Profile &, double focal,
                                         double fnumber, double focus,
                                         double crop);
std::vector<std::filesystem::path> default_profile_roots();
// Per Prepare invocation: deterministic index, lazy full parse. No global
// cache.
class ProfileCatalog {
public:
  explicit ProfileCatalog(std::vector<std::filesystem::path> roots);
  std::array<Model, 3> resolve(const Calibration &, double crop, double aspect,
                               std::string &reason);

private:
  std::vector<std::filesystem::path> roots_;
  std::map<std::string, std::vector<std::filesystem::path>> index_;
  bool indexed_ = false;
};
} // namespace frame::lens
