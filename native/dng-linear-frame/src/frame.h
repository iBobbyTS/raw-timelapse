#pragma once
#include "json.h"
#include "highlight.h"
#include "lens.h"
#include "lens_metadata.h"
#include <array>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <vector>
namespace frame {
void install_stop_handlers();
void check_cancelled();
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
  // Main RAW IFD storage dimensions. These are the full sensor-IFD geometry,
  // not the SDK active-area output size; decode reports the latter.
  uint32_t width = 0;
  uint32_t height = 0;
};
// Recognized DNG (classic TIFF structure declaring DNGVersion) metadata.
// nullopt means the file is not a recognized DNG; it is resolved by the
// configured converter instead. Malformed recognized DNGs still throw.
// Coverage uses the lens component order: vignette, distortion, lateral CA.
// True also covers operations whose effect on a component cannot be excluded.
std::optional<Routing> inspect(const fs::path &, std::array<bool, 3> *lens_coverage = nullptr);
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
  // Calibrated, unexposed AP1 samples when a profile HueSatMap is present.
  // Keep camera separately for the sensor-domain saturation mask.
  std::vector<float> calibrated_scene;
  // Owned camera-linear delivery chain. Colour consumers do not know which
  // upstream image modules ran; measurement always retains camera observations.
  std::vector<float> delivery_camera;
  Json highlight_recovery = Json{{"requested", false}, {"status", "off"}, {"revision", 1}};
  Json lens_correction;
  Vec as_shot{}, applied{};
  Mat camera_to_d50{}, camera_to_ap1{};
  Json color, semantics;
  double storage_black = -1, storage_white = -1, reference_gain = 1;
  Json exposure_calibration = Json{{"applied", false}, {"method", "none"}, {"status", "uncalibrated"}};
};
enum class CameraColorMethod { legacy_inverse_cm, dng_camera_to_pcs };
enum class CameraCalibration { none, canon_normal_white_fixed_v1 };
struct CameraRecipe {
  CameraColorMethod method = CameraColorMethod::legacy_inverse_cm;
  CameraCalibration calibration = CameraCalibration::none;
  std::string name = "legacy-test";
  bool highlight_recovery = false;
};
Decoded decode(const fs::path &, const std::optional<Vec> &,
               CameraRecipe = CameraRecipe{}, void (*cancel)() = nullptr,
               lens::Request lens_request = {}, uint32_t preview_edge = 0);
double canon_normal_white_gain(double maximum, double black, double white,
                               double dng_white, double dng_black);
void calibrate_canon_cr3(const fs::path &, Decoded &);
struct ProcessResult {
  int exit_code;
  bool timed_out;
};
ProcessResult run_process(const std::vector<std::string> &, const fs::path &log,
                          int timeout_seconds, std::optional<fs::path> stdout_path = std::nullopt,
                          const std::function<void(int)> &observe = {});
// One verified output plus a reference to its shared invocation record.
struct ConversionInfo {
  bool used = false;
  fs::path record_path;
  std::string record_hash;
  std::string executable;
  std::string version;
  std::vector<std::string> command;
  int exit_code = 0;
  std::string transcript;
  std::string output_name;
  std::string output_sha256;
  uint32_t photometric = 0;
  std::string directory;
  std::string cleanup;
  Json record() const;
};
struct Conversion {
  fs::path output;
  ConversionInfo info;
};
// Inputs have distinct normalized stem names. The caller owns the empty dir.
std::vector<Conversion> convert_inputs(const std::vector<fs::path> &inputs,
                         const fs::path &linear_dir,
                         const std::string &converter, int timeout_seconds,
                         bool temporary,
                         const std::function<void(size_t, const fs::path &)> &ready = {});
// Actual platform argv/environment budget, capped for bounded record transport.
size_t converter_argv_budget();
std::string output_collision_key(const fs::path &source);
// A PreparedFrame is one source file resolved to the LinearRaw DNG the SDK may
// decode, plus the original identity every later consumer must keep using.
// The prepared path is a cache/scratch artifact and is never the source of
// Canon calibration, white balance identity or capture metadata.
struct PreparedFrame {
  fs::path source;              // canonical original input
  std::string source_hash;      // sha256 of the original bytes
  std::optional<Routing> routing; // recognized original DNG main IFD, if any
  Route kind = Route::vendor_raw;
  fs::path prepared_dng;        // canonical LinearRaw SDK input
  std::string prepared_hash;    // sha256 of prepared_dng
  bool generated = false;       // prepared_dng was produced in this run
  uint32_t main_ifd_width = 0;  // prepared main IFD storage geometry
  uint32_t main_ifd_height = 0;
  ConversionInfo conversion;    // provenance copied into frame metadata
  std::optional<std::string> capture_time;
  Vec own{};                    // original/as-shot camera neutral
  lens::Calibration lens_calibration; // frozen Prepare -> Develop contract
};
const char *route_name(Route);
// Routing + optional Converter step + identity metadata. Performs no SDK
// pixel decode. `scratch` must exist; a generated DNG is left in
// `scratch/linear/` for the caller to delete. `temporary` only controls the
// provenance cleanup wording.
PreparedFrame prepare_frame(const fs::path &source, const fs::path &scratch,
                            const std::string &converter, int timeout_seconds,
                            bool temporary, const lens::PrepareOptions &lens_options = {});
// Batch and single-frame commands use this same routing/conversion core.
std::vector<PreparedFrame> prepare_frames(const std::vector<fs::path> &sources,
                            const fs::path &scratch, const std::string &converter,
                            int timeout_seconds, bool temporary, const lens::PrepareOptions &lens_options = {},
                            const std::vector<fs::path> &early_contexts = {});
void prepare_batch(const fs::path &request, const fs::path &scratch,
                   const fs::path &result, const std::string &converter,
                   int timeout_seconds, const lens::PrepareOptions &lens_options = {});
// Deterministic versioned context document (not the metadata sidecar).
void write_prepared(const PreparedFrame &, const fs::path &context_path, bool provisional = false);
// Strictly parses the context and revalidates every original/prepared identity:
// absolute paths, hashes, route recomputation and a three-channel LinearRaw
// prepared artifact. Any mismatch is an explicit Error(26).
PreparedFrame read_prepared(const fs::path &context_path, bool provisional = false);
// SDK decode of the prepared artifact, then the unchanged fixed Canon CR3
// calibration when requested. Never invokes the Converter or deletes a file.
Decoded decode_prepared(const PreparedFrame &, const std::optional<Vec> &wb,
                        CameraRecipe recipe, bool lens_enabled = false, const lens::Policy & = {},
                        uint32_t preview_edge = 0);
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
