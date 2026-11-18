#include "frame.h"
#include <Availability.h>
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
#include <map>
#include <libproc.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
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
  return size_t(limit) - environment - 4096;
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
std::string file_version(const fs::path &path) {
  struct stat info {};
  if (lstat(path.c_str(), &info) != 0 || !S_ISREG(info.st_mode))
    throw Error(26, "cannot read file state: " + path.string());
  std::ostringstream token;
  token << info.st_dev << ',' << info.st_ino << ',' << info.st_size << ','
        << info.st_mtimespec.tv_sec << ',' << info.st_mtimespec.tv_nsec << ','
        << info.st_ctimespec.tv_sec << ',' << info.st_ctimespec.tv_nsec;
  return token.str();
}

namespace {
using Clock = std::chrono::steady_clock;
std::optional<std::set<fs::path>> writable_files(int pid) {
  // A helper process could still own a writer. Use the sealed sequence path
  // whenever this Converter invocation cannot be observed unambiguously.
  pid_t child;
  if (proc_listchildpids(pid, &child, sizeof child) != 0) return {};
  struct proc_fdinfo descriptors[4096];
  int bytes = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, descriptors, sizeof descriptors);
  if (bytes <= 0 || bytes >= int(sizeof descriptors)) return {};
  std::set<fs::path> writers;
  for (size_t i = 0; i < size_t(bytes) / sizeof descriptors[0]; ++i) {
    if (descriptors[i].proc_fdtype != PROX_FDTYPE_VNODE) continue;
    struct vnode_fdinfowithpath info {};
    if (proc_pidfdinfo(pid, descriptors[i].proc_fd, PROC_PIDFDVNODEPATHINFO,
                       &info, sizeof info) != sizeof info)
      return {};
    if (info.pfi.fi_openflags & FWRITE)
      writers.emplace(info.pvip.vip_path);
  }
  return writers;
}
} // namespace

std::vector<Conversion> convert_inputs(const std::vector<fs::path> &inputs,
                         const fs::path &linear_dir,
                         const std::string &converter, std::optional<int> timeout_override,
                         bool temporary, bool sequence,
                         const std::function<void(size_t, const fs::path &)> &ready, bool in_memory) {
  constexpr int seconds_per_image = 15;
  if (!timeout_override && inputs.size() > size_t(std::numeric_limits<int>::max() / seconds_per_image))
    throw Error(24, "too many inputs to represent the Converter timeout");
  const int timeout = timeout_override ? *timeout_override : int(inputs.size()) * seconds_per_image;
  std::vector<std::string> command{converter, "-c", "-l", "-d", linear_dir.string(), "-mp"};
  command[0] = fs::absolute(converter).string();
  const auto command_prefix = command;
  const auto scratch = linear_dir.parent_path();
  const auto log = scratch / "linear-converter.log";
  std::set<std::string> expected;
  fs::path working_directory;
  const auto argv_bytes = [](const auto &args) {
    size_t bytes = 0;
    for (const auto &arg : args) bytes += arg.size() + 1 + sizeof(char *);
    return bytes;
  };
  auto budget = converter_argv_budget();
  bool collisions = false;
  for (const auto &input : inputs) {
    check_cancelled();
    command.push_back(input.string());
    collisions |= !expected.insert(output_collision_key(input)).second;
  }
  if (collisions || argv_bytes(command) > budget) {
    // Adobe accepts explicit relative filenames but does not accept a directory
    // as input. Short uniquely named aliases keep one invocation across volumes.
    working_directory = scratch / "converter-input";
    if (!fs::create_directory(working_directory))
      throw Error(24, "Converter aliases require a fresh working directory");
    command = command_prefix;
    for (size_t i = 0; i < inputs.size(); ++i) {
      check_cancelled();
      auto name = std::to_string(i) + inputs[i].extension().string();
      fs::create_symlink(inputs[i], working_directory / name);
      command.push_back("./" + name);
    }
    if (argv_bytes(command) > budget)
      throw Error(24, "sequence exceeds the OS command-line limit for one Adobe DNG Converter invocation");
  }
  expected.clear();
  std::vector<fs::path> paths;
  for (size_t i = 0; i < inputs.size(); ++i) {
    auto name = fs::path(command[command_prefix.size() + i]).stem().string() + ".dng";
    expected.insert(name);
    paths.push_back(linear_dir / name);
  }
  fs::path invocation;
  if (sequence && !in_memory) {
    invocation = scratch / "converter.argv";
    std::ofstream trace(invocation, std::ios::binary);
    for (const auto &arg : command) trace.write(arg.c_str(), arg.size() + 1);
    trace.close();
    if (!trace) throw Error(24, "could not record the Converter invocation");
  }
  auto conversion_start = std::chrono::steady_clock::now();
  std::map<fs::path, size_t> eligible;
  if (ready)
    for (size_t i = 0; i < paths.size(); ++i)
      eligible.emplace(paths[i], i);
  // Only track timers for outputs whose writer has actually been observed.
  std::map<size_t, std::optional<Clock::time_point>> pending;
  const auto observe = [&](int pid) {
    auto writers = writable_files(pid);
    if (!writers) {
      for (auto &[index, quiet_since] : pending) quiet_since.reset();
      return;
    }
    const auto now = Clock::now();
    for (const auto &path : *writers) {
      if (auto found = eligible.find(path); found != eligible.end())
        pending[found->second].reset();
    }
    for (auto it = pending.begin(); it != pending.end();) {
      const auto index = it->first;
      auto &quiet_since = it->second;
      if (!writers->contains(paths[index])) {
        if (!quiet_since) quiet_since = now;
        if (now - *quiet_since >= std::chrono::seconds(5)) {
          eligible.erase(paths[index]);
          it = pending.erase(it);
          ready(index, paths[index]);
          continue;
        }
      }
      ++it;
    }
  };
  auto result = run_process(command, log, timeout, std::nullopt,
                            ready ? std::function<void(int)>(observe) : std::function<void(int)>{},
                            working_directory, in_memory);
  auto verification_start = std::chrono::steady_clock::now();
  std::string transcript = in_memory ? std::move(result.errors) : read_text(log);
  std::string identity = "Converter [" + inputs.front().string() + " .. " + inputs.back().string() +
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
    throw Error(23, identity + ": Converter output set differs from requested names");
  std::vector<Conversion> outputs;
  const auto version_name = in_memory ? std::string{} : converter_version(command.front());
  for (size_t i = 0; i < inputs.size(); ++i) {
    check_cancelled();
    const auto &output = paths[i];
    const auto version = file_version(output);
    if (in_memory) {
      ConversionInfo info;
      info.used = true;
      outputs.push_back({output, version, std::move(info)});
      continue;
    }
    auto check = inspect(output);
    if (!check || check->photometric != 34892 || !check->width || !check->height)
      throw Error(23, identity + ": output is not nonzero LinearRaw (34892): " + output.string());
    ConversionInfo info;
    info.used = true;
    info.executable = command.front();
    info.version = version_name;
    info.command = command_prefix;
    info.command.push_back(command[command_prefix.size() + i]);
    info.working_directory = working_directory;
    info.invocation = invocation;
    info.exit_code = result.exit_code;
    if (!sequence) info.transcript = transcript;
    info.output_name = output.filename().string();
    info.output_sha256 = sha256(output);
    info.photometric = check->photometric;
    info.directory = linear_dir.string();
    info.cleanup = temporary ? "removed before publishing sidecar"
                             : "caller-owned; retained until caller deletes it";
    if (file_version(output) != version)
      throw Error(23, identity + ": Converter output changed during verification: " + output.string());
    outputs.push_back({output, version, info});
  }
  std::cerr << "converter count=" << inputs.size()
            << " source_first=" << inputs.front() << " source_last=" << inputs.back()
            << " timeout_s=" << timeout
            << " process_s=" << std::chrono::duration<double>(verification_start-conversion_start).count()
            << " output_verification_s=" << std::chrono::duration<double>(std::chrono::steady_clock::now()-verification_start).count() << '\n';
  if (in_memory && !working_directory.empty()) fs::remove_all(working_directory);
  return outputs;
}
ProcessResult run_process(const std::vector<std::string> &args,
                          const fs::path &log, int timeout, std::optional<fs::path> stdout_path,
                          const std::function<void(int)> &observe,
                          const fs::path &working_directory, bool capture, size_t output_limit) {
  check_cancelled();
  std::vector<char *> argv;
  for (auto &a : args)
    argv.push_back(const_cast<char *>(a.c_str()));
  argv.push_back(nullptr);
  posix_spawn_file_actions_t actions;
  posix_spawn_file_actions_init(&actions);
  struct CapturePipe {
    int fds[2]{-1, -1};
    std::string data;
    bool overflow = false;
    ~CapturePipe() { for (int fd : fds) if (fd >= 0) close(fd); }
    void open_pipe() {
      if (pipe(fds) || fcntl(fds[0], F_SETFL, O_NONBLOCK) < 0)
        throw Error(24, "cannot open subprocess capture pipe");
    }
    void redirect(posix_spawn_file_actions_t &actions, int target) const {
      posix_spawn_file_actions_addclose(&actions, fds[0]);
      posix_spawn_file_actions_adddup2(&actions, fds[1], target);
      posix_spawn_file_actions_addclose(&actions, fds[1]);
    }
    void parent_only() {
      close(fds[1]);
      fds[1] = -1;
    }
    void drain(size_t limit, bool tail) {
      if (fds[0] < 0) return;
      char buffer[8192];
      for (size_t pass = 0; pass < 128; ++pass) {
        auto n = read(fds[0], buffer, sizeof buffer);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return;
        if (tail) {
          data.append(buffer, size_t(n));
          if (data.size() > limit) data.erase(0, data.size() - limit);
        } else if (size_t(n) <= limit - data.size()) {
          data.append(buffer, size_t(n));
        } else {
          overflow = true;
        }
      }
    }
  } output, errors;
  if (capture) {
    try {
      if (output_limit) output.open_pipe();
      errors.open_pipe();
    } catch (...) {
      posix_spawn_file_actions_destroy(&actions);
      throw;
    }
    errors.redirect(actions, STDERR_FILENO);
    if (output_limit) output.redirect(actions, STDOUT_FILENO);
    else posix_spawn_file_actions_adddup2(&actions, STDERR_FILENO, STDOUT_FILENO);
  } else {
    posix_spawn_file_actions_addopen(&actions, STDOUT_FILENO, stdout_path.value_or(log).c_str(),
                                     O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (stdout_path)
      posix_spawn_file_actions_addopen(&actions, STDERR_FILENO, log.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0600);
    else
      posix_spawn_file_actions_adddup2(&actions, STDOUT_FILENO, STDERR_FILENO);
  }
  if (!working_directory.empty()) {
    int error;
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 260000
    if (__builtin_available(macOS 26.0, *)) {
      error = posix_spawn_file_actions_addchdir(&actions, working_directory.c_str());
    } else
#endif
    {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
      error = posix_spawn_file_actions_addchdir_np(&actions, working_directory.c_str());
#pragma clang diagnostic pop
    }
    if (error) {
      posix_spawn_file_actions_destroy(&actions);
      throw Error(24, "cannot set the Converter working directory: " + std::to_string(error));
    }
  }
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
  if (capture) {
    errors.parent_only();
    if (output_limit) output.parent_only();
  }
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
    output.drain(output_limit, false);
    errors.drain(8192, true);
    if (output.overflow) {
      terminate_group();
      throw Error(24, "subprocess stdout exceeds the capture limit");
    }
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
      throw Error(130, "caller cancelled Converter; owned process group terminated and leader reaped");
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
    if (!reaped && observe) {
      try {
        observe(pid);
      } catch (...) {
        terminate_group();
        throw;
      }
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
  }
  output.drain(output_limit, false);
  errors.drain(8192, true);
  if (output.overflow) throw Error(24, "subprocess stdout exceeds the capture limit");
  return {WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status), expired,
          std::move(output.data), std::move(errors.data)};
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
