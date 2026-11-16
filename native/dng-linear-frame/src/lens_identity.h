#pragma once
#include <algorithm>
#include <cctype>
#include <map>
#include <stdexcept>
#include <string>
#include <utility>
namespace frame::lens::identity_detail {
using Tags = std::map<std::string, std::string>;
inline std::string tag_value(const Tags &tags, const std::string &key) {
  if (tags.count(key + ".duplicate.1"))
    throw std::runtime_error("conflicting consumed lens metadata: " + key);
  auto it = tags.find(key);
  return it == tags.end() ? "" : it->second;
}
inline bool precise_lens_name(const std::string &name) {
  std::string lower;
  for(unsigned char c:name) lower+=char(std::tolower(c));
  for(const auto *marker:{"unknown","n/a"," or ","manual lens","series lens",
                          "other lens","sigma lens (","tamron lens (","none"})
    if(lower.find(marker)!=std::string::npos)return false;
  const size_t first=lower.find_first_not_of(" \t\r\n");
  if(first==std::string::npos||lower.compare(first,2,"0x")==0)return false;
  const bool focal_units=lower.find("mm")!=std::string::npos &&
      std::any_of(lower.begin(),lower.end(),[](unsigned char c){return std::isdigit(c);});
  if(!std::isalpha(static_cast<unsigned char>(lower[first]))&&!focal_units)return false;
  // Classify complete alphabetic tokens; never erase unit letters inside EF,
  // Summicron, etc. A leading mount/model designator (E, Z...) plus a focal
  // specification is a name, while naked mm/f/AF-S/G specifications are not.
  auto model_token=[&](const std::string &token,size_t start) {
    const bool af_s_family=start==first&&focal_units&&token=="af"&&
        lower.compare(first,4,"af-s")==0&&
        (first+4==lower.size()||std::isspace(static_cast<unsigned char>(lower[first+4]))||
         std::isdigit(static_cast<unsigned char>(lower[first+4])));
    if(af_s_family)return true;
    for(const auto *unit:{"mm","f","mmf","af","mf","afs","afd","s","g","d","vr","x"})
      if(token==unit)return false;
    return token.size()>=2 || (start==first&&focal_units);
  };
  std::string token;size_t start=0;
  for(size_t i=0;i<=lower.size();++i) {
    if(i<lower.size()&&std::isalpha(static_cast<unsigned char>(lower[i]))) {
      if(token.empty())start=i;
      token+=lower[i];
    } else if(!token.empty()) {
      if(model_token(token,start))return true;
      token.clear();
    }
  }
  return false;
}
inline std::pair<std::string, std::string> resolve(const Tags &tags) {
  for (const auto *key : {"ExifIFD:LensModel", "Sony:LensModel", "Canon:LensModel",
                          "XMP-exifEX:LensModel", "Nikon:LensModel", "Panasonic:LensModel",
                          "Olympus:LensModel", "FujiFilm:LensModel", "Pentax:LensModel",
                          "Samsung:LensModel", "PhaseOne:LensModel", "Leica:LensModel"}) {
    auto value = tag_value(tags, key);
    if (precise_lens_name(value))
      return {value, std::string("model-field:") + key};
  }
  // Composite resolution can disambiguate shared vendor IDs using the other
  // captured lens fields. A vendor ID's display label is not independent proof:
  // even a single displayed name (e.g. Canon 136) can hide shared IDs.
  auto composite = tag_value(tags, "display:Composite:LensID");
  if (precise_lens_name(composite))
    return {composite, "decoded-identity:display:Composite:LensID"};
  // Without a disambiguated result, accept a factory string only when it is
  // already a concrete name in the numeric/raw extraction, never a decoded ID.
  for (const auto *key : {"Panasonic:LensType", "Olympus:LensType", "Leica:LensType",
                          "Samsung:LensType", "Pentax:LensType", "Minolta:LensType",
                          "Sony:LensType", "Canon:LensType"}) {
    auto value = tag_value(tags, key);
    if (precise_lens_name(value))
      return {value, std::string("vendor-name:") + key};
  }
  return {"", "no-unambiguous-model-identity"};
}
} // namespace frame::lens::identity_detail
