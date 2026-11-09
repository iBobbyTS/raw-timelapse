#include "frame.h"
#include <algorithm>
#include <cmath>
namespace frame {
Mat multiply(const Mat &a, const Mat &b) {
  Mat c{};
  for (int i = 0; i < 3; ++i)
    for (int j = 0; j < 3; ++j)
      for (int k = 0; k < 3; ++k)
        c[i][j] += a[i][k] * b[k][j];
  return c;
}
Vec multiply(const Mat &a, const Vec &b) {
  Vec c{};
  for (int i = 0; i < 3; ++i)
    for (int k = 0; k < 3; ++k)
      c[i] += a[i][k] * b[k];
  return c;
}
Mat inverse(const Mat &m) {
  Mat a = m, b{{{1, 0, 0}, {0, 1, 0}, {0, 0, 1}}};
  for (int j = 0; j < 3; ++j) {
    int p = j;
    for (int i = j + 1; i < 3; ++i)
      if (std::abs(a[i][j]) > std::abs(a[p][j]))
        p = i;
    if (std::abs(a[p][j]) < 1e-12)
      throw Error(40, "singular color matrix");
    std::swap(a[p], a[j]);
    std::swap(b[p], b[j]);
    double d = a[j][j];
    for (int k = 0; k < 3; ++k) {
      a[j][k] /= d;
      b[j][k] /= d;
    }
    for (int i = 0; i < 3; ++i)
      if (i != j) {
        double x = a[i][j];
        for (int k = 0; k < 3; ++k) {
          a[i][k] -= x * a[j][k];
          b[i][k] -= x * b[j][k];
        }
      }
  }
  return b;
}
Mat adaptation(Vec from, Vec to) {
  const Mat bradford{{{0.8951, 0.2664, -0.1614},
                      {-0.7502, 1.7135, 0.0367},
                      {0.0389, -0.0685, 1.0296}}};
  auto a = multiply(bradford, from), b = multiply(bradford, to);
  Mat scale{};
  for (int i = 0; i < 3; ++i) {
    if (a[i] <= 0 || b[i] <= 0)
      throw Error(40, "invalid adaptation white");
    scale[i][i] = b[i] / a[i];
  }
  return multiply(inverse(bradford), multiply(scale, bradford));
}
Mat d50_to_ap1() { // AP1 primaries (.713,.293), (.165,.830), (.128,.044); D60
                   // (.32168,.33767).
  const Mat ap1_to_xyz{
      {{0.6624541811085053, 0.1340042064564331, 0.1561876870049078},
       {0.2722287167809145, 0.6740817658111483, 0.0536895174079371},
       {-0.0055746494903940, 0.0040607335289828, 1.0103391003129971}}};
  Vec d50{.3457 / .3585, 1, (1 - .3457 - .3585) / .3585},
      d60{.32168 / .33767, 1, (1 - .32168 - .33767) / .33767};
  return multiply(inverse(ap1_to_xyz), adaptation(d50, d60));
}
uint16_t quantize(float v) {
  if (!std::isfinite(v))
    throw Error(40, "nonfinite scene sample");
  if (v <= 0)
    return 0;
  if (v >= 65535.0f / 16384.0f)
    return 65535;
  return static_cast<uint16_t>(std::floor(double(v) * 16384.0 + 0.5));
}
} // namespace frame
