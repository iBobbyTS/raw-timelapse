#include "frame.h"
#include <iostream>
int main(int argc, char **argv) {
  try {
    if (argc != 2)
      return 2;
    auto d = frame::decode(argv[1], std::nullopt);
    std::vector<frame::Json> pixels;
    for (float x : d.camera)
      pixels.emplace_back(double(x));
    frame::Json out{
        {"camera", pixels}, {"color", d.color}, {"semantics", d.semantics}};
    std::cout << out.text << '\n';
    return 0;
  } catch (const frame::Error &e) {
    std::cerr << e.what() << '\n';
    return e.code;
  }
}
