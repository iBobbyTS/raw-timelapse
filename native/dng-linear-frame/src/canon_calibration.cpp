#include "frame.h"
#include <libraw/libraw.h>
#include <algorithm>
#include <cctype>
namespace frame {
double canon_normal_white_gain(double maximum, double black, double white,
                               double dng_white, double dng_black) {
  if (!(black >= 0 && black < white && white <= maximum && white > maximum / 2 &&
        maximum >= 255 && maximum <= 65535 && dng_white == 65535 && std::isfinite(dng_black)))
    throw Error(40, "invalid Canon NormalWhite calibration endpoints");
  auto range = static_cast<uint32_t>(maximum + 1);
  if (range != maximum + 1 || (range & (range - 1)))
    throw Error(40, "Canon ADC maximum must identify a power-of-two code range");
  double scale = (dng_white + 1) / (maximum + 1);
  if (std::abs(dng_black - black * scale) > scale + 1e-6)
    throw Error(40, "converted DNG black/white scale does not match Canon RAW metadata");
  return 1.8 * (dng_white - dng_black) / (scale * (white - black));
}
void calibrate_canon_cr3(const fs::path &source, Decoded &d) {
  std::string ext = source.extension().string();
  std::transform(ext.begin(), ext.end(), ext.begin(), [](unsigned char c){return std::tolower(c);});
  if (ext != ".cr3") throw Error(40, "fixed Canon calibration requires original CR3 metadata");
  LibRaw raw;
  if (raw.open_file(source.c_str()) != LIBRAW_SUCCESS)
    throw Error(40, "cannot read original Canon CR3 calibration metadata");
  if (std::string(raw.imgdata.idata.make) != "Canon")
    throw Error(40, "fixed Canon calibration requires a Canon camera");
  const auto &c = raw.imgdata.color;
  const auto &canon = raw.imgdata.makernotes.canon;
  double black = std::max(c.black, unsigned(canon.AverageBlackLevel));
  double white = canon.NormalWhiteLevel, maximum = c.maximum;
  d.reference_gain = canon_normal_white_gain(maximum, black, white, d.storage_white, d.storage_black);
  d.exposure_calibration = Json{{"applied", true}, {"method", "canon-normal-white-fixed-v1"},
    {"source", "original CR3; LibRaw metadata only, no LibRaw pixel decode"},
    {"metadata_reader_version", LibRaw::version()}, {"adc_maximum", maximum},
    {"black", black}, {"normal_white", white}, {"dng_white", d.storage_white},
    {"dng_black", d.storage_black}, {"storage_scale", (d.storage_white+1)/(maximum+1)},
    {"normal_white_gain", d.reference_gain/1.8}, {"fixed_scene_gain", 1.8},
    {"native_scene_gain", d.reference_gain}, {"scene_to_reflection", 0.9},
    {"auto_tone_normalization", false}, {"baseline_exposure_applied", false}};
}
}
