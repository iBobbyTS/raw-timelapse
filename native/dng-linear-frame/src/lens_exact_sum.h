#pragma once
// A sufficient exact-zero certificate for sums of finite binary products.
// Error-free TwoSum/TwoProduct retain lost low bits; an unsupported exponent,
// rounding mode or resource bound declines the identity shortcut, never the model.
#include <algorithm>
#include <cfenv>
#include <cmath>
#include <initializer_list>
#include <limits>
#include <vector>
#include <utility>
namespace frame::lens::detail {
class ExactSum {
  std::vector<double> pieces_;
  bool proved_ = std::numeric_limits<double>::is_iec559 &&
                 std::numeric_limits<double>::radix == 2 &&
                 std::fegetround() == FE_TONEAREST;
  static bool grow(std::vector<double> &pieces, double value) {
    std::vector<double> next;
    for (double part : pieces) {
      const double sum=value+part;
      if (!std::isfinite(sum)) return false;
      const double bv=sum-value, av=sum-bv;
      const double error=(value-av)+(part-bv);
      if (!std::isfinite(error)) return false;
      if (error!=0) next.push_back(error);
      value=sum;
    }
    if (value!=0) next.push_back(value);
    if (next.size()>512) return false;
    pieces=std::move(next);
    return true;
  }
public:
  void add(const std::vector<double> &factors) {
    if (!proved_) return;
    if (std::any_of(factors.begin(),factors.end(),[](double v){return !std::isfinite(v);})) {proved_=false;return;}
    if (std::find(factors.begin(),factors.end(),0.)!=factors.end()) return;
    std::vector<double> product{1};
    for (double factor:factors) {
      if (!std::isfinite(factor)) {proved_=false;return;}
      if (factor==1) continue;
      if (factor==-1) {for(auto &p:product)p=-p;continue;}
      std::vector<double> next;
      for(double part:product) {
        const double high=part*factor;
        // A 2p-bit product needs its low bits to remain representable. Outside
        // this range the proof is declined even when rounded arithmetic is zero.
        constexpr int floor=std::numeric_limits<double>::min_exponent+
                            std::numeric_limits<double>::digits;
        if (!std::isfinite(high) || high==0 || std::ilogb(std::abs(high))<floor) {
          proved_=false;return;
        }
        const double low=std::fma(part,factor,-high);
        if (!std::isfinite(low) || !grow(next,low) || !grow(next,high)) {
          proved_=false;return;
        }
      }
      product=std::move(next);
    }
    for(double part:product) if(!grow(pieces_,part)){proved_=false;return;}
  }
  void add(std::initializer_list<double> factors) {add(std::vector<double>(factors));}
  bool is_zero() const {return proved_&&pieces_.empty();}
};
} // namespace frame::lens::detail
