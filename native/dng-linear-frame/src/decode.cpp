#include "dng_auto_ptr.h"
#include "dng_camera_profile.h"
#include "dng_color_spec.h"
#include "dng_exceptions.h"
#include "dng_file_stream.h"
#include "dng_host.h"
#include "dng_ifd.h"
#include "dng_image.h"
#include "dng_info.h"
#include "dng_linearization_info.h"
#include "dng_negative.h"
#include "dng_opcode_list.h"
#include "dng_opcodes.h"
#include "dng_pixel_buffer.h"
#include "dng_tag_types.h"
#include "frame.h"
#include <algorithm>
#include <cmath>
namespace frame {
namespace {
class FloatNegative : public dng_negative {
public:
  explicit FloatNegative(dng_host &h) : dng_negative(h) { Initialize(); }

protected:
  void DoBuildStage2(dng_host &host) override {
    // Use the SDK's own complete linearization, requesting FP32 as its output
    // rather than its default 16-bit intermediate. No manual black subtraction.
    auto *lin = const_cast<dng_linearization_info *>(GetLinearizationInfo());
    AutoPtr<dng_image> image(host.Make_dng_image(
        lin->fActiveArea.Size(), Stage1Image()->Planes(), ttFloat));
    lin->Linearize(host, *this, *Stage1Image(), *image);
    SetStage2Image(image);
  }
};
class Host : public dng_host {
public:
  std::vector<Json> dispatches;
  dng_negative *Make_dng_negative() override {
    return new FloatNegative(*this);
  }
  void ApplyOpcodeList(dng_opcode_list &list, dng_negative &n,
                       AutoPtr<dng_image> &image) override {
    std::vector<Json> ops;
    for (uint32_t i = 0; i < list.Count(); ++i) {
      const auto &op = list.Entry(i);
      ops.push_back(Json{{"id", int(op.OpcodeID())},
                         {"optional", op.Optional()},
                         {"flags", int(op.Flags())}});
    }
    dng_host::ApplyOpcodeList(list, n, image);
    dispatches.push_back(
        Json{{"stage", int(dispatches.size() + 1)},
             {"count", int(ops.size())},
             {"opcodes", ops},
             {"status", ops.empty() ? "absent"
                                    : "SDK list dispatch completed; optional "
                                      "unsupported opcodes may be skipped"}});
  }
};
Json vec(const Vec &v) { return std::vector<Json>{v[0], v[1], v[2]}; }
Json optional_matrix(const dng_matrix &m);
Json matrix(const Mat &m) {
  return std::vector<Json>{vec(m[0]), vec(m[1]), vec(m[2])};
}
Mat mat(const dng_matrix &m) {
  if (m.Rows() != 3 || m.Cols() != 3)
    throw Error(40, "only three-channel color matrices supported");
  Mat r{};
  for (int i = 0; i < 3; ++i)
    for (int j = 0; j < 3; ++j) {
      if (!std::isfinite(m[i][j]))
        throw Error(40, "nonfinite color matrix");
      r[i][j] = m[i][j];
    }
  return r;
}
Json optional_matrix(const dng_matrix &m) {
  return m.NotEmpty() ? matrix(mat(m)) : Json();
}
} // namespace
Decoded decode(const fs::path &path, const std::optional<Vec> &wb, CameraColorMethod method) {
  auto routing = inspect(path);
  if (!routing || route(routing->photometric) != Route::direct)
    throw Error(23, "CFA cannot enter SDK");
  try {
    Host host;
    host.SetForPreview(false);
    host.SetIgnoreEnhanced(true);
    dng_file_stream stream(path.c_str());
    dng_info info;
    info.Parse(host, stream);
    info.PostParse(host);
    if (!info.IsValidDNG() || info.fMainIndex < 0)
      throw Error(30, "SDK rejected DNG");
    const auto &ifd = *info.fIFD.at(info.fMainIndex);
    if (ifd.fPhotometricInterpretation != 34892 || ifd.fSamplesPerPixel != 3)
      throw Error(30, "SDK main IFD must be three-channel LinearRaw");
    AutoPtr<dng_negative> n(host.Make_dng_negative());
    n->Parse(host, stream, info);
    n->PostParse(host, stream, info);
    if (!n->HasCameraNeutral() || n->CameraNeutral().Count() != 3)
      throw Error(40, "three-channel AsShotNeutral required");
    Decoded d;
    d.as_shot = read_neutral(path);
    d.applied = wb.value_or(d.as_shot);
    for (double x : d.applied)
      if (!std::isfinite(x) || x <= 0)
        throw Error(
            40, "white balance values must be positive finite camera neutrals");
    if (!n->ProfileCount())
      throw Error(40, "ColorMatrix profile missing");
    const auto &profile = n->ProfileByIndex(0);
    dng_color_spec spec(*n, &profile);
    dng_vector neutral(3);
    double max = *std::max_element(d.applied.begin(), d.applied.end());
    for (int i = 0; i < 3; ++i)
      neutral[i] = d.applied[i] / max;
    auto white = spec.NeutralToXY(neutral);
    spec.SetWhiteXY(white);
    // Both SDK paths include the selected neutral and PCS white adaptation.
    // Legacy deliberately ignores FM; the new matrix-only strategy uses
    // CameraToPCS, including SDK's no-FM fallback. Do not add another WB/CAT.
    const bool pcs = method == CameraColorMethod::dng_camera_to_pcs;
    const bool fm = profile.ForwardMatrix1().NotEmpty() ||
                    profile.ForwardMatrix2().NotEmpty() ||
                    profile.ForwardMatrix3().NotEmpty();
    d.camera_to_d50 = pcs ? mat(spec.CameraToPCS())
                          : inverse(mat(spec.PCStoCamera()));
    if (pcs) (void)inverse(d.camera_to_d50); // Reject singular profile transforms.
    d.camera_to_ap1 = multiply(d50_to_ap1(), d.camera_to_d50);
    d.color = Json{
        {"method", pcs ? "DNG SDK CameraToPCS; matrix only" : "DNG Ch6 no-ForwardMatrix; inverse(SDK normalized AB*CC*CM*MapWhiteMatrix(D50,selectedWhite))"},
        {"selected_white_xy", std::vector<Json>{white.x, white.y}},
        {"profile_index", 0},
        {"profile_name", profile.Name().Get()},
        {"profile_fingerprint", profile.Fingerprint().ToUtf8HexString().Get()},
        {"algorithm_revision", 1},
        {"calibration_illuminants",
         std::vector<Json>{int(profile.CalibrationIlluminant1()),
                           int(profile.CalibrationIlluminant2()),
                           int(profile.CalibrationIlluminant3())}},
        {"color_matrix1", matrix(mat(profile.ColorMatrix1()))},
        {"color_matrix2", optional_matrix(profile.ColorMatrix2())},
        {"color_matrix3", optional_matrix(profile.ColorMatrix3())},
        {"analog_balance", vec(Vec{n->AnalogBalance(0), n->AnalogBalance(1),
                                   n->AnalogBalance(2)})},
        {"camera_calibration1", optional_matrix(n->CameraCalibration1())},
        {"camera_calibration2", optional_matrix(n->CameraCalibration2())},
        {"camera_calibration3", optional_matrix(n->CameraCalibration3())},
        {"camera_calibration_signature", n->CameraCalibrationSignature().Get()},
        {"profile_calibration_signature",
         profile.ProfileCalibrationSignature().Get()},
        {"sdk_normalized_pcs_to_camera", matrix(mat(spec.PCStoCamera()))},
        {"camera_to_xyz_d50", matrix(d.camera_to_d50)},
        {"xyz_d50_to_acescg", matrix(d50_to_ap1())},
        {"camera_to_acescg", matrix(d.camera_to_ap1)},
        {"cat", "linear Bradford D50->D60; AP1 inverse primaries matrix"},
        {"white_balance_normalization",
         "max camera-neutral component=1; SDK PCS-to-camera white "
         "saturation normalization"},
        {"method_requested", pcs ? "dng-camera-to-pcs" : "legacy-inverse-cm"},
        {"method_effective", pcs ? (fm ? "dng-camera-to-pcs-forward-matrix" : "dng-camera-to-pcs-inverse-fallback") : "legacy-inverse-cm"},
        {"camera_model", n->ModelName().Get()},
        {"forward_matrix_present", fm},
        {"forward_matrix1", optional_matrix(profile.ForwardMatrix1())},
        {"forward_matrix2", optional_matrix(profile.ForwardMatrix2())},
        {"forward_matrix3", optional_matrix(profile.ForwardMatrix3())},
        {"hue_sat_map_present", profile.HasHueSatDeltas()},
        {"look_table_present", profile.HasLookTable()},
        {"tone_curve_present", profile.ToneCurve().IsValid()},
        {"hue_sat_map_applied", false}, {"look_table_applied", false},
        {"tone_curve_applied", false}, {"baseline_exposure_applied", false},
        {"forward_matrix_used", pcs && fm}};
    std::vector<Json> black, white_levels;
    for (uint32_t r = 0; r < ifd.fBlackLevelRepeatRows; ++r) {
      std::vector<Json> row;
      for (uint32_t c = 0; c < ifd.fBlackLevelRepeatCols; ++c) {
        std::vector<Json> channels;
        for (int p = 0; p < 3; ++p)
          channels.emplace_back(ifd.fBlackLevel[r][c][p]);
        row.emplace_back(channels);
      }
      black.emplace_back(row);
    }
    for (int p = 0; p < 3; ++p)
      white_levels.emplace_back(ifd.fWhiteLevel[p]);
    double baseline = n->BaselineExposure();
    n->ReadStage1Image(host, stream, info);
    n->BuildStage2Image(host);
    n->BuildStage3Image(host);
    const auto *image = n->Stage3Image();
    if (!image || image->Planes() != 3 || image->PixelType() != ttFloat)
      throw Error(30, "SDK did not produce FP32 RGB");
    if (n->Stage3BlackLevel() != 0)
      throw Error(30, "unhandled residual SDK Stage3 black level");
    d.width = image->Bounds().W();
    d.height = image->Bounds().H();
    if (uint64_t(d.width) * d.height > 200000000)
      throw Error(30, "image exceeds supported allocation bound");
    d.camera.resize(size_t(d.width) * d.height * 3);
    dng_pixel_buffer buf;
    buf.fArea = image->Bounds();
    buf.fPlane = 0;
    buf.fPlanes = 3;
    buf.fRowStep = d.width * 3;
    buf.fColStep = 3;
    buf.fPlaneStep = 1;
    buf.fPixelType = ttFloat;
    buf.fPixelSize = 4;
    buf.fData = d.camera.data();
    image->Get(buf);
    auto range = std::minmax_element(d.camera.begin(), d.camera.end());
    d.semantics = Json{
        {"adapter", "SDK Stage1 -> SDK Linearize(FP32 destination) -> Stage2 "
                    "-> Stage3; no dng_render"},
        {"evidence", "tests/integration.py controlled LinearRaw probes; "
                     "docs/migration-log.md S01"},
        {"linearization_table",
         Json{{"entries", int(ifd.fLinearizationTableCount)},
              {"status", ifd.fLinearizationTableCount
                             ? "applied by SDK Linearize"
                             : "absent"}}},
        {"black_level",
         Json{
             {"values", black},
             {"delta_h_count", int(ifd.fBlackLevelDeltaHCount)},
             {"delta_v_count", int(ifd.fBlackLevelDeltaVCount)},
             {"status",
              "applied by SDK Linearize; controlled 2x2 nonzero-black probe"}}},
        {"white_level", Json{{"values", white_levels},
                             {"status", "applied by SDK Linearize"}}},
        {"opcodes", host.dispatches},
        {"baseline_exposure",
         Json{{"ev", baseline},
              {"applied", false},
              {"evidence", "controlled 0 vs 1.9 EV probe produces equal camera "
                           "samples/PNG; decode bypasses dng_render"}}},
        {"storage_bits_per_sample", int(ifd.fBitsPerSample[0])},
        {"storage_sample_format", int(ifd.fSampleFormat[0])},
        {"camera_min", double(*range.first)},
        {"camera_max", double(*range.second)},
        {"residual_stage3_black", int(n->Stage3BlackLevel())},
        {"upstream_clipping",
         "SDK integer Linearize clamps below black and above white to [0,1]; "
         "probed. Converter demosaic clipping UNKNOWN. Scene FP32 color "
         "transform is unclipped."}};
    return d;
  } catch (const dng_exception &e) {
    throw Error(30, "Adobe DNG SDK error " + std::to_string(e.ErrorCode()));
  }
}
} // namespace frame
