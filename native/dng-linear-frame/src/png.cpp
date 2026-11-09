#include "frame.h"
#include <cstdio>
#include <memory>
#include <png.h>
namespace frame {
void write_png(const fs::path &path, uint32_t w, uint32_t h,
               const std::vector<uint16_t> &pixels) {
  std::unique_ptr<FILE, decltype(&fclose)> f(fopen(path.c_str(), "wb"), fclose);
  if (!f)
    throw Error(50, "PNG open failed");
  png_structp png =
      png_create_write_struct(PNG_LIBPNG_VER_STRING, nullptr, nullptr, nullptr);
  if (!png)
    throw Error(50, "PNG allocation failed");
  png_infop info = png_create_info_struct(png);
  if (!info) {
    png_destroy_write_struct(&png, nullptr);
    throw Error(50, "PNG info allocation failed");
  }
  // Allocate C++ objects before setjmp; libpng's longjmp never skips their
  // construction.
  std::vector<unsigned char> bytes(size_t(w) * h * 6);
  for (size_t i = 0; i < pixels.size(); ++i) {
    bytes[2 * i] = pixels[i] >> 8;
    bytes[2 * i + 1] = pixels[i] & 255;
  }
  std::vector<png_bytep> rows(h);
  for (uint32_t y = 0; y < h; ++y)
    rows[y] = bytes.data() + size_t(y) * w * 6;
  if (setjmp(png_jmpbuf(png))) {
    png_destroy_write_struct(&png, &info);
    throw Error(50, "PNG write failed");
  }
  png_init_io(png, f.get());
  png_set_IHDR(png, info, w, h, 16, PNG_COLOR_TYPE_RGB, PNG_INTERLACE_NONE,
               PNG_COMPRESSION_TYPE_DEFAULT, PNG_FILTER_TYPE_DEFAULT);
  // No sRGB/gamma/ICC claims: numeric scene-linear AP1 is described in sidecar.
  png_set_compression_level(png, 6);
  png_set_filter(png, PNG_FILTER_TYPE_BASE, PNG_FILTER_NONE);
  png_write_info(png, info);
  png_write_image(png, rows.data());
  png_write_end(png, info);
  png_destroy_write_struct(&png, &info);
  if (fflush(f.get()) != 0 || ferror(f.get()))
    throw Error(50, "PNG flush failed");
}
} // namespace frame
