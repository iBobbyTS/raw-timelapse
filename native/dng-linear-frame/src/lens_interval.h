#pragma once
// Internal interval automatic differentiation for certifying a bounded image
// domain. Pixel evaluation uses the same map templates with ordinary doubles.
#include <algorithm>
#include <array>
#include <cmath>
#include <limits>
namespace frame::lens::domain {
struct Interval {
  double lo = 0, hi = 0;
  Interval() = default;
  Interval(double v) : lo(v), hi(v) {}
  Interval(double a, double b) : lo(a), hi(b) {}
};
inline double down(double v) { return std::nextafter(v, -std::numeric_limits<double>::infinity()); }
inline double up(double v) { return std::nextafter(v, std::numeric_limits<double>::infinity()); }
inline Interval operator+(Interval a, Interval b) { return {down(a.lo+b.lo), up(a.hi+b.hi)}; }
inline Interval operator-(Interval a, Interval b) { return {down(a.lo-b.hi), up(a.hi-b.lo)}; }
inline Interval operator*(Interval a, Interval b) {
  const std::array<double,4> v{a.lo*b.lo,a.lo*b.hi,a.hi*b.lo,a.hi*b.hi};
  if (std::any_of(v.begin(), v.end(), [](double x) { return std::isnan(x); }))
    return {-std::numeric_limits<double>::infinity(), std::numeric_limits<double>::infinity()};
  return {down(*std::min_element(v.begin(),v.end())),up(*std::max_element(v.begin(),v.end()))};
}
inline Interval operator/(Interval a, Interval b) {
  if (b.lo <= 0 && b.hi >= 0)
    return {-std::numeric_limits<double>::infinity(), std::numeric_limits<double>::infinity()};
  return a * Interval(down(1/b.hi), up(1/b.lo));
}
inline Interval square(Interval a) {
  const double low = a.lo <= 0 && a.hi >= 0 ? 0 : std::min(a.lo*a.lo,a.hi*a.hi);
  return {std::max(0.,down(low)),up(std::max(a.lo*a.lo,a.hi*a.hi))};
}
struct Jet {
  Interval value, dx, dy;
  Jet() = default;
  Jet(double v) : value(v) {}
  Jet(Interval v, Interval x, Interval y) : value(v), dx(x), dy(y) {}
};
inline Jet operator+(const Jet &a, const Jet &b) { return {a.value+b.value,a.dx+b.dx,a.dy+b.dy}; }
inline Jet operator-(const Jet &a, const Jet &b) { return {a.value-b.value,a.dx-b.dx,a.dy-b.dy}; }
inline Jet operator*(const Jet &a, const Jet &b) {
  return {a.value*b.value,a.dx*b.value+a.value*b.dx,a.dy*b.value+a.value*b.dy};
}
inline Jet operator/(const Jet &a, const Jet &b) {
  return {a.value/b.value,(a.dx*b.value-a.value*b.dx)/square(b.value),
          (a.dy*b.value-a.value*b.dy)/square(b.value)};
}
inline Jet squared(const Jet &a) { return {square(a.value),2*a.value*a.dx,2*a.value*a.dy}; }
inline double squared(double a) { return a*a; }
inline Jet radius(const Jet &x, const Jet &y) {
  const auto sum = squared(x)+squared(y);
  Interval r{std::max(0.,down(std::sqrt(std::max(0.,sum.value.lo)))),up(std::sqrt(std::max(0.,sum.value.hi)))};
  return {r,sum.dx/(2*r),sum.dy/(2*r)};
}
inline double radius(double x, double y) { return std::hypot(x,y); }
// Point derivatives use the same map with no interval rounding in the hot path.
struct Point {
  double value = 0, dx = 0, dy = 0;
  Point() = default;
  Point(double v) : value(v) {}
  Point(double v, double x, double y) : value(v), dx(x), dy(y) {}
};
inline Point operator+(Point a, Point b) { return {a.value+b.value,a.dx+b.dx,a.dy+b.dy}; }
inline Point operator-(Point a, Point b) { return {a.value-b.value,a.dx-b.dx,a.dy-b.dy}; }
inline Point operator*(Point a, Point b) {
  return {a.value*b.value,a.dx*b.value+a.value*b.dx,a.dy*b.value+a.value*b.dy};
}
inline Point operator/(Point a, Point b) {
  return {a.value/b.value,(a.dx*b.value-a.value*b.dx)/(b.value*b.value),
          (a.dy*b.value-a.value*b.dy)/(b.value*b.value)};
}
inline Point squared(Point a) { return {a.value*a.value,2*a.value*a.dx,2*a.value*a.dy}; }
inline Point radius(Point x, Point y) {
  const double r = std::hypot(x.value,y.value);
  return {r,r ? (x.value*x.dx+y.value*y.dx)/r : 0,
            r ? (x.value*x.dy+y.value*y.dy)/r : 0};
}
} // namespace frame::lens::domain
