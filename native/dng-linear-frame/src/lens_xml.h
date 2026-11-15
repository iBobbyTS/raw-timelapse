#pragma once
#include <filesystem>
#include <fstream>
#include <libxml/parser.h>
#include <libxml/tree.h>
#include <memory>
#include <stdexcept>
#include <string>
namespace frame::lens::xml {
struct Free {
  void operator()(xmlDoc *d) const { xmlFreeDoc(d); }
};
using Document = std::unique_ptr<xmlDoc, Free>;
inline Document parse(const std::string &text) {
  if (text.empty() || text.size() > 16 * 1024 * 1024)
    throw std::runtime_error("lens XML size exceeds bound");
  // No DTDs, entity expansion, external file/network loading, or XInclude.
  if (text.find("<!DOCTYPE") != std::string::npos ||
      text.find("<!ENTITY") != std::string::npos)
    throw std::runtime_error("lens XML DTD/entities are forbidden");
  Document d(xmlReadMemory(text.data(), int(text.size()), "lens.xml", nullptr,
                           XML_PARSE_NONET | XML_PARSE_NOBLANKS |
                               XML_PARSE_NOERROR | XML_PARSE_NOWARNING));
  if (!d || d->intSubset || d->extSubset)
    throw std::runtime_error("invalid lens XML");
  return d;
}
inline std::string read(const std::filesystem::path &p) {
  std::ifstream in(p, std::ios::binary);
  if (!in)
    throw std::runtime_error("cannot read lens metadata: " + p.string());
  std::string s;
  char b[8192];
  while (in) {
    in.read(b, sizeof b);
    s.append(b, size_t(in.gcount()));
    if (s.size() > 16 * 1024 * 1024)
      throw std::runtime_error("lens metadata too large");
  }
  return s;
}
inline std::string value(xmlNode *n) {
  auto *s = xmlNodeGetContent(n);
  std::string out = s ? reinterpret_cast<char *>(s) : "";
  xmlFree(s);
  return out;
}
inline std::string name(xmlNode *n) {
  return reinterpret_cast<const char *>(n->name);
}
inline std::string ns(xmlNode *n) {
  return n->ns && n->ns->href ? reinterpret_cast<const char *>(n->ns->href)
                              : "";
}
} // namespace frame::lens::xml
