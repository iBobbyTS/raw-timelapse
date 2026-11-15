#include "frame.h"
#include <CommonCrypto/CommonDigest.h>
#include <CoreFoundation/CoreFoundation.h>
#include <algorithm>
#include <chrono>
#include <csignal>
#include <cstring>
#include <set>
#include <fcntl.h>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <spawn.h>
#include <sstream>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>
extern char **environ;
namespace frame {
namespace {
volatile std::sig_atomic_t cancelled = 0;
void stop_handler(int) { cancelled = 1; }
}
void install_stop_handlers() {
  std::signal(SIGINT, stop_handler);
  std::signal(SIGTERM, stop_handler);
}
void check_cancelled() {
  if (cancelled) throw Error(130, "caller cancelled native operation");
}

namespace {
// Keep the full transcript on disk, but bound the diagnostic carried by records.
std::string read_text(const fs::path &p) {
  std::ifstream f(p, std::ios::binary);
  std::string text(65536, '\0');
  f.read(text.data(), text.size());
  text.resize(size_t(f.gcount()));
  // NUL is valid UTF-8 but forbidden in protocol values. Sanitize only the
  // transported excerpt; the original converter log remains byte-for-byte.
  std::replace(text.begin(), text.end(), '\0', '?');
  CFStringRef valid = CFStringCreateWithBytes(nullptr,
      reinterpret_cast<const UInt8 *>(text.data()), text.size(), kCFStringEncodingUTF8, false);
  if (valid) CFRelease(valid);
  else for (char &c : text) if (static_cast<unsigned char>(c) >= 128) c = '?';
  if (f.peek() != EOF) text += "\n[transcript truncated; full " + p.filename().string() + " retained]";
  return text;
}
std::string converter_version(const fs::path &exe) {
  auto bundle_path = exe.parent_path().parent_path().parent_path().string();
  CFURLRef url = CFURLCreateFromFileSystemRepresentation(
      nullptr, (const UInt8 *)bundle_path.data(), bundle_path.size(), true);
  if (!url)
    return "UNKNOWN";
  CFBundleRef bundle = CFBundleCreate(nullptr, url);
  CFRelease(url);
  if (!bundle)
    return "UNKNOWN";
  CFTypeRef value = CFBundleGetValueForInfoDictionaryKey(
      bundle, CFSTR("CFBundleShortVersionString"));
  char text[256]{};
  bool ok = value && CFGetTypeID(value) == CFStringGetTypeID() &&
            CFStringGetCString((CFStringRef)value, text, sizeof text,
                               kCFStringEncodingUTF8);
  CFRelease(bundle);
  return ok ? text : "UNKNOWN";
}
} // namespace
size_t converter_argv_budget() {
  long limit = sysconf(_SC_ARG_MAX);
  size_t environment = 4096;
  for (char **p = environ; *p; ++p) environment += strlen(*p) + 1 + sizeof(char *);
  if (limit <= 0 || size_t(limit) <= environment + 4096)
    throw Error(26, "insufficient platform argv budget for Converter");
  return std::min<size_t>(65536, size_t(limit) - environment - 4096);
}
std::string output_collision_key(const fs::path &source) {
  auto name = source.stem().string() + ".dng";
  CFMutableStringRef text = CFStringCreateMutable(nullptr, 0);
  CFStringRef input = CFStringCreateWithBytes(nullptr,
      reinterpret_cast<const UInt8 *>(name.data()), name.size(), kCFStringEncodingUTF8, false);
  if (!text || !input) {
    if (text) CFRelease(text);
    if (input) CFRelease(input);
    throw Error(26, "source name is not valid UTF-8");
  }
  CFStringAppend(text, input); CFRelease(input);
  CFStringNormalize(text, kCFStringNormalizationFormD);
  CFStringFold(text, kCFCompareCaseInsensitive, nullptr);
  std::string out(CFStringGetMaximumSizeForEncoding(CFStringGetLength(text), kCFStringEncodingUTF8) + 1, '\0');
  bool ok = CFStringGetCString(text, out.data(), out.size(), kCFStringEncodingUTF8);
  CFRelease(text);
  if (!ok) throw Error(26, "cannot normalize source name");
  out.resize(strlen(out.c_str()));
  return out;
}
std::vector<Conversion> convert_inputs(const std::vector<fs::path> &inputs,
                         const fs::path &linear_dir,
                         const std::string &converter, int timeout,
                         bool temporary) {
  std::vector<std::string> command{converter, "-c", "-l", "-d", linear_dir.string(), "-mp"};
  std::set<std::string> expected;
  for (const auto &input : inputs) {
    check_cancelled();
    command.push_back(input.string());
    expected.insert(input.stem().string() + ".dng");
  }
  auto log = linear_dir.parent_path() / (linear_dir.filename().string() + "-converter.log");
  auto conversion_start = std::chrono::steady_clock::now();
  auto result = run_process(command, log, timeout);
  auto verification_start = std::chrono::steady_clock::now();
  std::string transcript = read_text(log);
  std::string identity = "batch Converter [" + inputs.front().string() + " .. " + inputs.back().string() +
      "] count=" + std::to_string(inputs.size()) + " timeout=" + std::to_string(timeout) + "s";
  if (result.timed_out || result.exit_code != 0) {
    std::cerr << transcript;
    throw Error(result.timed_out ? 25 : 24, identity +
                " failed; exit=" + std::to_string(result.exit_code));
  }
  check_cancelled();
  std::set<std::string> actual;
  for (auto &e : fs::directory_iterator(linear_dir)) {
    check_cancelled();
    if (!fs::is_regular_file(e.symlink_status()))
      throw Error(23, identity + ": unexpected non-regular output " + e.path().string());
    actual.insert(e.path().filename().string());
  }
  if (actual != expected)
    throw Error(23, identity + ": Converter output set differs from requested stems");
  std::vector<Conversion> outputs;
  for (const auto &input : inputs) {
    check_cancelled();
    auto output = linear_dir / (input.stem().string() + ".dng");
    auto check = inspect(output);
    if (!check || check->photometric != 34892 || !check->width || !check->height)
      throw Error(23, identity + ": output is not nonzero LinearRaw (34892): " + output.string());
    ConversionInfo info;
    info.used = true;
    info.executable = converter;
    info.version = converter_version(converter);
    info.command = command;
    info.exit_code = result.exit_code;
    info.transcript = transcript;
    info.output_name = output.filename().string();
    info.output_sha256 = sha256(output);
    info.photometric = check->photometric;
    info.directory = linear_dir.string();
    info.cleanup = temporary ? "removed before publishing sidecar"
                             : "caller-owned; retained until caller deletes it";
    outputs.push_back({output, info});
  }
  std::cerr << "converter batch count=" << inputs.size()
            << " source_first=" << inputs.front() << " source_last=" << inputs.back()
            << " timeout_s=" << timeout
            << " process_s=" << std::chrono::duration<double>(verification_start-conversion_start).count()
            << " output_verification_s=" << std::chrono::duration<double>(std::chrono::steady_clock::now()-verification_start).count() << '\n';
  return outputs;
}
ProcessResult run_process(const std::vector<std::string> &args,
                          const fs::path &log, int timeout, std::optional<fs::path> stdout_path) {
  check_cancelled();
  std::vector<char *> argv;
  for (auto &a : args)
    argv.push_back(const_cast<char *>(a.c_str()));
  argv.push_back(nullptr);
  posix_spawn_file_actions_t actions;
  posix_spawn_file_actions_init(&actions);
  posix_spawn_file_actions_addopen(&actions, STDOUT_FILENO, stdout_path.value_or(log).c_str(),
                                   O_WRONLY | O_CREAT | O_TRUNC, 0600);
  if (stdout_path)
    posix_spawn_file_actions_addopen(&actions, STDERR_FILENO, log.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0600);
  else
    posix_spawn_file_actions_adddup2(&actions, STDOUT_FILENO, STDERR_FILENO);
  posix_spawnattr_t attr;
  posix_spawnattr_init(&attr);
  posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP);
  posix_spawnattr_setpgroup(&attr, 0);
  pid_t pid;
  int err = posix_spawn(&pid, argv[0], &actions, &attr, argv.data(), environ);
  posix_spawnattr_destroy(&attr);
  posix_spawn_file_actions_destroy(&actions);
  if (err)
    throw Error(24, "cannot spawn child " + args.front() + ": " + std::to_string(err));
  auto start = std::chrono::steady_clock::now();
  bool expired = false, reaped = false;
  int status = 0;
  // Always terminate surviving writers on failure/cancellation, even when the
  // leader has already exited. A successful leader alone is not readiness.
  auto terminate_group = [&] {
    kill(-pid, SIGTERM);
    auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(500);
    while (std::chrono::steady_clock::now() < deadline) {
      if (!reaped && waitpid(pid, &status, WNOHANG) == pid) reaped = true;
      if (kill(-pid, 0) < 0 && errno == ESRCH) break;
      std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    kill(-pid, SIGKILL);
    if (!reaped) {
      while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
      reaped = true;
    }
  };
  for (;;) {
    if (!reaped) {
      pid_t r = waitpid(pid, &status, WNOHANG);
      if (r == pid) reaped = true;
      else if (r < 0 && errno != EINTR) {
        terminate_group();
        throw Error(24, "waitpid failed; Converter group terminated");
      }
    }
    if (cancelled) {
      terminate_group();
      throw Error(130, "caller cancelled batch Converter; owned process group terminated and leader reaped");
    }
    if (reaped && (!WIFEXITED(status) || WEXITSTATUS(status) != 0)) {
      int root_status = status;
      terminate_group();
      status = root_status;
      break;
    }
    if (reaped && kill(-pid, 0) < 0 && errno == ESRCH) break;
    if (std::chrono::steady_clock::now() - start > std::chrono::seconds(timeout)) {
      expired = true;
      terminate_group();
      break;
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
  }
  return {WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status), expired};
}
std::string sha256(const fs::path &p) {
  std::ifstream in(p, std::ios::binary);
  if (!in)
    throw Error(20, "hash input open failed");
  Sha256 hasher;
  char b[65536];
  while (in) {
    check_cancelled();
    in.read(b, sizeof b);
    if (auto n = in.gcount())
      hasher.update(b, size_t(n));
  }
  if (!in.eof())
    throw Error(20, "hash read failed");
  return hasher.hex();
}
struct Sha256::State {
  CC_SHA256_CTX context;
};
Sha256::Sha256() : state_(new State) { CC_SHA256_Init(&state_->context); }
Sha256::~Sha256() = default;
void Sha256::update(const void *data, size_t length) {
  const auto *bytes = static_cast<const unsigned char *>(data);
  while (length) {
    CC_LONG n = static_cast<CC_LONG>(
        std::min<size_t>(length, std::numeric_limits<CC_LONG>::max()));
    CC_SHA256_Update(&state_->context, bytes, n);
    bytes += n;
    length -= n;
  }
}
std::string Sha256::hex() {
  unsigned char digest[CC_SHA256_DIGEST_LENGTH];
  CC_SHA256_Final(digest, &state_->context);
  std::ostringstream out;
  out << std::hex << std::setfill('0');
  for (auto c : digest)
    out << std::setw(2) << unsigned(c);
  return out.str();
}
} // namespace frame
