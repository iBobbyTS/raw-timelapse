#include "lens_identity.h"
#include <iostream>
#include <stdexcept>
using namespace frame::lens::identity_detail;
void require(bool value, const char *message) { if (!value) throw std::runtime_error(message); }
int main() {
 try {
  for (const auto *text : {"", "123", "35mm F2", "18-70mm f/3.5-4.5 AF-S", "Unknown 24-85mm", "N/A",
       "Canon Lens or Sigma Lens", "A Series Lens", "Sigma Lens (0x131)", "Built-in or Manual Lens", "0x88", "123 (0x7b)", "MF 35mm F2", "VR 35mm F2", "AF 35mm F2"})
    require(!precise_lens_name(text), "numeric/specification/ambiguous identity accepted");
  for (const auto *text : {"Known Lens", "EF35mm f/2", "E 20mm F2.8", "AF-S 50mm F1.4 G", "50mm F1.4 DG HSM | Art 014", "Schneider LS 80mm f/2.8",
       "Olympus Zuiko Digital 14-54mm F2.8-3.5"})
    require(precise_lens_name(text), "concrete model identity rejected");
  Tags mode_only{{"ExifIFD:LensModel","MF 35mm F2"},{"display:Composite:LensID","Canon EF 35mm f/2"}};
  require(resolve(mode_only).first=="Canon EF 35mm f/2","focus-mode specification suppressed Composite identity");
  Tags shared{{"Canon:LensType","136"},{"display:Canon:LensType","Canon EF 300mm f/2.8L USM"},{"display:Composite:LensID","Tamron SP 15-30mm f/2.8 Di VC USD"}};
  require(resolve(shared).first=="Tamron SP 15-30mm f/2.8 Di VC USD","shared numeric ID overrode disambiguated identity");
  shared.erase("display:Composite:LensID");
  require(resolve(shared).first.empty(),"shared numeric ID display label became sole identity");
  Tags tags{{"PhaseOne:LensModel", "Schneider LS 80mm f/2.8"},
            {"display:Composite:LensID", "Other Derived Name"}};
  require(resolve(tags).first == "Schneider LS 80mm f/2.8" &&
          resolve(tags).second == "model-field:PhaseOne:LensModel", "authoritative field not preferred");
  tags = {{"display:Canon:LensType", "Sigma 14-24mm or other Sigma Lens"},
          {"display:Composite:LensID", "Sigma 50mm f/1.4 DG HSM | A"}};
  require(resolve(tags).first == "Sigma 50mm f/1.4 DG HSM | A", "disambiguated composite identity omitted");
  tags = {{"Canon:LensType", "180"}, {"Composite:LensID", "180"}, {"Sony:LensSpec", "35mm F2"}};
  require(resolve(tags).first.empty(), "raw numeric ID/specification became a model name");
  tags = {{"ExifIFD:LensModel", "Known Lens"}, {"ExifIFD:LensModel.duplicate.1", "Other Lens"}};
  bool rejected = false; try { resolve(tags); } catch (const std::exception &) { rejected = true; }
  require(rejected, "conflicting consumed scalar ignored");
  std::cout << "PASS explicit/vendor model priority, numeric/specification/ambiguity rejection and decoded identity provenance\n";
 } catch(const std::exception &e) { std::cerr << e.what() << '\n'; return 1; }
}
