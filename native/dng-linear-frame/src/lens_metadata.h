#pragma once
#include "lens.h"
#include <filesystem>
#include <vector>
namespace frame::lens {
struct PrepareOptions {
  bool collect = false;
  std::string exiftool = "/opt/homebrew/bin/exiftool";
  // Empty uses installed Adobe locations; explicit roots replace discovery.
  std::vector<std::filesystem::path> profile_roots;
};
struct SourcePair {
  std::filesystem::path source, prepared;
  bool converted = false;
  uint32_t width = 0, height = 0;
};
void validate_prepare_options(const PrepareOptions &);
std::vector<Calibration> collect(const std::vector<SourcePair> &,
                                 const std::filesystem::path &scratch,
                                 const PrepareOptions &);
} // namespace frame::lens
