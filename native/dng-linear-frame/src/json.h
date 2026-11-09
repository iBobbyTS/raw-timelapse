#pragma once
#include <cmath>
#include <iomanip>
#include <map>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>
namespace frame {
// A write-only JSON value: the runtime never parses untrusted JSON.
struct Json {
  std::string text = "null";
  Json() = default;
  Json(const char *s) : Json(std::string(s)) {}
  Json(const std::string &s) {
    std::ostringstream o;
    o << '"';
    for (unsigned char c : s) {
      if (c == '"' || c == '\\')
        o << '\\' << c;
      else if (c < 32)
        o << "\\u" << std::hex << std::setw(4) << std::setfill('0')
          << unsigned(c) << std::dec;
      else
        o << c;
    }
    o << '"';
    text = o.str();
  }
  Json(double n) {
    if (!std::isfinite(n))
      throw std::runtime_error("nonfinite JSON number");
    std::ostringstream o;
    o << std::setprecision(17) << n;
    text = o.str();
  }
  Json(int n) : Json(double(n)) {}
  Json(bool b) { text = b ? "true" : "false"; }
  Json(const std::vector<Json> &a) {
    text = "[";
    for (auto &v : a) {
      if (text.size() > 1)
        text += ",";
      text += v.text;
    }
    text += "]";
  }
  Json(std::initializer_list<std::pair<const std::string, Json>> fields) {
    text = "{";
    for (auto &[k, v] : fields) {
      if (text.size() > 1)
        text += ",";
      text += Json(k).text + ":" + v.text;
    }
    text += "}";
  }
};
} // namespace frame
