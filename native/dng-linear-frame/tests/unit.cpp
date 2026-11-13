#include "frame.h"
#include <cmath>
#include <iostream>
#include <stdexcept>
using namespace frame;
void check(bool v) {
  if (!v)
    throw std::runtime_error("assertion failed");
}
int main() {
  check(std::abs(canon_normal_white_gain(16383,2047,13135,65535,8188) * .9 -
                 1.62 * 57347.0 / 44352.0) < 1e-12);
  check(std::abs(canon_normal_white_gain(16383,511,12535,65535,2048) * .9 -
                 1.62 * 63487.0 / 48096.0) < 1e-12);
  for (auto bad : {0.0, 511.0, 8191.0, 20000.0}) {
    bool rejected=false; try { canon_normal_white_gain(16383,511,bad,65535,2048); }
    catch (const Error &) { rejected=true; } check(rejected);
  }
  bool mismatch=false; try { canon_normal_white_gain(16383,511,12535,65535,4096); }
  catch (const Error &) { mismatch=true; } check(mismatch);
  check(quantize(0) == 0);
  check(quantize(-.25f) == 0);
  check(quantize(1) == 16384);
  check(quantize(.125f) == 2048);
  check(quantize(65535.f / 16384.f) == 65535);
  check(quantize(5) == 65535);
  Vec camera{-.1, 1.5, 4.25};
  Mat identity{{{1, 0, 0}, {0, 1, 0}, {0, 0, 1}}};
  auto f = multiply(identity, camera);
  check(f[0] < 0 && f[1] > 1 && f[2] > 4);
  auto scene = multiply(d50_to_ap1(), camera);
  std::array<float, 3> fp32{static_cast<float>(scene[0]),
                            static_cast<float>(scene[1]),
                            static_cast<float>(scene[2])};
  check(fp32[0] < 0 && fp32[2] > 1);
  check(quantize(fp32[0]) == 0);
  check(fp32[0] < 0); // Quantization must not modify the FP32-domain input.
  check(route(34892) == Route::direct);
  check(route(32803) == Route::converter);
  try {
    route(2);
    check(false);
  } catch (const Error &e) {
    check(e.code == 22);
  }
  std::cout << "quantization and FP domain checks passed\n";
}
